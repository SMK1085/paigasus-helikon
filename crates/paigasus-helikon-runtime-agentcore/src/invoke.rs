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

use std::convert::Infallible;

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
///   (JSON mode only) the run itself failed. In SSE mode a run failure is instead
///   surfaced as the stream's terminal `RunFailed` frame — the response itself stays
///   `200`, per SSE semantics.
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
    let ctx = state.context.build(&parts, session, cancel).await?;

    if json_mode {
        run_json(&state, ctx, input).await
    } else {
        Ok(run_sse(&state, ctx, input).await)
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
async fn run_json<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    ctx: RunContext<Ctx>,
    input: AgentInput,
) -> Result<Response, AgentCoreError> {
    let result = state
        .runner
        .run(state.agent.as_ref(), ctx, input, state.run_config.clone())
        .await
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
async fn run_sse<Ctx: Send + Sync + 'static>(
    state: &AppState<Ctx>,
    ctx: RunContext<Ctx>,
    input: AgentInput,
) -> Response {
    let events = match state
        .runner
        .run_streamed(state.agent.as_ref(), ctx, input, state.run_config.clone())
        .await
    {
        Ok(streaming) => streaming.events,
        Err(e) => stream::iter(vec![AgentEvent::RunFailed {
            error: e.to_string(),
        }])
        .boxed(),
    };

    let frames = events.map(|ev| Ok::<Event, Infallible>(to_sse_event(&ev)));
    Sse::new(frames)
        .keep_alive(KeepAlive::default())
        .into_response()
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
}
