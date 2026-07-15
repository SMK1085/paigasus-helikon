//! `POST /invocations` — the endpoint AgentCore calls to run the agent.
//!
//! One handler, [`invocations`], serves both response shapes AgentCore's
//! HTTP-protocol contract recognises, keyed on the request's `Accept` header:
//!
//! - `Accept: application/json` — **buffered**: block until the run reaches a
//!   terminal event, then return `200` with `{"final_output": "...", "usage": {...}}`.
//! - default / `Accept: text/event-stream` — **Server-Sent Events**: stream every
//!   [`AgentEvent`] as `data: <json>` frames, terminated by the run's
//!   `RunCompleted`/`RunFailed` event.
//!
//! # Request body
//!
//! See [`InvocationRequest`] for the three accepted JSON shapes.
//!
//! # Session resolution
//!
//! See the [`crate::session`] module for the optional
//! `X-Amzn-Bedrock-AgentCore-Runtime-Session-Id` header's validation rules. The
//! resolved [`Session`](paigasus_helikon_core::Session) — and the request's [`Parts`]
//! — are then handed to the configured
//! [`ContextProvider`](paigasus_helikon_runtime_axum::ContextProvider) to build the
//! [`RunContext`], mirroring `paigasus-helikon-runtime-axum`'s handler glue exactly so
//! a self-hosted deployment and an AgentCore deployment of the same agent share one
//! construction path.

use std::{convert::Infallible, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    Json,
};
use futures_util::{stream, StreamExt as _};
use paigasus_helikon_core::{
    AgentEvent, AgentInput, CancellationToken, Item, RunContext, TokenUsage,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::DropGuard;

use crate::{error::AgentCoreError, server::AppState, session::extract_session_id};

/// Upper bound on the request body buffered before deserializing (2 MiB). AgentCore
/// invocation payloads are conversational text/tool-call JSON, not file uploads, so
/// this comfortably covers real traffic while bounding worst-case memory per request.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

// ── InvocationRequest ─────────────────────────────────────────────────────────

/// Accepted request bodies for `POST /invocations`.
///
/// Exactly one of three JSON shapes:
/// - `{"messages": [Item, ...]}` — an explicit item list (use for multi-turn context
///   or non-text content parts).
/// - `{"prompt": "<text>"}` — shorthand for a single user text message.
/// - `{"input": "<text>"}` — identical semantics to `prompt`; AgentCore's own SDK
///   examples use both spellings interchangeably, so both are accepted.
///
/// `#[serde(untagged)]` tries each variant **in this declared order** — `Messages`
/// first (it is the only variant with a `messages` key, so trying it first cannot
/// misfire on a `prompt`/`input` body), then `Prompt`, then `Input`. Untagged
/// deserialization does not reject unrecognised extra keys (unlike
/// `#[serde(deny_unknown_fields)]`): `{"prompt": "hi", "junk": 1}` still parses as
/// `Prompt`, silently ignoring `junk`. This is documented, tested behavior, not an
/// oversight — AgentCore's own request envelope may carry additional
/// platform-reserved fields this crate does not need to know about.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum InvocationRequest {
    /// An explicit conversation turn as a list of [`Item`]s.
    Messages {
        /// The message list, appended after any persisted session history.
        messages: Vec<Item>,
    },
    /// A single user text message, AgentCore's `prompt` spelling.
    Prompt {
        /// The user's message text.
        prompt: String,
    },
    /// A single user text message, AgentCore's `input` spelling (identical semantics
    /// to [`InvocationRequest::Prompt`]).
    Input {
        /// The user's message text.
        input: String,
    },
}

impl InvocationRequest {
    /// Convert this request into an [`AgentInput`] ready to pass to a [`Runner`].
    ///
    /// [`Runner`]: paigasus_helikon_core::Runner
    fn into_agent_input(self) -> AgentInput {
        match self {
            InvocationRequest::Messages { messages } => {
                let mut input = AgentInput::new();
                input.messages = messages;
                input
            }
            InvocationRequest::Prompt { prompt } => AgentInput::from_user_text(prompt),
            InvocationRequest::Input { input } => AgentInput::from_user_text(input),
        }
    }
}

// ── InvocationResponse ────────────────────────────────────────────────────────

/// JSON-mode (`Accept: application/json`) response body for `POST /invocations`.
#[derive(Debug, Serialize)]
pub(crate) struct InvocationResponse {
    /// The run's final assistant output text.
    final_output: String,
    /// Aggregated token usage across the run.
    usage: TokenUsage,
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// `POST /invocations` handler. See the [module docs](self) for the full contract.
///
/// # Errors
///
/// - [`AgentCoreError::BadRequest`] (400) — the session header failed validation, the
///   body could not be read or was not valid JSON for an [`InvocationRequest`], or the
///   configured `SessionProvider`/`ContextProvider` reported a client-side problem.
/// - [`AgentCoreError::Internal`] (500) — session resolution, context construction, or
///   (JSON mode only) the run itself failed, or (JSON mode only) the detached run task
///   ended without reporting a result because it panicked or the runtime shut down. In
///   SSE mode a run failure is instead surfaced as the stream's terminal `RunFailed`
///   frame — the response itself stays `200`, per SSE semantics.
pub(crate) async fn invocations<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
    request: Request,
) -> Result<Response, AgentCoreError> {
    let (parts, body) = request.into_parts();

    let json_mode = wants_json(&parts.headers);
    let session_id = extract_session_id(&parts.headers)?;

    let bytes = axum::body::to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|e| AgentCoreError::BadRequest(format!("failed to read request body: {e}")))?;
    let invocation: InvocationRequest = serde_json::from_slice(&bytes)
        .map_err(|e| AgentCoreError::BadRequest(format!("invalid invocation request body: {e}")))?;
    let input = invocation.into_agent_input();

    let session = state.sessions.session(session_id).await?;
    let cancel = CancellationToken::new();
    // Retain a clone before it is moved into `ctx`: both transports need their own
    // handle on the token to cancel the run on client disconnect (see each one's
    // doc comment).
    let cancel_for_run = cancel.clone();
    let ctx = state.context.build(&parts, session, cancel).await?;

    if json_mode {
        run_json(&state, ctx, cancel_for_run, input).await
    } else {
        Ok(run_sse(&state, ctx, cancel_for_run, input).await)
    }
}

/// `true` if the request's `Accept` header selects the buffered JSON transport
/// (`application/json`, ignoring any `;` parameters). Every other case — an absent
/// header, `text/event-stream`, `*/*`, or any other media type — defaults to SSE, per
/// the AgentCore contract's "default is streaming" rule.
fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.split(';').next().unwrap_or("").trim() == "application/json")
        })
}

/// Buffered JSON-mode response: run to completion, then aggregate into an
/// [`InvocationResponse`].
///
/// # Disconnect semantics
///
/// The run is driven by a **detached** [`tokio::spawn`] task rather than awaited
/// inline in the handler future, mirroring [`run_sse`] (and
/// `paigasus-helikon-runtime-axum`'s `spawn_writer`):
///
/// - [`paigasus_helikon_core::Runner::run`] performs its finalize step — which
///   persists the turn to the session — inside the future it returns. Awaiting that
///   future *in the handler* would mean a client disconnect drops it mid-run and the
///   turn's session write is silently lost (SMA-456). Owning it in a detached task
///   decouples the run's lifetime from the HTTP response's, so finalize always runs.
/// - `cancel` (a clone of the token also embedded in `ctx`, retained by the caller —
///   see [`invocations`]) is wrapped in a [`DropGuard`] bound for the handler
///   future's lifetime. When that future is dropped — a client disconnecting mid-run
///   — the guard fires [`CancellationToken::cancel`], so the runner aborts the
///   in-flight run instead of running to its natural end. Dropping the guard after a
///   clean completion is harmless (cancelling a finished run is a no-op).
/// - Net effect: a disconnect cancels the run; the runner's stream ends, `finalize`
///   persists the recorder's events (the turn's user message plus any assistant/tool
///   items observed before the cancel), and `run` returns `Err(RunError::Cancelled)`
///   — which nobody is left to receive. The turn is persisted; nothing is leaked.
///   Unlike [`run_sse`], no synthetic terminal event is produced: terminal synthesis
///   lives in `run_streamed`, not in `Runner::run`.
///
/// Because finalize runs *before* `Runner::run`'s future resolves, a received result
/// implies the session write already landed — so the `200` is never returned ahead of
/// the persisted turn.
async fn run_json<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    ctx: RunContext<Ctx>,
    cancel: CancellationToken,
    input: AgentInput,
) -> Result<Response, AgentCoreError> {
    let runner = Arc::clone(&state.runner);
    let agent = Arc::clone(&state.agent);
    let run_config = state.run_config.clone();

    // Detached: its lifetime is independent of the handler future's, which is
    // exactly why the runner's finalize step always runs — see the doc above.
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let result = runner.run(agent.as_ref(), ctx, input, run_config).await;
        if tx.send(result).is_err() {
            // The client disconnected, so nobody is left to receive the outcome.
            // The session write has already happened (finalize runs before `run`
            // resolves), so this is bookkeeping, not a lost turn.
            tracing::debug!("invocation client disconnected; run outcome discarded");
        }
    });

    // MUST bind to a name: `let _ = cancel.drop_guard()` would drop the guard
    // immediately and cancel every run the instant it started.
    let _disconnect = cancel.drop_guard();

    let result = rx
        .await
        .map_err(|_| {
            tracing::error!(
                "run task ended without reporting a result (panicked or runtime shut down)"
            );
            AgentCoreError::Internal("run task ended without a result".to_owned())
        })?
        .map_err(|e| AgentCoreError::Internal(format!("run failed: {e}")))?;

    Ok(Json(InvocationResponse {
        final_output: result.final_output,
        usage: result.usage,
    })
    .into_response())
}

/// Server-Sent-Events response: stream every [`AgentEvent`] as a `data: <json>` frame.
///
/// If the run fails to *start* (before any event is emitted), a single synthetic
/// `RunFailed` frame is emitted in its place, so the stream always ends in a terminal
/// frame — the HTTP response itself stays `200`, per SSE semantics (the contract fact
/// that a run's failure is a stream-content concern, not a status-code concern).
///
/// # Disconnect semantics
///
/// The runner's event stream is driven by a **detached** [`tokio::spawn`] task, not
/// by the SSE response body directly — decoupling the run's lifetime from the HTTP
/// response's lifetime (the same pattern `paigasus-helikon-runtime-axum`'s
/// `spawn_writer` uses, sized down to this crate's single-shot, no-registry request):
///
/// - The driver task owns the runner's stream and polls it to its terminal
///   *unconditionally*, forwarding every event over a bounded channel to the
///   response body. [`paigasus_helikon_core::Runner::run_streamed`]'s finalize step —
///   which persists the turn to the session — only runs when its stream is driven to
///   termination, so draining unconditionally in the driver guarantees that happens
///   even once nobody is reading from the channel anymore.
/// - `cancel` (a clone of the token also embedded in `ctx`, retained by the caller —
///   see [`invocations`]) is wrapped in a [`DropGuard`] owned by the *response*
///   stream. When the SSE response is dropped — a client disconnecting mid-stream —
///   the guard fires [`CancellationToken::cancel`], so the runner aborts the
///   in-flight run promptly instead of running to its natural end. Dropping the
///   guard after a clean terminal is harmless (cancelling an already-finished run is
///   a no-op).
/// - Net effect: a client disconnect cancels the run promptly, the runner
///   synthesizes its terminal and finalizes/persists whatever events it had, and the
///   detached driver drains the (now-failing-to-send) channel to completion and
///   exits. No task is leaked and no partial turn is silently lost.
async fn run_sse<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    ctx: RunContext<Ctx>,
    cancel: CancellationToken,
    input: AgentInput,
) -> Response {
    let runner = Arc::clone(&state.runner);
    let agent = Arc::clone(&state.agent);
    let run_config = state.run_config.clone();

    // Bounded: the driver still applies backpressure to a slow-but-connected client
    // instead of buffering unboundedly, while staying small since SSE frames are
    // tiny and the driver's only real job is to keep the runner's stream moving.
    let (tx, rx) = mpsc::channel::<AgentEvent>(64);

    // Detached driver: its lifetime is independent of the response body's, which is
    // exactly why the runner's finalize step always runs — see the doc above.
    tokio::spawn(async move {
        let mut events = match runner
            .run_streamed(agent.as_ref(), ctx, input, run_config)
            .await
        {
            Ok(streaming) => streaming.events,
            Err(e) => stream::iter(vec![AgentEvent::RunFailed {
                error: e.to_string(),
            }])
            .boxed(),
        };
        while let Some(ev) = events.next().await {
            // Ignore send failures: a closed channel means the client disconnected
            // (the response stream, and with it its `Receiver`, was dropped), but
            // the driver must keep draining to the runner stream's terminal
            // regardless so finalize/persistence still runs.
            let _ = tx.send(ev).await;
        }
    });

    let stream = stream::unfold(
        SseDriverState {
            rx,
            disconnect: cancel.drop_guard(),
        },
        |mut state| async move {
            state
                .rx
                .recv()
                .await
                .map(|ev| (Ok::<Event, Infallible>(to_sse_event(&ev)), state))
        },
    );

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Unfold state for [`run_sse`]'s response stream: the channel the detached driver
/// task forwards events through, plus the cancel [`DropGuard`] that fires
/// [`CancellationToken::cancel`] when this state — and with it, the SSE response —
/// is dropped (a client disconnect).
struct SseDriverState {
    rx: mpsc::Receiver<AgentEvent>,
    /// Held only for its `Drop` side effect; never read directly.
    #[allow(dead_code)]
    disconnect: DropGuard,
}

/// Serialize an [`AgentEvent`] into an SSE [`Event`]'s `data:` payload.
///
/// [`AgentEvent`] always serializes (it derives `Serialize` with no fallible
/// user-supplied fields), so the `expect` here mirrors
/// `paigasus-helikon-runtime-axum`'s equivalent helper.
fn to_sse_event(ev: &AgentEvent) -> Event {
    Event::default().data(serde_json::to_string(ev).expect("AgentEvent serializes"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentCoreServer;
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request as HttpRequest, StatusCode},
    };
    use futures_util::stream::BoxStream;
    use paigasus_helikon_core::{Agent, AgentError, ContentPart, RunContext, TokenUsage};
    use tower::ServiceExt as _;

    // ── (a) DTO parsing ────────────────────────────────────────────────────

    #[test]
    fn messages_form_parses() {
        let req: InvocationRequest = serde_json::from_str(
            r#"{"messages":[{"type":"user_message","content":[{"type":"text","text":"hi"}]}]}"#,
        )
        .unwrap();
        assert!(matches!(req, InvocationRequest::Messages { messages } if messages.len() == 1));
    }

    #[test]
    fn prompt_form_parses() {
        let req: InvocationRequest = serde_json::from_str(r#"{"prompt":"hi"}"#).unwrap();
        assert!(matches!(req, InvocationRequest::Prompt { prompt } if prompt == "hi"));
    }

    #[test]
    fn input_form_parses() {
        let req: InvocationRequest = serde_json::from_str(r#"{"input":"hi"}"#).unwrap();
        assert!(matches!(req, InvocationRequest::Input { input } if input == "hi"));
    }

    /// Documents the untagged enum's actual (and accepted) behavior: an unrecognised
    /// extra key does NOT reject the request — it is silently ignored, and the body
    /// still parses as `Prompt`. See the [`InvocationRequest`] doc for why.
    #[test]
    fn extra_unknown_key_is_tolerated() {
        let req: InvocationRequest = serde_json::from_str(r#"{"prompt":"hi","junk":1}"#).unwrap();
        assert!(matches!(req, InvocationRequest::Prompt { prompt } if prompt == "hi"));
    }

    /// Pins the untagged enum's declared-order resolution: a body carrying BOTH a
    /// `messages` key AND a `prompt` key parses as `Messages`, because
    /// `#[serde(untagged)]` tries variants in declaration order and `Messages` is
    /// tried first (see the [`InvocationRequest`] doc). This is ambiguous input the
    /// AgentCore contract does not define, so this test exists to catch a silent
    /// resolution-order change (e.g. from reordering the enum's variants) rather
    /// than to bless the ambiguity as a supported shape.
    #[test]
    fn ambiguous_body_with_both_messages_and_prompt_parses_as_messages() {
        let req: InvocationRequest = serde_json::from_str(
            r#"{"messages":[{"type":"user_message","content":[{"type":"text","text":"hi"}]}],"prompt":"ignored"}"#,
        )
        .unwrap();
        assert!(matches!(req, InvocationRequest::Messages { messages } if messages.len() == 1));
    }

    // ── Test fixture: an echo agent ────────────────────────────────────────

    /// Emits one assistant message reporting how many messages it saw (the merged
    /// session history plus the new turn) — a cheap way to prove session continuity
    /// without a real model: a second request on the same session id sees a strictly
    /// larger count than the first.
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
            let count = input.messages.len();
            let events = vec![
                AgentEvent::RunStarted {
                    agent: self.name().to_owned(),
                },
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage {
                        content: vec![ContentPart::Text {
                            text: format!("saw {count} messages"),
                        }],
                        agent: Some(self.name().to_owned()),
                    },
                },
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ];
            Ok(stream::iter(events).boxed())
        }
    }

    fn test_server() -> AgentCoreServer<()> {
        AgentCoreServer::builder()
            .agent(std::sync::Arc::new(EchoAgent))
            .with_default_context()
            .build()
            .expect("server builds")
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn body_string(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // ── (b) JSON mode ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn json_mode_returns_final_output_and_usage() {
        let resp = test_server()
            .router()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/invocations")
                    .header("accept", "application/json")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"prompt":"hi"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["final_output"], "saw 1 messages");
        assert!(json.get("usage").is_some());
    }

    // ── (c) SSE mode ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn sse_mode_is_the_default_and_streams_events() {
        let resp = test_server()
            .router()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/invocations")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"prompt":"hi"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_owned();
        assert!(content_type.starts_with("text/event-stream"));

        let body = body_string(resp).await;
        assert!(
            body.contains(r#"data: {"type":"run_started""#),
            "missing run_started frame in: {body}"
        );
        assert!(
            body.contains(r#""type":"run_completed""#),
            "missing terminal run_completed frame in: {body}"
        );
    }

    #[tokio::test]
    async fn explicit_event_stream_accept_also_selects_sse() {
        let resp = test_server()
            .router()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/invocations")
                    .header("accept", "text/event-stream")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"prompt":"hi"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_owned();
        assert!(content_type.starts_with("text/event-stream"));
    }

    // ── (d) session-header validation ──────────────────────────────────────

    #[tokio::test]
    async fn short_session_id_is_rejected_with_json_error() {
        let resp = test_server()
            .router()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/invocations")
                    .header("accept", "application/json")
                    .header("content-type", "application/json")
                    .header("X-Amzn-Bedrock-AgentCore-Runtime-Session-Id", "too-short")
                    .body(Body::from(r#"{"prompt":"hi"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(json.get("error").is_some());
    }

    // ── (e) session continuity ──────────────────────────────────────────────

    #[tokio::test]
    async fn same_session_id_continues_the_conversation() {
        let server = test_server();
        let session_id = "a".repeat(40);

        let first = server
            .router()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/invocations")
                    .header("accept", "application/json")
                    .header("content-type", "application/json")
                    .header(
                        "X-Amzn-Bedrock-AgentCore-Runtime-Session-Id",
                        session_id.as_str(),
                    )
                    .body(Body::from(r#"{"prompt":"turn one"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_json = body_json(first).await;
        assert_eq!(first_json["final_output"], "saw 1 messages");

        let second = server
            .router()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/invocations")
                    .header("accept", "application/json")
                    .header("content-type", "application/json")
                    .header(
                        "X-Amzn-Bedrock-AgentCore-Runtime-Session-Id",
                        session_id.as_str(),
                    )
                    .body(Body::from(r#"{"prompt":"turn two"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let second_json = body_json(second).await;
        // Turn 1 persisted a user message + an assistant message (2 events); turn 2's
        // merged input is that history plus the new user message = 3.
        assert_eq!(
            second_json["final_output"], "saw 3 messages",
            "second request on the same session id must see turn 1's history"
        );
    }

    #[tokio::test]
    async fn different_session_ids_do_not_share_history() {
        let server = test_server();

        let resp_a = server
            .router()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/invocations")
                    .header("accept", "application/json")
                    .header("content-type", "application/json")
                    .header(
                        "X-Amzn-Bedrock-AgentCore-Runtime-Session-Id",
                        "a".repeat(40).as_str(),
                    )
                    .body(Body::from(r#"{"prompt":"hi"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let json_a = body_json(resp_a).await;
        assert_eq!(json_a["final_output"], "saw 1 messages");

        let resp_b = server
            .router()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/invocations")
                    .header("accept", "application/json")
                    .header("content-type", "application/json")
                    .header(
                        "X-Amzn-Bedrock-AgentCore-Runtime-Session-Id",
                        "b".repeat(40).as_str(),
                    )
                    .body(Body::from(r#"{"prompt":"hi"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let json_b = body_json(resp_b).await;
        assert_eq!(json_b["final_output"], "saw 1 messages");
    }

    // ── (f) SSE client disconnect cancels and finalizes the run ─────────────

    /// Emits one `RunStarted` event then hangs for 30s before it would emit
    /// `RunCompleted` — long enough that the test's client disconnect always wins
    /// the race against a natural completion.
    struct SlowAgent;

    #[async_trait]
    impl Agent<()> for SlowAgent {
        fn name(&self) -> &str {
            "slow"
        }

        fn description(&self) -> &str {
            "test-only agent that emits one event then hangs"
        }

        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            let started = stream::once(async {
                AgentEvent::RunStarted {
                    agent: "slow".to_owned(),
                }
            });
            let hangs = stream::once(async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                }
            });
            Ok(started.chain(hangs).boxed())
        }
    }

    /// A client that reads the first SSE frame then drops the connection mid-run
    /// must still get its turn persisted: the detached driver task (see
    /// [`run_sse`]'s doc comment) keeps draining the runner's stream to its
    /// terminal — guaranteeing
    /// [`paigasus_helikon_core::Runner::run_streamed`]'s finalize step runs — while
    /// the retained cancel token aborts the now-orphaned run promptly instead of
    /// leaking it for the agent's full 30-second hang.
    ///
    /// Drives a real TCP disconnect (rather than `Router::oneshot`, which buffers
    /// the whole response and cannot model a client walking away mid-stream).
    #[tokio::test]
    async fn sse_client_disconnect_still_finalizes_the_session() {
        use paigasus_helikon_runtime_axum::{InMemorySessionProvider, SessionProvider};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let sessions = Arc::new(InMemorySessionProvider::new(16));
        let server = AgentCoreServer::<()>::builder()
            .agent(Arc::new(SlowAgent))
            .with_default_context()
            .session_provider(Arc::clone(&sessions) as Arc<dyn SessionProvider>)
            .build()
            .expect("server builds");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = server.router();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let session_id = "d".repeat(40);
        let body = r#"{"prompt":"hi"}"#;
        let len = body.len();
        let request = format!(
            "POST /invocations HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Content-Type: application/json\r\n\
             Accept: text/event-stream\r\n\
             X-Amzn-Bedrock-AgentCore-Runtime-Session-Id: {session_id}\r\n\
             Content-Length: {len}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}"
        );

        {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            client.write_all(request.as_bytes()).await.unwrap();

            // Read until the first SSE frame (`run_started`) has arrived, then let
            // `client` drop at the end of this block — a real mid-stream
            // disconnect, not a graceful close.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let n = client.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "connection closed before any data arrived");
                    buf.extend_from_slice(&chunk[..n]);
                    if String::from_utf8_lossy(&buf).contains("run_started") {
                        break;
                    }
                }
            })
            .await
            .expect("timed out waiting for the first SSE frame");
        }

        // The dropped connection must cancel the run, and the detached driver must
        // still drain it to a (synthetic) terminal, finalizing the session with the
        // turn's input message. Poll with a timeout since finalize is async and
        // races the server noticing the TCP disconnect.
        let session = sessions.session(Some(&session_id)).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let snapshot = session.snapshot().await.unwrap();
                if !snapshot.messages.is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("session was never finalized after the client disconnected");

        let snapshot = session.snapshot().await.unwrap();
        assert!(
            matches!(&snapshot.messages[0], Item::UserMessage { .. }),
            "expected the turn's user message to be persisted, got {:?}",
            snapshot.messages[0]
        );
    }

    // ── (g) JSON client disconnect cancels and finalizes the run ────────────

    /// Signals on `started` from its FIRST stream element, then hangs for 30s
    /// before it would emit `RunCompleted`.
    ///
    /// The signal exists because a JSON-mode client receives nothing until the
    /// run ends — unlike SSE there is no frame to key a disconnect off, and a
    /// fixed sleep would race the run's start.
    struct SignallingSlowAgent {
        started: mpsc::UnboundedSender<()>,
    }

    #[async_trait]
    impl Agent<()> for SignallingSlowAgent {
        fn name(&self) -> &str {
            "signalling-slow"
        }

        fn description(&self) -> &str {
            "test-only agent that signals run start then hangs"
        }

        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            let started = self.started.clone();
            let first = stream::once(async move {
                let _ = started.send(());
                AgentEvent::RunStarted {
                    agent: "signalling-slow".to_owned(),
                }
            });
            let hangs = stream::once(async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                }
            });
            Ok(first.chain(hangs).boxed())
        }
    }

    /// A JSON-mode client that disconnects mid-run must still get its turn
    /// persisted: the detached run task (see [`run_json`]'s doc comment) drives
    /// the runner to its terminal — guaranteeing `TokioRunner::run`'s inline
    /// finalize step executes — while the retained cancel token aborts the
    /// now-orphaned run instead of leaking it for the agent's full 30s hang.
    ///
    /// The 30s hang against a 10s poll window is deliberate: a shorter hang
    /// would let a NON-cancelling implementation pass, because the run would
    /// finalize naturally inside the window. Do not shorten it.
    ///
    /// Drives a real TCP disconnect (rather than `Router::oneshot`, which
    /// buffers the whole response and cannot model a client walking away).
    #[tokio::test]
    async fn json_client_disconnect_still_finalizes_the_session() {
        use paigasus_helikon_runtime_axum::{InMemorySessionProvider, SessionProvider};
        use tokio::io::AsyncWriteExt as _;

        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let sessions = Arc::new(InMemorySessionProvider::new(16));
        let server = AgentCoreServer::<()>::builder()
            .agent(Arc::new(SignallingSlowAgent {
                started: started_tx,
            }))
            .with_default_context()
            .session_provider(Arc::clone(&sessions) as Arc<dyn SessionProvider>)
            .build()
            .expect("server builds");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = server.router();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let session_id = "e".repeat(40);
        let body = r#"{"prompt":"hi"}"#;
        let len = body.len();
        let request = format!(
            "POST /invocations HTTP/1.1\r\n\
             Host: {addr}\r\n\
             Content-Type: application/json\r\n\
             Accept: application/json\r\n\
             X-Amzn-Bedrock-AgentCore-Runtime-Session-Id: {session_id}\r\n\
             Content-Length: {len}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}"
        );

        {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            client.write_all(request.as_bytes()).await.unwrap();

            // Wait until the run has demonstrably started server-side, then let
            // `client` drop at the end of this block — a real mid-run
            // disconnect, not a graceful close.
            tokio::time::timeout(std::time::Duration::from_secs(10), started_rx.recv())
                .await
                .expect("timed out waiting for the run to start")
                .expect("agent signalled run start");
        }

        // The dropped connection must cancel the run, and the detached task must
        // still run `finalize`, persisting the turn's input message. (`Runner::run`
        // aborts on cancel without synthesizing a terminal — that behavior belongs
        // to `run_streamed` — so the assertion below is on the persisted user
        // message, not on a terminal event.)
        let session = sessions.session(Some(&session_id)).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let snapshot = session.snapshot().await.unwrap();
                if !snapshot.messages.is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("session was never finalized after the JSON client disconnected");

        let snapshot = session.snapshot().await.unwrap();
        assert!(
            matches!(&snapshot.messages[0], Item::UserMessage { .. }),
            "expected the turn's user message to be persisted, got {:?}",
            snapshot.messages[0]
        );
    }
}
