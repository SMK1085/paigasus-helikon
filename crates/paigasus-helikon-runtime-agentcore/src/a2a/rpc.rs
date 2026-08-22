//! `POST /` — A2A's JSON-RPC 2.0 method dispatch.
//!
//! # Errors ride an HTTP 200
//!
//! Per the A2A specification a JSON-RPC error is a *body*, not a status code: every
//! well-formed request answers `200` with either a `result` or an `error`. AWS's
//! platform returns real status codes in front of this container; that is platform
//! behaviour and deliberately not mirrored here.
//!
//! The codes emitted are **A2A-specification** codes (see
//! [`rpc_error`](crate::a2a::types) in this crate's `a2a::types` module). AWS's
//! `-32051`…`-32055` table describes conditions the *platform* reports and is never
//! emitted from inside the container.
//!
//! # A disconnect does not cancel a task
//!
//! Unlike `POST /invocations`, no [`CancellationToken`] drop-guard is bound to any
//! response here. `tasks/resubscribe` exists so a client can reattach to a task after a
//! dropped stream — binding a drop-guard would cancel the task on exactly the disconnect
//! resubscription is meant to survive, so a resubscribing client could only ever find a
//! cancelled task. The detached driver runs to its terminal regardless of who is
//! listening; only `tasks/cancel` produces `canceled`.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse as _, Response, Sse,
    },
    Json,
};
use futures_util::StreamExt as _;
use paigasus_helikon_core::{AgentEvent, AgentInput, CancellationToken, ContentPart, Item};
use paigasus_helikon_runtime_axum::SessionKey;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    a2a::types::{
        now_rfc3339, rpc_error, Artifact, JsonRpcRequest, JsonRpcResponse, MessageSendParams, Part,
        Task, TaskIdParams, TaskKind, TaskState, TaskStatus,
    },
    server::AppState,
    session::extract_session_id,
};

/// Upper bound on the buffered request body (2 MiB), matching `/invocations`.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

// ── Method names ──────────────────────────────────────────────────────────────
//
// Spelled out as constants rather than inline literals because the dispatch
// fallthrough is a silent `-32601`: a typo would present as "that method does not
// exist" rather than as a failure, and no test would notice.

/// Send a message and receive the completed task.
const M_MESSAGE_SEND: &str = "message/send";
/// Send a message and receive an SSE stream of task updates.
const M_MESSAGE_STREAM: &str = "message/stream";
/// Fetch a task by id.
const M_TASKS_GET: &str = "tasks/get";
/// Cancel an in-flight task.
const M_TASKS_CANCEL: &str = "tasks/cancel";
/// Reattach to a task's event stream.
const M_TASKS_RESUBSCRIBE: &str = "tasks/resubscribe";
/// Prefix covering the whole push-notification-config family
/// (`.../set`, `.../get`, `.../list`, `.../delete`).
const M_PUSH_CONFIG_PREFIX: &str = "tasks/pushNotificationConfig/";

/// Both spellings of the authenticated-extended-card method.
///
/// Published A2A sources disagree between `agent/authenticatedExtendedCard` and
/// `agent/getAuthenticatedExtendedCard` across specification revisions. Since an
/// unmatched name falls through to a silent `-32601` — indistinguishable from "this
/// agent does not implement it" — both are matched deliberately so the answer is the
/// intended `-32004` either way, whichever spelling a client uses.
const M_EXTENDED_CARD: [&str; 2] = [
    "agent/authenticatedExtendedCard",
    "agent/getAuthenticatedExtendedCard",
];

// ── Dispatch ──────────────────────────────────────────────────────────────────

/// `POST /` — dispatch one JSON-RPC 2.0 request. See the [module docs](self).
pub(crate) async fn dispatch<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();

    // Validated for its own sake: a malformed session header is a contract violation
    // regardless of which method follows.
    let session_id = match extract_session_id(&parts.headers) {
        Ok(id) => id.map(str::to_owned),
        Err(e) => {
            return rpc_err(Value::Null, rpc_error::INVALID_REQUEST, e.to_string());
        }
    };

    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return rpc_err(
                Value::Null,
                rpc_error::PARSE_ERROR,
                format!("failed to read request body: {e}"),
            );
        }
    };

    let req: JsonRpcRequest = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => {
            return rpc_err(
                Value::Null,
                rpc_error::PARSE_ERROR,
                format!("parse error: {e}"),
            );
        }
    };
    let id = req.id.clone().unwrap_or(Value::Null);

    if req.jsonrpc != "2.0" {
        return rpc_err(
            id,
            rpc_error::INVALID_REQUEST,
            format!(
                "unsupported jsonrpc version {:?}; expected \"2.0\"",
                req.jsonrpc
            ),
        );
    }

    match req.method.as_str() {
        M_MESSAGE_SEND => send(&state, &parts, session_id, id, req.params).await,
        M_TASKS_GET => tasks_get(&state, id, req.params).await,
        M_MESSAGE_STREAM => stream_message(&state, &parts, session_id, id, req.params).await,
        M_TASKS_RESUBSCRIBE => resubscribe(&state, id, req.params).await,
        M_TASKS_CANCEL => tasks_cancel(&state, id, req.params).await,
        m if m.starts_with(M_PUSH_CONFIG_PREFIX) => rpc_err(
            id,
            rpc_error::PUSH_NOTIFICATION_NOT_SUPPORTED,
            "this agent does not support push notifications",
        ),
        m if M_EXTENDED_CARD.contains(&m) => rpc_err(
            id,
            rpc_error::UNSUPPORTED_OPERATION,
            "this agent does not publish an authenticated extended card",
        ),
        other => rpc_err(
            id,
            rpc_error::METHOD_NOT_FOUND,
            format!("method not found: {other}"),
        ),
    }
}

// ── message/send ──────────────────────────────────────────────────────────────

/// `message/send` — run the agent to completion and return the finished task.
async fn send<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    parts: &axum::http::request::Parts,
    session_id: Option<String>,
    id: Value,
    params: Option<Value>,
) -> Response {
    let params = match parse_params::<MessageSendParams>(params) {
        Ok(p) => p,
        Err(msg) => return rpc_err(id, rpc_error::INVALID_PARAMS, msg),
    };

    if params.message.has_non_text_parts() {
        return rpc_err(
            id,
            rpc_error::CONTENT_TYPE_NOT_SUPPORTED,
            "only text parts are supported; file and data parts are not accepted",
        );
    }

    // The platform-issued session header is authoritative over a client-proposed
    // contextId — the same precedence AG-UI mode applies to `threadId`.
    let context_id = session_id
        .clone()
        .or_else(|| params.message.context_id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let task_id = match resolve_task(state, &params.message.task_id, &context_id).await {
        Ok(t) => t,
        Err(resp) => return resp.into_rpc(id),
    };

    let text = params.message.text();
    let handle = match start_run(state, parts, session_id, &task_id, &context_id, text).await {
        Ok(h) => h,
        Err(msg) => return rpc_err(id, rpc_error::INTERNAL_ERROR, msg),
    };

    // Buffered mode: wait for the detached driver to reach the task's terminal.
    if handle.await.is_err() {
        tracing::error!(
            target: "paigasus::runtime_agentcore::a2a",
            task_id = %task_id,
            "a2a run task panicked"
        );
    }

    match state.tasks.get(&task_id).await {
        Ok(Some(task)) => rpc_ok(id, task),
        Ok(None) => rpc_err(
            id,
            rpc_error::INTERNAL_ERROR,
            "the task disappeared while it was running",
        ),
        Err(e) => rpc_err(id, rpc_error::INTERNAL_ERROR, e.to_string()),
    }
}

/// `message/stream` — start a run and stream its task updates as SSE.
///
/// Identical to [`send`] up to the point the driver is spawned; instead of awaiting it,
/// the task's event log is streamed. **No drop-guard is attached** — see the
/// [module docs](self).
async fn stream_message<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    parts: &axum::http::request::Parts,
    session_id: Option<String>,
    id: Value,
    params: Option<Value>,
) -> Response {
    let params = match parse_params::<MessageSendParams>(params) {
        Ok(p) => p,
        Err(msg) => return rpc_err(id, rpc_error::INVALID_PARAMS, msg),
    };

    if params.message.has_non_text_parts() {
        return rpc_err(
            id,
            rpc_error::CONTENT_TYPE_NOT_SUPPORTED,
            "only text parts are supported; file and data parts are not accepted",
        );
    }

    let context_id = session_id
        .clone()
        .or_else(|| params.message.context_id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let task_id = match resolve_task(state, &params.message.task_id, &context_id).await {
        Ok(t) => t,
        Err(failure) => return failure.into_rpc(id),
    };

    let text = params.message.text();
    // The handle is deliberately dropped: the driver is detached and owns the run's
    // lifetime. Nothing here waits for it, and nothing cancels it.
    if let Err(msg) = start_run(state, parts, session_id, &task_id, &context_id, text).await {
        return rpc_err(id, rpc_error::INTERNAL_ERROR, msg);
    }

    // `from = 0` replays the whole log, so subscribing after the driver started cannot
    // miss an event it already appended.
    sse_for_task(state, id, &task_id).await
}

/// `tasks/resubscribe` — reattach to a task's event stream without starting a run.
async fn resubscribe<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    id: Value,
    params: Option<Value>,
) -> Response {
    let params = match parse_params::<TaskIdParams>(params) {
        Ok(p) => p,
        Err(msg) => return rpc_err(id, rpc_error::INVALID_PARAMS, msg),
    };
    sse_for_task(state, id, &params.id).await
}

/// Stream a task's event log as SSE, or answer `-32001` when the task is unknown.
async fn sse_for_task<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    id: Value,
    task_id: &str,
) -> Response {
    match state.tasks.subscribe(task_id, 0).await {
        Ok(events) => Sse::new(events.map(|ev| {
            Ok::<Event, std::convert::Infallible>(Event::default().data(ev.payload.to_string()))
        }))
        .keep_alive(KeepAlive::default())
        .into_response(),
        Err(crate::AgentCoreError::NotFound(_)) => rpc_err(
            id,
            rpc_error::TASK_NOT_FOUND,
            format!("task not found: {task_id}"),
        ),
        Err(e) => rpc_err(id, rpc_error::INTERNAL_ERROR, e.to_string()),
    }
}

/// `tasks/get` — look up a task by id.
async fn tasks_get<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    id: Value,
    params: Option<Value>,
) -> Response {
    let params = match parse_params::<TaskIdParams>(params) {
        Ok(p) => p,
        Err(msg) => return rpc_err(id, rpc_error::INVALID_PARAMS, msg),
    };
    match state.tasks.get(&params.id).await {
        Ok(Some(task)) => rpc_ok(id, task),
        Ok(None) => rpc_err(
            id,
            rpc_error::TASK_NOT_FOUND,
            format!("task not found: {}", params.id),
        ),
        Err(e) => rpc_err(id, rpc_error::INTERNAL_ERROR, e.to_string()),
    }
}

/// `tasks/cancel` — cancel an in-flight task.
///
/// The taxonomy, in order:
///
/// | Condition | Answer |
/// | --- | --- |
/// | unknown task | `-32001` TaskNotFound |
/// | already terminal | `-32002` TaskNotCancelable |
/// | no live token in this container | `-32002`, naming the reason |
/// | the `working` → `canceled` swap is refused | `-32002`, stored state untouched |
/// | otherwise | the task, now `canceled` |
///
/// The last row is the race this method exists to get right: the run's driver may write
/// its terminal state between the terminal check above and the swap below. `update_state`
/// is a compare-and-swap precisely so the loser finds out, and a refused swap here means
/// the run finished first — so the answer is "not cancelable" and the stored state is
/// left exactly as the driver wrote it, never overwritten with `canceled`.
async fn tasks_cancel<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    id: Value,
    params: Option<Value>,
) -> Response {
    let params = match parse_params::<TaskIdParams>(params) {
        Ok(p) => p,
        Err(msg) => return rpc_err(id, rpc_error::INVALID_PARAMS, msg),
    };
    let task_id = params.id;

    let task = match state.tasks.get(&task_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return rpc_err(
                id,
                rpc_error::TASK_NOT_FOUND,
                format!("task not found: {task_id}"),
            );
        }
        Err(e) => return rpc_err(id, rpc_error::INTERNAL_ERROR, e.to_string()),
    };

    if task.status.state.is_terminal() {
        return rpc_err(
            id,
            rpc_error::TASK_NOT_CANCELABLE,
            format!(
                "task {task_id} is already in terminal state {:?}",
                task.status.state
            ),
        );
    }

    if !state.cancels.cancel(&task_id) {
        return rpc_err(
            id,
            rpc_error::TASK_NOT_CANCELABLE,
            format!(
                "task {task_id} has no live run in this container; \
                 with a durable task store it may be running elsewhere"
            ),
        );
    }

    // Try every non-terminal state, not a guessed one. The task's own driver advances it
    // concurrently — `resolve_task` creates it `submitted` and `start_run` registers the
    // cancel token *before* the driver swaps to `working` — so the state read above may
    // already be stale by the time the swap runs. Walking `NON_TERMINAL` also means a
    // state added later cannot be silently skipped here.
    let mut swapped = Ok(false);
    for from in TaskState::NON_TERMINAL {
        swapped = state
            .tasks
            .update_state(&task_id, *from, TaskState::Canceled)
            .await;
        match swapped {
            // Won the swap, or the store itself failed: either way, stop.
            Ok(true) | Err(_) => break,
            // Not in this state; try the next one.
            Ok(false) => {}
        }
    }

    match swapped {
        // Refused from every non-terminal state: the run reached its own terminal
        // first. Leave whatever it wrote alone.
        Ok(false) => rpc_err(
            id,
            rpc_error::TASK_NOT_CANCELABLE,
            format!("task {task_id} reached a terminal state before it could be cancelled"),
        ),
        Ok(true) => match state.tasks.get(&task_id).await {
            Ok(Some(task)) => rpc_ok(id, task),
            Ok(None) => rpc_err(
                id,
                rpc_error::INTERNAL_ERROR,
                "the task disappeared while it was being cancelled",
            ),
            Err(e) => rpc_err(id, rpc_error::INTERNAL_ERROR, e.to_string()),
        },
        Err(e) => rpc_err(id, rpc_error::INTERNAL_ERROR, e.to_string()),
    }
}

// ── Task resolution ───────────────────────────────────────────────────────────

/// A failure that still needs the request's JSON-RPC id to become a response.
struct RpcFailure {
    /// A2A-specification error code.
    code: i32,
    /// Human-readable description.
    message: String,
}

impl RpcFailure {
    /// Attach the request id and render the error response.
    fn into_rpc(self, id: Value) -> Response {
        rpc_err(id, self.code, self.message)
    }
}

/// Resolve the task this request addresses, creating one when no `taskId` was supplied.
///
/// Implements §5.3's inbound-`taskId` table: absent mints a new task, a known
/// non-terminal task is continued, a known terminal task is `-32602` (it cannot take
/// more input), and an unknown id is `-32001`.
async fn resolve_task<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    inbound: &Option<String>,
    context_id: &str,
) -> Result<String, RpcFailure> {
    let Some(existing) = inbound else {
        let task_id = Uuid::new_v4().to_string();
        let task = Task {
            id: task_id.clone(),
            context_id: context_id.to_owned(),
            status: TaskStatus {
                state: TaskState::Submitted,
                timestamp: now_rfc3339(),
            },
            artifacts: vec![],
            kind: TaskKind::Task,
        };
        state.tasks.create(task).await.map_err(|e| RpcFailure {
            code: rpc_error::INTERNAL_ERROR,
            message: e.to_string(),
        })?;
        return Ok(task_id);
    };

    match state.tasks.get(existing).await {
        Ok(Some(task)) if task.status.state.is_terminal() => Err(RpcFailure {
            code: rpc_error::INVALID_PARAMS,
            message: format!(
                "task {existing} is in terminal state {:?} and cannot accept more input",
                task.status.state
            ),
        }),
        Ok(Some(_)) => Ok(existing.clone()),
        Ok(None) => Err(RpcFailure {
            code: rpc_error::TASK_NOT_FOUND,
            message: format!("task not found: {existing}"),
        }),
        Err(e) => Err(RpcFailure {
            code: rpc_error::INTERNAL_ERROR,
            message: e.to_string(),
        }),
    }
}

// ── The run driver ────────────────────────────────────────────────────────────

/// Build the run context and spawn the detached driver for a task.
///
/// Returns the driver's join handle. `message/send` awaits it; `message/stream` does
/// not, streaming the store's event log instead.
///
/// The run is owned by a **detached** task so the runner's finalize step (and with it
/// the session write) always happens, exactly as in `crate::invoke`. What is
/// deliberately *absent* is a drop-guard tying cancellation to a response — see the
/// [module docs](self).
async fn start_run<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    parts: &axum::http::request::Parts,
    session_id: Option<String>,
    task_id: &str,
    context_id: &str,
    text: String,
) -> Result<tokio::task::JoinHandle<()>, String> {
    let session = state
        .sessions
        .session(SessionKey::new(None, session_id.as_deref()))
        .await
        .map_err(|e| e.to_string())?;

    let cancel = CancellationToken::new();
    let ctx = state
        .context
        .build(parts, session, cancel.clone())
        .await
        .map_err(|e| e.to_string())?;

    state.cancels.register(task_id.to_owned(), cancel);

    let input = AgentInput::from_user_text(text);
    let runner = Arc::clone(&state.runner);
    let agent = Arc::clone(&state.agent);
    let run_config = state.run_config.clone();
    let tasks = Arc::clone(&state.tasks);
    let cancels = Arc::clone(&state.cancels);
    let task_id = task_id.to_owned();
    let context_id = context_id.to_owned();

    Ok(tokio::spawn(async move {
        drive(
            runner, agent, tasks, cancels, ctx, input, run_config, task_id, context_id,
        )
        .await;
    }))
}

/// Run the agent, translating its events into the task's A2A event log.
#[allow(clippy::too_many_arguments)]
async fn drive<Ctx: Send + Sync + 'static>(
    runner: Arc<dyn paigasus_helikon_core::Runner<Ctx>>,
    agent: Arc<dyn paigasus_helikon_core::Agent<Ctx>>,
    tasks: Arc<dyn crate::TaskStore>,
    cancels: Arc<crate::a2a::cancel::CancelRegistry>,
    ctx: paigasus_helikon_core::RunContext<Ctx>,
    input: AgentInput,
    run_config: paigasus_helikon_core::RunConfig,
    task_id: String,
    context_id: String,
) {
    // `submitted` -> `working`, and announce it. A task continued via an inbound
    // `taskId` is already `working`, so a refused swap is expected, not an error.
    let _ = tasks
        .update_state(&task_id, TaskState::Submitted, TaskState::Working)
        .await;
    append(
        &tasks,
        &task_id,
        status_update(&task_id, &context_id, TaskState::Working, false),
    )
    .await;

    let mut events = match runner
        .run_streamed(agent.as_ref(), ctx, input, run_config)
        .await
    {
        Ok(streaming) => streaming.events,
        Err(e) => futures_util::stream::iter(vec![AgentEvent::RunFailed {
            error: e.to_string(),
        }])
        .boxed(),
    };

    let mut accumulated = String::new();
    // Mirrors the AG-UI mapper's `streamed_text`: once deltas have carried the text, a
    // trailing `MessageOutput` repeats it and must not be counted twice.
    let mut streamed_text = false;
    let mut outcome = TaskState::Completed;

    while let Some(ev) = events.next().await {
        match &ev {
            AgentEvent::TokenDelta { text } if !text.is_empty() => {
                streamed_text = true;
                accumulated.push_str(text);
                append(
                    &tasks,
                    &task_id,
                    artifact_update(&task_id, &context_id, text, false),
                )
                .await;
            }
            AgentEvent::MessageOutput { item } => {
                if !streamed_text {
                    // Non-streaming provider: the whole message arrives at once, so
                    // synthesize the artifact from it or the client sees nothing.
                    let text = assistant_text(item);
                    if !text.is_empty() {
                        accumulated.push_str(&text);
                        append(
                            &tasks,
                            &task_id,
                            artifact_update(&task_id, &context_id, &text, false),
                        )
                        .await;
                    }
                }
            }
            AgentEvent::RunFailed { error } => {
                outcome = TaskState::Failed;
                tracing::debug!(
                    target: "paigasus::runtime_agentcore::a2a",
                    task_id = %task_id,
                    error = %error,
                    "a2a run failed"
                );
            }
            _ => {}
        }
    }

    if !accumulated.is_empty() {
        let artifacts = vec![Artifact {
            artifact_id: Uuid::new_v4().to_string(),
            name: "agent_response".to_owned(),
            parts: vec![Part::Text { text: accumulated }],
        }];
        if let Err(e) = tasks.set_artifacts(&task_id, artifacts).await {
            tracing::debug!(
                target: "paigasus::runtime_agentcore::a2a",
                task_id = %task_id,
                error = %e,
                "could not store task artifacts"
            );
        }
    }

    // Honour a lost CAS: `tasks/cancel` may already have written `canceled`, and this
    // run must not overwrite it. The final frame reports whatever state actually won.
    let _ = tasks
        .update_state(&task_id, TaskState::Working, outcome)
        .await;
    let final_state = tasks
        .get(&task_id)
        .await
        .ok()
        .flatten()
        .map_or(outcome, |t| t.status.state);

    append(
        &tasks,
        &task_id,
        status_update(&task_id, &context_id, final_state, true),
    )
    .await;

    cancels.remove(&task_id);
}

/// Append one event to the task log, logging rather than propagating a store failure —
/// the run itself has already happened and must not be abandoned over a bookkeeping
/// error.
async fn append(tasks: &Arc<dyn crate::TaskStore>, task_id: &str, payload: Value) {
    if let Err(e) = tasks
        .append_event(task_id, crate::TaskEvent { seq: 0, payload })
        .await
    {
        tracing::debug!(
            target: "paigasus::runtime_agentcore::a2a",
            task_id = %task_id,
            error = %e,
            "could not append task event"
        );
    }
}

/// The assistant text carried by a `MessageOutput` item, if any.
fn assistant_text(item: &Item) -> String {
    match item {
        Item::AssistantMessage { content, .. } => content
            .iter()
            .filter_map(|c| match c {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

/// An A2A `status-update` streaming event.
fn status_update(task_id: &str, context_id: &str, state: TaskState, final_frame: bool) -> Value {
    json!({
        "taskId": task_id,
        "contextId": context_id,
        "kind": "status-update",
        "status": {
            "state": state,
            "timestamp": now_rfc3339(),
        },
        "final": final_frame,
    })
}

/// An A2A `artifact-update` streaming event carrying one text chunk.
fn artifact_update(task_id: &str, context_id: &str, text: &str, last_chunk: bool) -> Value {
    json!({
        "taskId": task_id,
        "contextId": context_id,
        "kind": "artifact-update",
        "artifact": {
            "artifactId": "agent_response",
            "name": "agent_response",
            "parts": [{"kind": "text", "text": text}],
        },
        "append": true,
        "lastChunk": last_chunk,
    })
}

// ── Response helpers ──────────────────────────────────────────────────────────

/// Decode a method's params, turning both "absent" and "wrong shape" into one message.
fn parse_params<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T, String> {
    let params = params.ok_or_else(|| "missing params".to_owned())?;
    serde_json::from_value(params).map_err(|e| format!("invalid params: {e}"))
}

/// A successful JSON-RPC response on HTTP 200.
fn rpc_ok<T: serde::Serialize>(id: Value, result: T) -> Response {
    let value = match serde_json::to_value(result) {
        Ok(v) => v,
        Err(e) => {
            return rpc_err(id, rpc_error::INTERNAL_ERROR, e.to_string());
        }
    };
    Json(JsonRpcResponse::result(id, value)).into_response()
}

/// A JSON-RPC error response on HTTP 200 — see the [module docs](self).
fn rpc_err(id: Value, code: i32, message: impl Into<String>) -> Response {
    Json(JsonRpcResponse::error(id, code, message)).into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use futures_util::stream::{self, BoxStream, StreamExt as _};
    use paigasus_helikon_core::{
        Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
    };
    use tower::ServiceExt as _;

    use crate::{AgentCoreServer, TaskStore as _};

    /// Echoes the user's text back as a single assistant message.
    struct EchoAgent;

    #[async_trait]
    impl Agent<()> for EchoAgent {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "test-only echo agent"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            let text = input
                .messages
                .iter()
                .filter_map(|i| match i {
                    Item::UserMessage { content } => Some(
                        content
                            .iter()
                            .filter_map(|c| match c {
                                ContentPart::Text { text } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .next_back()
                .unwrap_or_default();
            Ok(stream::iter(vec![
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage {
                        content: vec![ContentPart::Text { text }],
                        agent: Some("echo".to_owned()),
                    },
                },
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ])
            .boxed())
        }
    }

    fn server() -> AgentCoreServer<()> {
        AgentCoreServer::builder()
            .agent(Arc::new(EchoAgent))
            .with_default_context()
            .build()
            .expect("server builds")
    }

    /// POST one JSON-RPC body at the A2A router and return the parsed response.
    async fn post_rpc(server: &AgentCoreServer<()>, body: &str) -> serde_json::Value {
        let resp = server
            .a2a_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    const SEND_HI: &str = r#"{"jsonrpc":"2.0","id":"req-001","method":"message/send",
        "params":{"message":{"role":"user","parts":[{"kind":"text","text":"hi"}],
        "messageId":"m1"}}}"#;

    #[tokio::test]
    async fn message_send_returns_a_completed_task_with_artifacts() {
        let v = post_rpc(&server(), SEND_HI).await;
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], "req-001");
        assert_eq!(v["result"]["kind"], "task");
        assert_eq!(v["result"]["status"]["state"], "completed");
        assert!(v["result"]["artifacts"][0]["parts"][0]["text"].is_string());
        assert!(v["result"]["id"].is_string());
        assert!(v["result"]["contextId"].is_string());
    }

    #[tokio::test]
    async fn tasks_get_returns_a_task_created_by_message_send() {
        let s = server();
        let sent = post_rpc(&s, SEND_HI).await;
        let id = sent["result"]["id"].as_str().unwrap();
        let got = post_rpc(
            &s,
            &format!(r#"{{"jsonrpc":"2.0","id":2,"method":"tasks/get","params":{{"id":"{id}"}}}}"#),
        )
        .await;
        assert_eq!(got["result"]["id"], id);
        assert_eq!(
            got["result"]["artifacts"][0]["parts"][0]["text"], "hi",
            "a later get must report the same artifacts the send returned"
        );
    }

    #[tokio::test]
    async fn tasks_get_on_an_unknown_id_is_task_not_found() {
        let v = post_rpc(
            &server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"tasks/get","params":{"id":"nope"}}"#,
        )
        .await;
        assert_eq!(v["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn an_unknown_method_is_method_not_found() {
        let v = post_rpc(
            &server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"does/notExist"}"#,
        )
        .await;
        assert_eq!(v["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn a_malformed_body_is_a_parse_error() {
        let v = post_rpc(&server(), "not json").await;
        assert_eq!(v["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn a_non_two_point_zero_envelope_is_an_invalid_request() {
        let v = post_rpc(
            &server(),
            r#"{"jsonrpc":"1.0","id":1,"method":"message/send"}"#,
        )
        .await;
        assert_eq!(v["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn a_non_text_part_is_content_type_not_supported() {
        let v = post_rpc(
            &server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"message/send",
            "params":{"message":{"role":"user",
            "parts":[{"kind":"file","file":{"uri":"s3://x"}}],"messageId":"m"}}}"#,
        )
        .await;
        assert_eq!(v["error"]["code"], -32005);
    }

    #[tokio::test]
    async fn push_notification_and_extended_card_methods_answer_explicitly() {
        let v = post_rpc(
            &server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"tasks/pushNotificationConfig/set"}"#,
        )
        .await;
        assert_eq!(v["error"]["code"], -32003);
        let v = post_rpc(
            &server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/authenticatedExtendedCard"}"#,
        )
        .await;
        assert_eq!(v["error"]["code"], -32004);
        // Both published spellings must land on the same deliberate answer, since an
        // unmatched name would fall through to an indistinguishable -32601.
        let v = post_rpc(
            &server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/getAuthenticatedExtendedCard"}"#,
        )
        .await;
        assert_eq!(v["error"]["code"], -32004);
    }

    /// A JSON-RPC error rides an HTTP 200, per the A2A specification. AWS's platform
    /// returns real status codes instead; that is platform behaviour, not ours.
    #[tokio::test]
    async fn json_rpc_errors_ride_an_http_200() {
        let resp = server()
            .a2a_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Streams a token delta, pauses, then completes — long enough that a test can drop
    /// the response while the run is still in flight.
    struct SlowAgent;

    #[async_trait]
    impl Agent<()> for SlowAgent {
        fn name(&self) -> &str {
            "slow"
        }
        fn description(&self) -> &str {
            "test-only agent that pauses mid-run"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            Ok(stream::once(async {
                AgentEvent::TokenDelta {
                    text: "partial".to_owned(),
                }
            })
            .chain(stream::once(async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                }
            }))
            .boxed())
        }
    }

    /// POST a JSON-RPC body and return the raw SSE body text.
    async fn post_sse(server: &AgentCoreServer<()>, body: &str) -> String {
        let resp = server
            .a2a_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// Parse every `data:` line of an SSE body into JSON.
    fn sse_frames(body: &str) -> Vec<serde_json::Value> {
        body.lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .map(|d| serde_json::from_str(d).expect("each data: frame is JSON"))
            .collect()
    }

    #[tokio::test]
    async fn message_stream_emits_status_then_artifact_then_final_status() {
        let body = post_sse(
            &server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"message/stream",
            "params":{"message":{"role":"user","parts":[{"kind":"text","text":"hi"}],
            "messageId":"m1"}}}"#,
        )
        .await;
        let frames = sse_frames(&body);
        assert!(!frames.is_empty(), "body: {body}");

        assert_eq!(frames[0]["kind"], "status-update");
        assert_eq!(frames[0]["status"]["state"], "working");
        assert_eq!(frames[0]["final"], false);

        assert!(
            frames.iter().any(|f| f["kind"] == "artifact-update"),
            "expected an artifact-update, got {frames:?}"
        );

        let last = frames.last().unwrap();
        assert_eq!(last["kind"], "status-update");
        assert_eq!(last["status"]["state"], "completed");
        assert_eq!(last["final"], true);
    }

    #[tokio::test]
    async fn resubscribe_replays_a_completed_task_from_the_start() {
        let s = server();
        let sent = post_rpc(&s, SEND_HI).await;
        let id = sent["result"]["id"].as_str().unwrap();

        let body = post_sse(
            &s,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"tasks/resubscribe","params":{{"id":"{id}"}}}}"#
            ),
        )
        .await;
        let frames = sse_frames(&body);
        assert!(!frames.is_empty(), "a completed task replays its log");
        let last = frames.last().unwrap();
        assert_eq!(last["kind"], "status-update");
        assert_eq!(last["final"], true, "the replayed stream terminates");
    }

    #[tokio::test]
    async fn resubscribe_on_an_unknown_task_is_task_not_found() {
        let v = post_rpc(
            &server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"tasks/resubscribe","params":{"id":"ghost"}}"#,
        )
        .await;
        assert_eq!(v["error"]["code"], -32001);
    }

    /// Regression for the disconnect semantics: dropping a `message/stream` response
    /// must leave the task reachable and NOT cancelled, or `tasks/resubscribe` could
    /// only ever find cancelled tasks.
    #[tokio::test]
    async fn dropping_a_stream_leaves_the_task_resubscribable() {
        let store = Arc::new(crate::InMemoryTaskStore::new(8));
        store
            .create(crate::Task {
                id: "streamed".to_owned(),
                context_id: "ctx".to_owned(),
                status: crate::TaskStatus {
                    state: crate::TaskState::Working,
                    timestamp: "2026-08-08T00:00:00Z".to_owned(),
                },
                artifacts: vec![],
                kind: crate::TaskKind::Task,
            })
            .await
            .unwrap();
        let s = AgentCoreServer::builder()
            .agent(Arc::new(SlowAgent))
            .with_default_context()
            .task_store(Arc::clone(&store) as Arc<dyn crate::TaskStore>)
            .build()
            .expect("server builds");

        // Drop the response without reading its body — a client disconnecting mid-stream.
        let resp = s
            .a2a_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"message/stream",
                        "params":{"message":{"role":"user",
                        "parts":[{"kind":"text","text":"hi"}],
                        "messageId":"m","taskId":"streamed"}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        drop(resp);

        // The detached driver must still reach its terminal.
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let state = store.get("streamed").await.unwrap().unwrap().status.state;
            assert_ne!(
                state,
                crate::TaskState::Canceled,
                "a disconnect must never cancel an A2A task"
            );
            if state == crate::TaskState::Completed {
                return;
            }
        }
        panic!("the detached driver never completed the task after the client disconnected");
    }

    /// The happy path: a live run, cancelled through a task id known up front.
    /// The id has to be seeded rather than minted, because `message/stream`
    /// returns an SSE body and never reports the task id in its headers.
    #[tokio::test]
    async fn cancelling_a_live_task_reports_canceled() {
        let store = Arc::new(crate::InMemoryTaskStore::new(8));
        store
            .create(crate::Task {
                id: "live".to_owned(),
                context_id: "ctx".to_owned(),
                status: crate::TaskStatus {
                    state: crate::TaskState::Working,
                    timestamp: "2026-08-08T00:00:00Z".to_owned(),
                },
                artifacts: vec![],
                kind: crate::TaskKind::Task,
            })
            .await
            .unwrap();
        let s = AgentCoreServer::builder()
            .agent(Arc::new(SlowAgent))
            .with_default_context()
            .task_store(Arc::clone(&store) as Arc<dyn crate::TaskStore>)
            .build()
            .expect("server builds");

        let resp = s
            .a2a_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"message/stream",
                        "params":{"message":{"role":"user",
                        "parts":[{"kind":"text","text":"hi"}],
                        "messageId":"m","taskId":"live"}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        drop(resp);

        let v = post_rpc(
            &s,
            r#"{"jsonrpc":"2.0","id":2,"method":"tasks/cancel","params":{"id":"live"}}"#,
        )
        .await;
        assert_eq!(
            v["result"]["status"]["state"], "canceled",
            "a live task must cancel: {v}"
        );
    }

    /// Regression (CodeRabbit, PR #186): `resolve_task` creates a task `submitted` and
    /// `start_run` registers its cancel token *before* the driver swaps it to `working`.
    /// A cancel landing in that window used to fire the token, fail the
    /// `working -> canceled` swap, and answer "-32002 reached a terminal state" about a
    /// task that was not terminal — leaving it anything but `canceled`.
    #[tokio::test]
    async fn cancelling_a_task_still_in_submitted_reports_canceled() {
        let store = Arc::new(crate::InMemoryTaskStore::new(8));
        store
            .create(crate::Task {
                id: "pending".to_owned(),
                context_id: "ctx".to_owned(),
                status: crate::TaskStatus {
                    state: crate::TaskState::Submitted,
                    timestamp: "2026-08-08T00:00:00Z".to_owned(),
                },
                artifacts: vec![],
                kind: crate::TaskKind::Task,
            })
            .await
            .unwrap();
        let s = AgentCoreServer::builder()
            .agent(Arc::new(SlowAgent))
            .with_default_context()
            .task_store(Arc::clone(&store) as Arc<dyn crate::TaskStore>)
            .build()
            .expect("server builds");

        // Register a live token for the task without letting a driver advance it past
        // `submitted` — exactly the window between registration and the first swap.
        s.state_for_a2a().cancels.register(
            "pending".to_owned(),
            paigasus_helikon_core::CancellationToken::new(),
        );

        let v = post_rpc(
            &s,
            r#"{"jsonrpc":"2.0","id":1,"method":"tasks/cancel","params":{"id":"pending"}}"#,
        )
        .await;
        assert_eq!(
            v["result"]["status"]["state"], "canceled",
            "a submitted task with a live run must cancel, not report a false error: {v}"
        );
        assert_eq!(
            store.get("pending").await.unwrap().unwrap().status.state,
            crate::TaskState::Canceled,
            "the stored state must reflect the cancellation"
        );
    }

    /// Regression (CodeRabbit round 2, PR #186): `input-required` is non-terminal too.
    /// The round-1 fix added `submitted` but stopped there, so a task in this state
    /// passed the terminal check, had its token fired, failed both swaps, and was left
    /// uncancelled behind a `-32002`. This runtime never produces `input-required`, but
    /// a custom `TaskStore` that models an interrupt seam does — which is the whole
    /// reason the variant exists.
    #[tokio::test]
    async fn cancelling_an_input_required_task_reports_canceled() {
        let store = Arc::new(crate::InMemoryTaskStore::new(8));
        store
            .create(crate::Task {
                id: "waiting".to_owned(),
                context_id: "ctx".to_owned(),
                status: crate::TaskStatus {
                    state: crate::TaskState::InputRequired,
                    timestamp: "2026-08-08T00:00:00Z".to_owned(),
                },
                artifacts: vec![],
                kind: crate::TaskKind::Task,
            })
            .await
            .unwrap();
        let s = AgentCoreServer::builder()
            .agent(Arc::new(SlowAgent))
            .with_default_context()
            .task_store(Arc::clone(&store) as Arc<dyn crate::TaskStore>)
            .build()
            .expect("server builds");

        s.state_for_a2a().cancels.register(
            "waiting".to_owned(),
            paigasus_helikon_core::CancellationToken::new(),
        );

        let v = post_rpc(
            &s,
            r#"{"jsonrpc":"2.0","id":1,"method":"tasks/cancel","params":{"id":"waiting"}}"#,
        )
        .await;
        assert_eq!(
            v["result"]["status"]["state"], "canceled",
            "every non-terminal state must be cancellable: {v}"
        );
        assert_eq!(
            store.get("waiting").await.unwrap().unwrap().status.state,
            crate::TaskState::Canceled
        );
    }

    #[tokio::test]
    async fn cancelling_an_unknown_task_is_task_not_found() {
        let v = post_rpc(
            &server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"tasks/cancel","params":{"id":"ghost"}}"#,
        )
        .await;
        assert_eq!(v["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn cancelling_a_terminal_task_is_not_cancelable() {
        let s = server();
        let sent = post_rpc(&s, SEND_HI).await;
        let id = sent["result"]["id"].as_str().unwrap();
        let v = post_rpc(
            &s,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"tasks/cancel","params":{{"id":"{id}"}}}}"#
            ),
        )
        .await;
        assert_eq!(v["error"]["code"], -32002);
    }

    /// Regression for the CAS race (§5.7): a cancel that loses to a completed run must
    /// report -32002 AND leave the stored state `completed` — never overwrite it.
    #[tokio::test]
    async fn a_cancel_losing_the_race_leaves_the_task_completed() {
        let s = server();
        let sent = post_rpc(&s, SEND_HI).await;
        let id = sent["result"]["id"].as_str().unwrap().to_owned();

        let cancelled = post_rpc(
            &s,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"tasks/cancel","params":{{"id":"{id}"}}}}"#
            ),
        )
        .await;
        assert_eq!(cancelled["error"]["code"], -32002);

        let got = post_rpc(
            &s,
            &format!(r#"{{"jsonrpc":"2.0","id":3,"method":"tasks/get","params":{{"id":"{id}"}}}}"#),
        )
        .await;
        assert_eq!(
            got["result"]["status"]["state"], "completed",
            "a losing cancel must not overwrite the run's own terminal state"
        );
    }

    /// A task in the store with no live token (a durable store, another microVM) is not
    /// cancellable from here, and must say so rather than silently succeed.
    #[tokio::test]
    async fn a_task_with_no_live_token_is_not_cancelable() {
        let s = server_with_working_task("orphan").await;
        let v = post_rpc(
            &s,
            r#"{"jsonrpc":"2.0","id":1,"method":"tasks/cancel","params":{"id":"orphan"}}"#,
        )
        .await;
        assert_eq!(v["error"]["code"], -32002);
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("no live run"),
            "the reason must be stated, not implied: {v}"
        );
    }

    /// Seed a non-terminal task directly so an inbound `taskId` has something live to
    /// continue — `message/send` always completes its own task, so a second send
    /// against it would legitimately be refused as terminal.
    async fn server_with_working_task(task_id: &str) -> AgentCoreServer<()> {
        let store = Arc::new(crate::InMemoryTaskStore::new(8));
        store
            .create(crate::Task {
                id: task_id.to_owned(),
                context_id: "seeded-ctx".to_owned(),
                status: crate::TaskStatus {
                    state: crate::TaskState::Working,
                    timestamp: "2026-08-08T00:00:00Z".to_owned(),
                },
                artifacts: vec![],
                kind: crate::TaskKind::Task,
            })
            .await
            .unwrap();
        AgentCoreServer::builder()
            .agent(Arc::new(EchoAgent))
            .with_default_context()
            .task_store(store)
            .build()
            .expect("server builds")
    }

    #[tokio::test]
    async fn an_inbound_task_id_continues_an_existing_task() {
        let s = server_with_working_task("live-task").await;
        let v = post_rpc(
            &s,
            r#"{"jsonrpc":"2.0","id":1,"method":"message/send",
            "params":{"message":{"role":"user","parts":[{"kind":"text","text":"more"}],
            "messageId":"m2","taskId":"live-task"}}}"#,
        )
        .await;
        assert_eq!(
            v["result"]["id"], "live-task",
            "the run must continue the named task, not mint a new one"
        );
    }

    #[tokio::test]
    async fn an_inbound_task_id_for_a_terminal_task_is_invalid_params() {
        let s = server();
        let sent = post_rpc(&s, SEND_HI).await;
        let id = sent["result"]["id"].as_str().unwrap();
        assert_eq!(sent["result"]["status"]["state"], "completed");
        let v = post_rpc(
            &s,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"message/send",
                "params":{{"message":{{"role":"user","parts":[{{"kind":"text","text":"again"}}],
                "messageId":"m2","taskId":"{id}"}}}}}}"#
            ),
        )
        .await;
        assert_eq!(v["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn an_unknown_inbound_task_id_is_task_not_found() {
        let v = post_rpc(
            &server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"message/send",
            "params":{"message":{"role":"user","parts":[{"kind":"text","text":"hi"}],
            "messageId":"m","taskId":"ghost"}}}"#,
        )
        .await;
        assert_eq!(v["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn the_session_header_wins_over_an_inbound_context_id() {
        let session = "a-session-id-that-is-long-enough-to-pass-validation-000";
        let resp = server()
            .a2a_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .header("X-Amzn-Bedrock-AgentCore-Runtime-Session-Id", session)
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"message/send",
                        "params":{"message":{"role":"user",
                        "parts":[{"kind":"text","text":"hi"}],
                        "messageId":"m","contextId":"client-proposed"}}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            v["result"]["contextId"], session,
            "the platform header is authoritative over a client-supplied contextId"
        );
    }
}
