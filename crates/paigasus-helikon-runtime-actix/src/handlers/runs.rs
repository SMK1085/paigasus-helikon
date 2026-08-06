//! Handler for the `/agents/{name}/runs` resource.
//!
//! A single handler, [`create_run`], serves all three response shapes keyed on
//! the `?stream=` / `?mode=` query:
//!
//! - default — **one-shot**: block until the run reaches a terminal event, then
//!   return the aggregated [`RunResponse`] as JSON.
//! - `?stream=sse` — **Server-Sent Events**: stream every [`AgentEvent`] as it
//!   is produced, replaying any already-emitted events first. Framed by hand
//!   (`HttpResponse::streaming`) rather than via `actix-web-lab`, byte-matching
//!   the axum runtime's `text/event-stream` layout.
//! - `?mode=async` — **detached**: spawn the run and return `202 Accepted` with
//!   the run id immediately; the run continues independently of the connection.
//!
//! # Execution model
//!
//! Every request spawns one **writer task** that drives the agent through the
//! [`Runner`] and drains its event stream into the run's
//! [`EventLog`](crate::event_log::EventLog). The response side merely
//! *subscribes* to that log.
//!
//! Unlike the axum runtime — where the writer task runs on the caller's tokio
//! runtime — actix-web gives each worker its own single-threaded `actix-rt`
//! runtime. A run must outlive the request's worker and be reachable from any
//! worker, so the writer is spawned on the **process-wide** runtime
//! ([`crate::runtime::shared_handle`]), NOT on the worker via `tokio::spawn`.
//! The writer future holds only `Send` values (`Arc<dyn Runner>`,
//! `Arc<dyn Agent>`, the `Send` [`RunContext`], [`AgentInput`],
//! `Arc<RunHandle>`, `OwnedMutexGuard`), so `Handle::spawn` accepts it. The
//! `!Send` [`HttpRequest`] is consumed on the worker (to build the context) and
//! is never moved into the writer.
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
//! The run's [`CancellationToken`] is cloned into the [`RunContext`]. The
//! one-shot and SSE response sides each hold a `DropGuard` over a clone of that
//! token, so if the response future is dropped the run is cancelled. The
//! detached `?mode=async` path deliberately attaches no such guard — the run
//! outlives the connection.
//!
//! These guards differ in when they actually fire on a **client disconnect**,
//! and the two transports are asymmetric under actix-web. A `.streaming()` body
//! future (SSE) *is* dropped when the peer goes away, so the SSE `DropGuard`
//! fires and cancels the run mid-flight. A buffered one-shot handler future,
//! by contrast, is *not* dropped on disconnect (actix drives it to completion),
//! so its `DropGuard` fires only after the run finishes — effectively a no-op
//! for cancellation. This mirrors actix's body-vs-buffered semantics rather
//! than the axum runtime, where both cancel on disconnect.

use std::{convert::Infallible, sync::Arc, time::Instant};

use actix_web::{
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE},
        StatusCode,
    },
    web::{self, Data},
    HttpRequest, HttpResponse,
};
use futures_util::StreamExt;
use paigasus_helikon_core::{Agent, AgentEvent, AgentInput, RunConfig, RunContext, Runner};
use serde::Deserialize;
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::{CancellationToken, DropGuard};
use uuid::Uuid;

use crate::{
    auth::Principal,
    dto::{AsyncAccepted, RunRequest, RunResponse},
    error::{AuthRejection, ServerError},
    event_log::{is_terminal, EventLog},
    registry::{RunHandle, RunRegistry},
    server::AppState,
    session::SessionKey,
};

/// Upper bound on the request body we will buffer before deserializing (2 MiB).
///
/// Read manually from the [`web::Payload`] stream rather than via `web::Json` /
/// `web::Bytes`, whose default 256 KiB `PayloadConfig` limit would reject larger
/// bodies with an actix error envelope instead of our [`ServerError`] JSON.
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
///   request's credentials, or the request carried `X-Session-Id` with no
///   established [`Principal`](crate::Principal) while the principal gate is on
///   (403).
/// - [`ServerError::BadRequest`] (400) — an invalid or conflicting `?stream=` /
///   `?mode=` selector, the body was not valid JSON for a [`RunRequest`], an
///   explicit non-JSON content type was supplied, or the body exceeded the
///   [`MAX_BODY_BYTES`] cap.
/// - [`ServerError::RunStart`] (500) — the run failed before emitting any event
///   (one-shot mode only).
pub(crate) async fn create_run<Ctx: Send + Sync + 'static>(
    state: Data<AppState<Ctx>>,
    path: web::Path<String>,
    query: web::Query<RunQuery>,
    req: HttpRequest,
    body: web::Payload,
) -> Result<HttpResponse, ServerError> {
    // 0. Reject invalid / conflicting transport selectors (400) before any work.
    query.validate()?;

    let name = path.into_inner();

    // 1. Resolve the agent (404 if unknown).
    let agent = state
        .agents
        .get(&name)
        .cloned()
        .ok_or_else(|| ServerError::UnknownAgent(name.clone()))?;

    // 2. Deserialize the JSON body (400 on a bad body / non-JSON content type /
    //    oversize).
    let input = read_run_request(&req, body).await?.into_agent_input();

    // 3. Resolve the principal the auth layer established, and the session id.
    //
    //    The `Ref` from `extensions()` MUST be dropped before any `.await`.
    //    actix request extensions are `RefCell`-backed and handler futures carry
    //    no `Send` bound, so holding it across an await compiles and then panics
    //    with "already mutably borrowed" the first time a `ContextProvider` or
    //    `AuthLayer` calls `extensions_mut()`.
    let principal: Option<String> = {
        use actix_web::HttpMessage as _;
        req.extensions().get::<Principal>().map(|p| p.0.clone())
    };

    // A present-but-non-UTF-8 header is a 400 rather than a silent `None`:
    // coercing it to `None` would skip the fail-closed gate below.
    let session_id: Option<String> = match req.headers().get("x-session-id") {
        None => None,
        Some(value) => Some(
            value
                .to_str()
                .map_err(|_| {
                    ServerError::BadRequest(
                        "invalid `X-Session-Id` header: not valid UTF-8".to_owned(),
                    )
                })?
                .to_owned(),
        ),
    };

    // Fail closed: a named session with no principal would join the shared
    // principal-less namespace, which is exactly the IDOR this gate prevents.
    if state.require_principal && principal.is_none() && session_id.is_some() {
        return Err(ServerError::Unauthorized(AuthRejection {
            status: StatusCode::FORBIDDEN,
            message: "session id requires an authenticated principal".to_owned(),
        }));
    }

    let key = SessionKey::new(principal.as_deref(), session_id.as_deref());
    let session = state.sessions.session(key).await?;

    // 4. Acquire the per-session serialization lock BEFORE creating/spawning the
    //    run so that same-session requests queue. `SessionKey` is `Copy`, so the
    //    same value feeds both calls without a clone. The owned guard is moved
    //    into the writer task and released when the run completes.
    let guard: OwnedMutexGuard<()> = state.locks.lock_for(key).lock_owned().await;

    // 5. Build the run context, then register the run. Building the context
    //    before registering avoids leaking a never-terminal registry entry if
    //    the context provider fails. `build` borrows the `!Send` `HttpRequest`,
    //    so it is awaited here on the worker; the resulting `RunContext` is
    //    `Send` and is what moves into the writer task.
    let cancel = CancellationToken::new();
    let ctx = state.context.build(&req, session, cancel.clone()).await?;
    let (run_id, handle) = state.registry.create(name, cancel);

    // 6. Spawn the writer task on the process-wide runtime: drive the agent and
    //    drain its events into the log.
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
/// Reads the [`web::Payload`] stream manually, capping at [`MAX_BODY_BYTES`].
/// Performs a lightweight content-type check: returns 400 on an explicit
/// non-JSON content type, while a missing content type is tolerated and the
/// bytes are parsed optimistically.
async fn read_run_request(
    req: &HttpRequest,
    mut body: web::Payload,
) -> Result<RunRequest, ServerError> {
    if let Some(ct) = req
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        // Media types are case-insensitive (RFC 9110 §8.3.1), so `Application/JSON`
        // must be accepted exactly like `application/json`.
        let mime = ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let is_json = mime == "application/json"
            || (mime.starts_with("application/") && mime.ends_with("+json"));
        if !is_json {
            return Err(ServerError::BadRequest(format!(
                "unsupported content type `{mime}`; expected application/json"
            )));
        }
    }

    let mut bytes = web::BytesMut::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk
            .map_err(|e| ServerError::BadRequest(format!("failed to read request body: {e}")))?;
        if bytes.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(ServerError::BadRequest("request body too large".into()));
        }
        bytes.extend_from_slice(&chunk);
    }

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
/// Owns every input by value so the task satisfies `'static`, and is spawned on
/// the process-wide runtime ([`crate::runtime::shared_handle`]) rather than the
/// per-worker `actix-rt` runtime. Holds the session lock `guard` for the whole
/// run and drops it (releasing the lock) once the run is terminal and recorded
/// in the registry. Terminal bookkeeping is owned by a [`TerminalGuard`] so it
/// still happens if the agent stream panics mid-drain.
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
    crate::runtime::shared_handle().spawn(async move {
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
                // The run failed to *start* (no events were ever emitted). Log the
                // detailed cause exactly once — the wire-facing frame and the 500
                // body are both redacted, so this is the only place it survives.
                tracing::error!(
                    agent = %handle.agent_name,
                    %run_id,
                    error = %e,
                    "run failed to start"
                );
                *handle
                    .start_error
                    .lock()
                    .expect("start_error mutex poisoned") = Some(e.to_string());
            }
        }
    });
}

/// Build the one-shot response: subscribe, drain to the terminal event, then
/// aggregate into a [`RunResponse`].
async fn oneshot_response(
    run_id: Uuid,
    handle: &Arc<RunHandle>,
) -> Result<HttpResponse, ServerError> {
    // Cancel the run if the handler future is dropped while we await the result.
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

    Ok(HttpResponse::Ok()
        .insert_header(("x-run-id", run_id.to_string()))
        .json(RunResponse::from_events(run_id, events)))
}

/// Build the `202 Accepted` body for a detached run.
fn async_response(run_id: Uuid) -> HttpResponse {
    HttpResponse::Accepted().json(AsyncAccepted {
        run_id: run_id.to_string(),
    })
}

/// Unfold state for the SSE response stream: the live event stream, the cancel
/// drop-guard (held for the stream's whole lifetime so a client disconnect
/// cancels the run), a clone of the run handle (to synthesize a terminal frame
/// on a terminal-less close), and the `saw_terminal` / `done` flags.
struct SseState<S> {
    events: S,
    // Held only for its `Drop` side effect (cancels the run when the SSE
    // response body — and with it this state — is dropped); never read directly.
    // Unlike the one-shot handler future, actix DOES drop a `.streaming()` body
    // future on client disconnect, so this guard genuinely fires mid-run.
    #[allow(dead_code)]
    disconnect: DropGuard,
    handle: Arc<RunHandle>,
    saw_terminal: bool,
    done: bool,
}

/// Build the hand-rolled SSE streaming response.
///
/// Streams `text/event-stream` frames straight over
/// [`HttpResponse::streaming`](actix_web::HttpResponseBuilder::streaming) — no
/// `actix-web-lab` — folding each [`AgentEvent`] into a [`sse_frame`] whose byte
/// layout matches the axum runtime's `to_sse_event`. The run's cancel
/// `DropGuard` is folded into the stream state so that dropping the response (a
/// client disconnect) cancels the run. If the live event stream ends without
/// delivering a real terminal event (a start-error, or a stream that
/// ended/panicked mid-run), exactly one synthetic `run_failed` frame is appended
/// before the SSE stream closes — see [`RunHandle::synthetic_terminal_frame`].
fn sse_response(run_id: Uuid, handle: &Arc<RunHandle>) -> HttpResponse {
    let disconnect = handle.cancel.clone().drop_guard();
    let events = handle.log.subscribe(0);
    let handle = Arc::clone(handle);

    let byte_stream = futures_util::stream::unfold(
        SseState {
            events,
            disconnect,
            handle,
            saw_terminal: false,
            done: false,
        },
        |mut state| async move {
            if state.done {
                return None;
            }
            match state.events.next().await {
                Some(ev) => {
                    state.saw_terminal |= is_terminal(&ev);
                    let frame = sse_frame(&ev);
                    Some((Ok::<web::Bytes, Infallible>(frame), state))
                }
                None => {
                    // Live stream ended. If no real terminal was delivered, emit
                    // exactly one synthetic `run_failed` frame, then finish.
                    let synthetic = state.handle.synthetic_terminal_frame(state.saw_terminal)?;
                    let frame = sse_frame(&synthetic);
                    state.done = true;
                    Some((Ok::<web::Bytes, Infallible>(frame), state))
                }
            }
        },
    );

    HttpResponse::Ok()
        .insert_header(("x-run-id", run_id.to_string()))
        .insert_header((CONTENT_TYPE, "text/event-stream"))
        .insert_header((CACHE_CONTROL, "no-cache"))
        .streaming(byte_stream)
}

/// Serialize an [`AgentEvent`] into a single SSE frame's bytes.
///
/// Byte-for-byte matches the axum runtime's `to_sse_event` layout: an
/// `event: <tag>\n` line carrying the event's serde `type` discriminant (omitted
/// when the event has no `type` field), then a `data: <event-json>\n\n` line
/// carrying the full event JSON, terminated by the SSE frame's blank line.
fn sse_frame(ev: &AgentEvent) -> web::Bytes {
    let value = serde_json::to_value(ev).expect("AgentEvent serializes");
    let json = serde_json::to_string(&value).expect("serde_json::Value serializes");
    let mut s = String::new();
    if let Some(tag) = value.get("type").and_then(serde_json::Value::as_str) {
        s.push_str("event: ");
        s.push_str(tag);
        s.push('\n');
    }
    s.push_str("data: ");
    s.push_str(&json);
    s.push_str("\n\n");
    web::Bytes::from(s)
}
