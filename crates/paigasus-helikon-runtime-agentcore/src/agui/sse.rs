//! AG-UI `POST /invocations` — [`RunAgentInput`] in, an AG-UI SSE event stream out.
//!
//! # Stateless per request
//!
//! AG-UI clients resend the entire conversation in `messages` on every request, while
//! [`Runner::run_streamed`] seeds the model with `history ++ input.messages`. Pairing a
//! persisted session with a full client history therefore double-counts every prior
//! turn. This handler resolves a **fresh, unshared session per request** and treats
//! `messages` as the whole conversation — the same shape MCP mode uses, and with the
//! same consequence: AG-UI mode cannot use a persistent session backend in v0. The
//! `X-Amzn-Bedrock-AgentCore-Runtime-Session-Id` header is still validated (a malformed
//! header is a contract violation regardless of mode) and, when present, takes
//! precedence over the body's `threadId` as this run's thread id — platform-authoritative
//! beats client-supplied, matching `crate::agui::ws` and A2A's `contextId` rule. It is
//! never used to look up a stored session.
//!
//! [`Runner::run_streamed`]: paigasus_helikon_core::Runner::run_streamed
//!
//! # Every run emits exactly one `RUN_STARTED`
//!
//! AG-UI's contract opens every run with `RUN_STARTED`, but nothing in
//! [`Agent`](paigasus_helikon_core::Agent) requires an implementation to emit the core
//! `AgentEvent::RunStarted` convention most (not all) agents follow. This handler
//! therefore emits the frame itself, unconditionally, as the very first element of the
//! response stream — before the agent has produced anything — and discards any
//! `AgentEvent::RunStarted` the agent's own stream produces (its only effect in
//! [`EventMapper::push`] is to emit that exact same frame again), so a well-behaved
//! agent's own signal never produces a second, duplicate `RUN_STARTED`.
//!
//! # A stream that ends without a terminal event still closes cleanly
//!
//! [`Runner::run_streamed`] usually guarantees a terminal `RunCompleted`/`RunFailed` —
//! synthesizing one on cancellation or timeout — but an [`Agent`](paigasus_helikon_core::Agent)
//! whose stream simply ends with neither a terminal event nor a cancellation in flight
//! defeats even that. This handler calls [`EventMapper::finish`] once the channel from
//! the detached driver closes, so a text/tool-call/step span left open by such a stream
//! is still closed on the wire rather than left dangling.
//!
//! # Disconnect
//!
//! Identical in spirit to `/invocations`' HTTP-protocol counterpart
//! (`crate::invoke::run_sse`): the run is driven by a detached task so its finalize step
//! always runs, with a [`CancellationToken`] drop-guard on the response so a departed
//! client stops the run. Unlike A2A, the guard *does* apply here — AG-UI has no
//! resubscribe, so nothing is waiting to reattach.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
};
use futures_util::stream::{self, StreamExt as _};
use paigasus_helikon_core::{AgentEvent, CancellationToken, Session};
use paigasus_helikon_runtime_axum::{InMemorySessionProvider, SessionKey, SessionProvider as _};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::DropGuard;
use uuid::Uuid;

use crate::{
    agui::{
        map::EventMapper,
        types::{event, RunAgentInput},
    },
    server::AppState,
    session::extract_session_id,
};

/// Upper bound on the buffered request body (2 MiB), matching `/invocations`.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// `POST /invocations` — see the [module docs](self) for the full contract.
pub(crate) async fn invocations<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();

    // Validate the session header for its own sake even though AG-UI mode does not
    // persist through it — a malformed header is still a contract violation. Owned
    // (`to_owned`, not the borrowed `&str` the extractor returns) because `parts` is
    // consumed by `state.context.build` further down, and this value must outlive it.
    let header_session: Option<String> = match extract_session_id(&parts.headers) {
        Ok(id) => id.map(str::to_owned),
        Err(e) => {
            return error_stream(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", &e.to_string());
        }
    };

    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return error_stream(
                StatusCode::BAD_REQUEST,
                "VALIDATION_ERROR",
                &format!("failed to read request body: {e}"),
            );
        }
    };
    let input: RunAgentInput = match serde_json::from_slice(&bytes) {
        Ok(i) => i,
        Err(e) => {
            return error_stream(
                StatusCode::BAD_REQUEST,
                "VALIDATION_ERROR",
                &format!("invalid RunAgentInput body: {e}"),
            );
        }
    };

    let thread_id = header_session
        .or_else(|| input.thread_id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let run_id = input
        .run_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Fresh, unshared session: see the module docs. `InMemorySessionProvider::session`
    // never actually errs when `key.id` is `None` (an anonymous request always gets a
    // brand-new, unstored session), but the fallible seam is kept rather than unwrapped,
    // matching every other session resolution in this crate.
    let session: Arc<dyn Session> = match InMemorySessionProvider::new(1)
        .session(SessionKey::new(None, None))
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return error_stream(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                &e.to_string(),
            );
        }
    };

    let cancel = CancellationToken::new();
    // Retained for the response stream's drop-guard; see the module docs.
    let cancel_for_run = cancel.clone();
    let ctx = match state.context.build(&parts, session, cancel).await {
        Ok(c) => c,
        Err(e) => {
            return error_stream(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                &e.to_string(),
            );
        }
    };

    let agent_input = input.into_agent_input();
    let (tx, rx) = mpsc::channel::<AgentEvent>(64);
    let runner = Arc::clone(&state.runner);
    let agent = Arc::clone(&state.agent);
    let run_config = state.run_config.clone();

    // Detached driver: its lifetime is independent of the response body's, so the
    // runner's finalize step always runs even if the client disconnects — mirroring
    // `crate::invoke::run_sse` exactly.
    tokio::spawn(async move {
        let mut events = match runner
            .run_streamed(agent.as_ref(), ctx, agent_input, run_config)
            .await
        {
            Ok(streaming) => streaming.events,
            Err(e) => stream::iter(vec![AgentEvent::RunFailed {
                error: e.to_string(),
            }])
            .boxed(),
        };
        while let Some(ev) = events.next().await {
            // Ignore send failures: a closed channel means the client disconnected,
            // but the driver must keep draining to the runner stream's terminal
            // regardless so finalize/persistence still runs.
            let _ = tx.send(ev).await;
        }
    });

    let mapper = EventMapper::new(thread_id.clone(), run_id.clone());
    // The transport-level `RUN_STARTED` (see the module docs); queued ahead of anything
    // the agent's own stream produces.
    let start_frame = event::run_started(&thread_id, &run_id);

    let frames = stream::unfold(
        FrameGen {
            rx,
            mapper,
            pending: VecDeque::from([start_frame]),
            finished: false,
            disconnect: cancel_for_run.drop_guard(),
        },
        next_frame,
    )
    .map(|value| Ok::<Event, Infallible>(Event::default().data(value.to_string())));

    Sse::new(frames)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// State threaded through [`next_frame`]'s [`stream::unfold`].
struct FrameGen {
    /// Forwards events from the detached driver task (see [`invocations`]).
    rx: mpsc::Receiver<AgentEvent>,
    /// Owns the bracketing state machine for this run.
    mapper: EventMapper,
    /// Frames queued for emission before the next `rx.recv()`. Seeded with the
    /// synthetic `RUN_STARTED` frame; refilled by [`EventMapper::push`] or
    /// [`EventMapper::finish`], either of which may return zero, one, or several
    /// frames per call — `push`/`finish`'s `Vec<Value>` has to be drained one item at a
    /// time to become individual SSE frames.
    pending: VecDeque<Value>,
    /// Set once `rx` has closed and [`EventMapper::finish`] has run — an `mpsc`
    /// receiver keeps returning `None` immediately on every subsequent `recv()`, so
    /// without this flag [`next_frame`] would call `finish` (and therefore
    /// `close_all`) forever once `pending` drained empty, since `close_all` on an
    /// already-fully-closed mapper is a legal, silent no-op rather than an error.
    finished: bool,
    /// Held only for its `Drop` side effect: firing [`CancellationToken::cancel`] when
    /// this state — and with it, the SSE response — is dropped (a client disconnect).
    /// Never read directly.
    #[allow(dead_code)]
    disconnect: DropGuard,
}

/// Advance [`FrameGen`] by exactly one output frame, draining `rx` and running it
/// through the mapper as needed. See [`FrameGen`]'s fields for why a `pending` queue and
/// a `finished` flag are both required.
async fn next_frame(mut state: FrameGen) -> Option<(Value, FrameGen)> {
    loop {
        if let Some(frame) = state.pending.pop_front() {
            return Some((frame, state));
        }
        if state.finished {
            return None;
        }
        match state.rx.recv().await {
            Some(ev) => {
                // The transport already emitted its own `RUN_STARTED` up front (see the
                // module docs) — never forward the agent's own copy, or a well-behaved
                // agent that follows the `AgentEvent::RunStarted` convention would
                // produce a second, duplicate frame.
                if !matches!(ev, AgentEvent::RunStarted { .. }) {
                    state.pending.extend(state.mapper.push(&ev));
                }
            }
            None => {
                state.pending.extend(state.mapper.finish());
                state.finished = true;
            }
        }
    }
}

/// A single-frame `RUN_ERROR` stream with a real HTTP status.
///
/// AG-UI serializes every error as a `RUN_ERROR` SSE event; the status code is the
/// error's own when the stream has not begun (this function's every caller), and `200`
/// once it has (the in-stream `AGENT_ERROR` case [`EventMapper::push`] handles instead).
fn error_stream(status: StatusCode, code: &str, message: &str) -> Response {
    let frame = event::run_error(code, message);
    let body = format!("data: {frame}\n\n");
    (status, [(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

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

    use crate::AgentCoreServer;

    /// Records how many messages each run was given, so a test can prove turn 2 was not
    /// handed the conversation twice.
    struct CountingAgent {
        seen: Arc<Mutex<Vec<usize>>>,
    }

    #[async_trait]
    impl Agent<()> for CountingAgent {
        fn name(&self) -> &str {
            "counting"
        }
        fn description(&self) -> &str {
            "records input message counts"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            self.seen.lock().unwrap().push(input.messages.len());
            Ok(stream::iter(vec![
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage {
                        content: vec![ContentPart::Text {
                            text: "ok".to_owned(),
                        }],
                        agent: None,
                    },
                },
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ])
            .boxed())
        }
    }

    fn server(seen: Arc<Mutex<Vec<usize>>>) -> AgentCoreServer<()> {
        AgentCoreServer::builder()
            .agent(Arc::new(CountingAgent { seen }))
            .with_default_context()
            .build()
            .expect("server builds")
    }

    async fn post(server: &AgentCoreServer<()>, body: &str, session: Option<&str>) -> String {
        let mut req = Request::builder().method("POST").uri("/invocations");
        if let Some(s) = session {
            req = req.header("X-Amzn-Bedrock-AgentCore-Runtime-Session-Id", s);
        }
        let resp = server
            .agui_router()
            .oneshot(req.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn streams_the_documented_agui_event_sequence() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let body = post(
            &server(Arc::clone(&seen)),
            r#"{"threadId":"t1","runId":"r1","messages":[{"role":"user","content":"hi"}]}"#,
            None,
        )
        .await;
        assert!(body.contains(r#""type":"RUN_STARTED""#), "body: {body}");
        assert!(
            body.contains(r#""type":"TEXT_MESSAGE_START""#),
            "body: {body}"
        );
        assert!(body.contains(r#""type":"RUN_FINISHED""#), "body: {body}");
        assert!(body.contains(r#""threadId":"t1""#));
        assert!(body.contains(r#""runId":"r1""#));
    }

    /// Regression for the double-counting bug: AG-UI clients resend the full
    /// conversation each turn, so a second request carrying 3 messages must reach the
    /// agent as exactly 3 — not 3 plus a replayed session history.
    #[tokio::test]
    async fn turn_two_does_not_double_count_history() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = server(Arc::clone(&seen));
        let session = "a-session-id-that-is-long-enough-to-pass-validation-000";

        post(
            &s,
            r#"{"threadId":"t1","runId":"r1","messages":[{"role":"user","content":"one"}]}"#,
            Some(session),
        )
        .await;
        post(
            &s,
            r#"{"threadId":"t1","runId":"r2","messages":[
                {"role":"user","content":"one"},
                {"role":"assistant","content":"ok"},
                {"role":"user","content":"two"}
            ]}"#,
            Some(session),
        )
        .await;

        let counts = seen.lock().unwrap().clone();
        assert_eq!(
            counts,
            vec![1, 3],
            "turn 2 must see exactly the client's 3 messages, not a doubled history"
        );
    }

    #[tokio::test]
    async fn an_invalid_body_yields_a_run_error_frame() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = server(seen);
        let resp = s
            .agui_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/invocations")
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains(r#""type":"RUN_ERROR""#), "body: {body}");
        assert!(body.contains("VALIDATION_ERROR"), "body: {body}");
    }

    #[tokio::test]
    async fn ping_is_reachable_on_the_agui_router() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let resp = server(seen)
            .agui_router()
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ── Regressions for defects found while implementing this handler ─────────

    /// Regression: a well-behaved agent that follows the core `AgentEvent::RunStarted`
    /// convention (unlike `CountingAgent` above, which — like the AG-UI contract
    /// itself — does not require it) must not cause a second, duplicate `RUN_STARTED`
    /// frame. The transport's own synthetic `RUN_STARTED` (see the module docs) and the
    /// agent's own event both map to the identical frame, and only one may reach the
    /// wire.
    #[tokio::test]
    async fn an_agents_own_run_started_event_does_not_duplicate_the_frame() {
        struct SelfAnnouncingAgent;

        #[async_trait]
        impl Agent<()> for SelfAnnouncingAgent {
            fn name(&self) -> &str {
                "self-announcing"
            }
            fn description(&self) -> &str {
                "emits its own RunStarted, like a real LlmAgent"
            }
            async fn run(
                &self,
                _ctx: RunContext<()>,
                _input: AgentInput,
            ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
                Ok(stream::iter(vec![
                    AgentEvent::RunStarted {
                        agent: "self-announcing".to_owned(),
                    },
                    AgentEvent::RunCompleted {
                        usage: TokenUsage::default(),
                    },
                ])
                .boxed())
            }
        }

        let server = AgentCoreServer::<()>::builder()
            .agent(Arc::new(SelfAnnouncingAgent))
            .with_default_context()
            .build()
            .expect("server builds");
        let resp = server
            .agui_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/invocations")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        let count = body.matches(r#""type":"RUN_STARTED""#).count();
        assert_eq!(
            count, 1,
            "exactly one RUN_STARTED frame, got {count} in: {body}"
        );
    }

    /// Regression: an agent whose stream ends with no terminal event at all (neither a
    /// real `RunCompleted`/`RunFailed` nor a cancellation/timeout for `TokioRunner` to
    /// synthesize one from) must still leave the wire well-formed — `EventMapper`'s
    /// still-open tool-call span is closed via `EventMapper::finish`, not left
    /// dangling.
    #[tokio::test]
    async fn a_stream_that_ends_without_a_terminal_event_still_closes_open_spans() {
        struct TrailsOffAgent;

        #[async_trait]
        impl Agent<()> for TrailsOffAgent {
            fn name(&self) -> &str {
                "trails-off"
            }
            fn description(&self) -> &str {
                "opens a tool call then ends its stream with no terminal event"
            }
            async fn run(
                &self,
                _ctx: RunContext<()>,
                _input: AgentInput,
            ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
                Ok(stream::iter(vec![AgentEvent::ToolCallDelta {
                    call_id: "tc1".to_owned(),
                    name: Some("search".to_owned()),
                    args_delta: "{}".to_owned(),
                }])
                .boxed())
            }
        }

        let server = AgentCoreServer::<()>::builder()
            .agent(Arc::new(TrailsOffAgent))
            .with_default_context()
            .build()
            .expect("server builds");
        let resp = server
            .agui_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/invocations")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            body.contains(r#""type":"TOOL_CALL_END""#),
            "the dangling tool-call span must still be closed: {body}"
        );
        assert!(
            !body.contains(r#""type":"RUN_FINISHED""#) && !body.contains(r#""type":"RUN_ERROR""#),
            "no terminal was ever observed, so none may be fabricated: {body}"
        );
    }
}
