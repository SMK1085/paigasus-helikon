//! Handlers for the `/agents/{name}/runs` resource.
//!
//! A single handler, [`create_run`], serves all three response shapes keyed on
//! the `?stream=` / `?mode=` query:
//!
//! - default — **one-shot**: block until the run reaches a terminal event, then
//!   return the aggregated [`RunResponse`] as JSON.
//! - `?stream=sse` — **Server-Sent Events**: stream every [`AgentEvent`] as it
//!   is produced, replaying any already-emitted events first.
//! - `?mode=async` — **detached**: spawn the run and return `202 Accepted` with
//!   the run id immediately; the run continues independently of the connection.
//!
//! # Execution model
//!
//! Every request — regardless of mode — spawns one **writer task** that drives
//! the agent through the [`Runner`] and drains its event stream into the run's
//! [`EventLog`](crate::event_log::EventLog). The response side merely *subscribes*
//! to that log. This decoupling is what makes a run replayable (one-shot, SSE,
//! and the WebSocket/async-replay transports all read the same log) and is what
//! lets `?mode=async` return before the run finishes.
//!
//! ## Per-session serialization
//!
//! Before the run is created the handler acquires the per-session lock
//! ([`SessionLocks::lock_for`](crate::session::SessionLocks::lock_for)) as an
//! *owned* guard and moves it into the writer task, which holds it for the whole
//! run and releases it at completion. Two requests carrying the same
//! `X-Session-Id` therefore queue: the second blocks on the lock until the
//! first run finishes.
//!
//! ## Cancellation
//!
//! The run's [`CancellationToken`] is cloned into the [`RunContext`]. For the
//! one-shot and SSE modes the response side holds a `DropGuard` over a clone of
//! that token, so a client disconnect cancels the run. The detached `?mode=async`
//! path deliberately attaches no such guard — the run outlives the connection.

use std::{convert::Infallible, sync::Arc, time::Instant};

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header::CONTENT_TYPE, request::Parts, HeaderMap, HeaderValue, StatusCode},
    response::{sse::Event, IntoResponse, Response, Sse},
    Json,
};
use futures_util::StreamExt;
use paigasus_helikon_core::{Agent, AgentEvent, AgentInput, RunConfig, RunContext, Runner};
use serde::Deserialize;
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    dto::{AsyncAccepted, RunRequest, RunResponse},
    error::ServerError,
    event_log::EventLog,
    registry::{RunHandle, RunRegistry},
    server::AppState,
};

/// Upper bound on the request body we will buffer before deserializing (2 MiB).
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Query parameters selecting the response transport.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RunQuery {
    /// `sse` selects the Server-Sent-Events transport.
    #[serde(default)]
    stream: Option<String>,
    /// `async` detaches the run and returns `202 Accepted` immediately.
    #[serde(default)]
    mode: Option<String>,
}

impl RunQuery {
    /// `true` when the caller requested the detached (`?mode=async`) transport.
    fn is_async(&self) -> bool {
        self.mode.as_deref() == Some("async")
    }

    /// `true` when the caller requested the SSE (`?stream=sse`) transport.
    fn is_sse(&self) -> bool {
        self.stream.as_deref() == Some("sse")
    }

    /// Reject unrecognised or mutually-exclusive transport selectors.
    ///
    /// Without this an unknown `?mode=`/`?stream=` value would silently fall
    /// back to one-shot, and `?mode=async&stream=sse` would silently prefer
    /// async — both surprising the caller. Each is surfaced as a 400 instead.
    ///
    /// # Errors
    ///
    /// [`ServerError::BadRequest`] if `mode` is set to anything other than
    /// `async`, if `stream` is set to anything other than `sse`, or if both the
    /// async and SSE transports are requested together.
    fn validate(&self) -> Result<(), ServerError> {
        if let Some(mode) = self.mode.as_deref() {
            if mode != "async" {
                return Err(ServerError::BadRequest(format!(
                    "invalid `mode` selector `{mode}`; the only supported value is `async`"
                )));
            }
        }
        if let Some(stream) = self.stream.as_deref() {
            if stream != "sse" {
                return Err(ServerError::BadRequest(format!(
                    "invalid `stream` selector `{stream}`; the only supported value is `sse`"
                )));
            }
        }
        if self.is_async() && self.is_sse() {
            return Err(ServerError::BadRequest(
                "`mode=async` and `stream=sse` are mutually exclusive".to_owned(),
            ));
        }
        Ok(())
    }
}

/// `POST /agents/{name}/runs` — start a run of the named agent.
///
/// See the [module docs](self) for the execution model and the meaning of the
/// `?stream=` / `?mode=` query parameters.
///
/// # Errors
///
/// - [`ServerError::UnknownAgent`] (404) — no agent with `name` is registered.
/// - [`ServerError::Unauthorized`] (401/403) — the context provider rejected the
///   request's credentials. (The configured [`AuthLayer`](crate::AuthLayer) runs
///   earlier, as router-level middleware.)
/// - [`ServerError::BadRequest`] (400) — an invalid or conflicting `?stream=` /
///   `?mode=` selector, the body was not valid JSON for a [`RunRequest`], or an
///   explicit non-JSON content type was supplied.
/// - [`ServerError::RunStart`] (500) — the run failed before emitting any event
///   (one-shot mode only).
pub(crate) async fn create_run<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
    Path(name): Path<String>,
    Query(query): Query<RunQuery>,
    request: Request,
) -> Result<Response, ServerError> {
    // 0. Reject invalid / conflicting transport selectors (400) before doing
    //    any work.
    query.validate()?;

    // Split into parts + body up front; the body is consumed last. Auth has
    // already run as router-level middleware, so any identity it inserted is
    // present in `parts.extensions` for the context provider below.
    let (parts, body) = request.into_parts();

    // 1. Resolve the agent (404 if unknown).
    let agent = state
        .agents
        .get(&name)
        .cloned()
        .ok_or_else(|| ServerError::UnknownAgent(name.clone()))?;

    // 2. Deserialize the JSON body (400 on a bad body / non-JSON content type).
    let input = read_run_request(&parts, body).await?.into_agent_input();

    // 3. Resolve the session from the optional `X-Session-Id` header.
    let session_id: Option<String> = parts
        .headers
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let session = state.sessions.session(session_id.as_deref()).await?;

    // 4. Acquire the per-session serialization lock BEFORE creating/spawning the
    //    run so that same-session requests queue. The owned guard is moved into
    //    the writer task and released when the run completes.
    let guard: OwnedMutexGuard<()> = state
        .locks
        .lock_for(session_id.as_deref())
        .lock_owned()
        .await;

    // 5. Build the run context, then register the run. Building the context
    //    before registering avoids leaking a never-terminal registry entry if
    //    the context provider fails.
    let cancel = CancellationToken::new();
    let ctx = state.context.build(&parts, session, cancel.clone()).await?;
    let (run_id, handle) = state.registry.create(name, cancel);

    // 6. Spawn the writer task: drive the agent and drain its events into the log.
    spawn_writer(
        Arc::clone(&state.runner),
        agent,
        ctx,
        input,
        state.run_config.clone(),
        Arc::clone(&handle),
        Arc::clone(&state.registry),
        run_id,
        guard,
    );

    // 7. Respond per the requested transport.
    if query.is_async() {
        return Ok(async_response(run_id));
    }
    if query.is_sse() {
        return Ok(sse_response(run_id, &handle));
    }
    oneshot_response(run_id, &handle).await
}

/// Read and validate the JSON request body into a [`RunRequest`].
///
/// Performs a lightweight content-type check: returns 400 on an explicit
/// non-JSON content type, while a missing content type is tolerated and the
/// bytes are parsed optimistically.
async fn read_run_request(parts: &Parts, body: Body) -> Result<RunRequest, ServerError> {
    if let Some(ct) = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        let mime = ct.split(';').next().unwrap_or("").trim();
        let is_json = mime == "application/json"
            || (mime.starts_with("application/") && mime.ends_with("+json"));
        if !is_json {
            return Err(ServerError::BadRequest(format!(
                "unsupported content type `{mime}`; expected application/json"
            )));
        }
    }

    let bytes = axum::body::to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|e| ServerError::BadRequest(format!("failed to read request body: {e}")))?;

    serde_json::from_slice::<RunRequest>(&bytes)
        .map_err(|e| ServerError::BadRequest(format!("invalid run request body: {e}")))
}

/// Drop-guard that records a run's terminal bookkeeping exactly once — on the
/// normal path **and** on a panic unwind of the writer task.
///
/// Both operations are idempotent: [`EventLog::mark_terminal`] just sets a flag,
/// and [`RunRegistry::note_terminal`] only stamps when `terminal_at` is still
/// `None`. Without this guard a panic mid-drain (e.g. a faulty agent stream)
/// would strand every subscriber waiting forever.
struct TerminalGuard {
    log: Arc<EventLog>,
    registry: Arc<RunRegistry>,
    run_id: Uuid,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.log.mark_terminal();
        self.registry.note_terminal(self.run_id, Instant::now());
    }
}

/// Spawn the detached writer task that drives one run to completion.
///
/// Owns every input by value so the task satisfies `'static`. Holds the session
/// lock `guard` for the whole run and drops it (releasing the lock) once the run
/// is terminal and recorded in the registry. Terminal bookkeeping is owned by a
/// [`TerminalGuard`] so it still happens if the agent stream panics mid-drain.
#[allow(clippy::too_many_arguments)]
fn spawn_writer<Ctx: Send + Sync + 'static>(
    runner: Arc<dyn Runner<Ctx>>,
    agent: Arc<dyn Agent<Ctx>>,
    ctx: RunContext<Ctx>,
    input: AgentInput,
    run_config: RunConfig,
    handle: Arc<RunHandle>,
    registry: Arc<RunRegistry>,
    run_id: Uuid,
    guard: OwnedMutexGuard<()>,
) {
    tokio::spawn(async move {
        // Declared FIRST so it drops LAST: terminal bookkeeping (below) runs
        // before the session lock is released, preserving the original ordering.
        let _session_lock = guard;
        // Declared AFTER the lock so it drops FIRST. Its `Drop` marks the log
        // terminal and stamps the registry — even on a panic unwind.
        let _terminal = TerminalGuard {
            log: Arc::clone(&handle.log),
            registry,
            run_id,
        };

        match runner
            .run_streamed(agent.as_ref(), ctx, input, run_config)
            .await
        {
            Ok(streaming) => {
                let mut events = streaming.events;
                while let Some(ev) = events.next().await {
                    handle.log.append(ev);
                }
                // Terminal marking is handled by `_terminal` on scope exit; a
                // real `RunCompleted`/`RunFailed` already set the flag, and the
                // guard's `mark_terminal` is an idempotent safety net otherwise.
            }
            Err(e) => {
                // The run failed to *start* (no events were ever emitted). Record
                // the cause; `_terminal` marks the log terminal so subscribers
                // unblock.
                *handle
                    .start_error
                    .lock()
                    .expect("start_error mutex poisoned") = Some(e.to_string());
            }
        }
    });
}

/// Build the `202 Accepted` body for a detached run.
fn async_response(run_id: Uuid) -> Response {
    (
        StatusCode::ACCEPTED,
        Json(AsyncAccepted {
            run_id: run_id.to_string(),
        }),
    )
        .into_response()
}

/// Build the SSE streaming response.
///
/// The run's cancel `DropGuard` is folded into the stream state so that dropping
/// the response (a client disconnect) cancels the run.
fn sse_response(run_id: Uuid, handle: &Arc<RunHandle>) -> Response {
    let disconnect = handle.cancel.clone().drop_guard();
    let events = handle.log.subscribe(0);

    // Carry both the event stream and the cancel guard through the unfold state.
    // When the SSE response is dropped, the state — and with it the guard — is
    // dropped, cancelling the run.
    let stream = futures_util::stream::unfold(
        (events, disconnect),
        |(mut events, disconnect)| async move {
            let ev = events.next().await?;
            Some((
                Ok::<Event, Infallible>(to_sse_event(&ev)),
                (events, disconnect),
            ))
        },
    );

    let mut response = Sse::new(stream).into_response();
    insert_run_id(response.headers_mut(), run_id);
    response
}

/// Build the one-shot response: subscribe, drain to the terminal event, then
/// aggregate into a [`RunResponse`].
async fn oneshot_response(run_id: Uuid, handle: &Arc<RunHandle>) -> Result<Response, ServerError> {
    // Cancel the run if the client disconnects while we await the result.
    let _disconnect = handle.cancel.clone().drop_guard();

    // NOTE: the event log is capped at `max_events_per_run` events; `output`
    // in the response reflects only the events retained by the ring buffer.
    let events: Vec<AgentEvent> = handle.log.subscribe(0).collect().await;

    // If the run failed to *start*, surface a 500 rather than a 200 envelope.
    if let Some(msg) = handle
        .start_error
        .lock()
        .expect("start_error mutex poisoned")
        .clone()
    {
        return Err(ServerError::RunStart(msg));
    }

    let mut response = Json(RunResponse::from_events(run_id, events)).into_response();
    insert_run_id(response.headers_mut(), run_id);
    Ok(response)
}

/// Serialize an [`AgentEvent`] into an SSE [`Event`], tagging the SSE `event:`
/// field with the event's serde `type` discriminant and carrying the full event
/// JSON as the `data:` payload.
fn to_sse_event(ev: &AgentEvent) -> Event {
    let value = serde_json::to_value(ev).expect("AgentEvent serializes");
    let event = match value.get("type").and_then(serde_json::Value::as_str) {
        Some(tag) => Event::default().event(tag),
        None => Event::default(),
    };
    event
        .json_data(&value)
        .expect("serde_json::Value serializes without error")
}

/// Insert the `X-Run-Id` response header.
fn insert_run_id(headers: &mut HeaderMap, run_id: Uuid) {
    let value =
        HeaderValue::from_str(&run_id.to_string()).expect("uuid is always a valid header value");
    headers.insert("x-run-id", value);
}
