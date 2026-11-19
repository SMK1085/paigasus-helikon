//! Integration tests for the run endpoints (one-shot, SSE, async).

mod support;

use std::sync::Arc;

use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, Session, TokenUsage,
};
use paigasus_helikon_runtime_axum::{AgentServer, ContextProvider, ServerError};
use tokio_util::sync::CancellationToken;

/// **AC1** — a one-shot `POST /agents/{name}/runs` returns the aggregated run
/// result as JSON, with an `x-run-id` response header.
#[tokio::test]
async fn oneshot_returns_aggregated_result() {
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hello"}"#)
        .send()
        .await
        .unwrap();
    assert!(resp.headers().contains_key("x-run-id"));
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["status"], "completed");
    assert_eq!(v["output"], "echo");
}

/// **AC2** — the SSE stream emits exactly the agent's local event sequence,
/// event for event.
#[tokio::test]
async fn sse_stream_matches_local_events() {
    let addr = support::spawn_echo_server().await;
    let text = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs?stream=sse"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let got = support::parse_sse(&text);
    let want = support::echo_script();
    // `AgentEvent` does not derive `PartialEq` in core, so assert event-for-event
    // equality through the canonical JSON of each sequence.
    assert_eq!(
        serde_json::to_value(&got).unwrap(),
        serde_json::to_value(&want).unwrap(),
    );
}

/// `?mode=async` accepts the run and returns `202 Accepted` with a `run_id`
/// field, without waiting for the run to finish.
#[tokio::test]
async fn async_mode_returns_202() {
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs?mode=async"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(v["run_id"].is_string());
}

/// An unrecognised `?mode=` selector is rejected with `400 Bad Request` instead
/// of silently falling back to one-shot.
#[tokio::test]
async fn invalid_mode_selector_is_400() {
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs?mode=bogus"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// An unrecognised `?stream=` selector is rejected with `400 Bad Request`.
#[tokio::test]
async fn invalid_stream_selector_is_400() {
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs?stream=foo"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// Requesting the async and SSE transports together is rejected with `400 Bad
/// Request` rather than silently preferring one.
#[tokio::test]
async fn conflicting_async_and_sse_is_400() {
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new()
        .post(format!(
            "http://{addr}/agents/echo/runs?mode=async&stream=sse"
        ))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// Posting to an unregistered agent name returns `404 Not Found`.
#[tokio::test]
async fn unknown_agent_404() {
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/nope/runs"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// A run whose runner start-errors must still surface a final synthetic
/// `run_failed` frame on the SSE stream (the stream is HTTP 200; the failure
/// is in-band), then close.
#[tokio::test]
async fn sse_emits_synthetic_run_failed_on_start_error() {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .runner(Arc::new(support::FailingRunner))
        .agent(Arc::new(support::ScriptedAgent {
            name: "echo".into(),
            events: support::echo_script(),
        }))
        .build()
        .expect("server builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { server.serve_with_listener(listener).await.unwrap() });

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs?stream=sse"))
        .header("content-type", "application/json")
        .body(r#"{"input":"x"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "SSE responses are 200; failure is in-band"
    );
    let events = support::parse_sse(&resp.text().await.unwrap());
    assert_eq!(events.len(), 1, "exactly one synthetic terminal frame");
    assert!(
        matches!(&events[0], AgentEvent::RunFailed { error } if !error.is_empty()),
        "expected a non-empty RunFailed, got {:?}",
        events[0]
    );
}

/// A run that yields real events then ends with no terminal must get a final
/// synthetic `run_failed` frame (generic message) appended AFTER the real events.
#[tokio::test]
async fn sse_emits_synthetic_run_failed_after_terminalless_stream() {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .runner(Arc::new(support::PartialThenEndRunner))
        .agent(Arc::new(support::ScriptedAgent {
            name: "echo".into(),
            events: vec![],
        }))
        .build()
        .expect("server builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { server.serve_with_listener(listener).await.unwrap() });

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs?stream=sse"))
        .header("content-type", "application/json")
        .body(r#"{"input":"x"}"#)
        .send()
        .await
        .unwrap();
    let events = support::parse_sse(&resp.text().await.unwrap());
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], AgentEvent::TokenDelta { text } if text == "hi"));
    assert!(
        matches!(&events[1], AgentEvent::RunFailed { error }
            if error == "run ended before producing a terminal event"),
        "expected generic RunFailed last, got {:?}",
        events[1]
    );
}

/// A [`ContextProvider`] that turns the deferral flag on, as a careless
/// operator would.
struct DeferringProvider;

#[async_trait::async_trait]
impl ContextProvider<()> for DeferringProvider {
    async fn build(
        &self,
        _parts: &axum::http::request::Parts,
        session: Arc<dyn Session>,
        cancel: CancellationToken,
    ) -> Result<RunContext<()>, ServerError> {
        Ok(RunContext::ephemeral(())
            .with_session(session)
            .with_cancel(cancel)
            .with_deferred_tool_calls(true))
    }
}

/// An agent that reports whether the deferral flag reached it.
struct DeferralProbe;

#[async_trait::async_trait]
impl Agent<()> for DeferralProbe {
    fn name(&self) -> &str {
        "probe"
    }

    fn description(&self) -> &str {
        "reports the deferral flag"
    }

    async fn run(
        &self,
        ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let events = vec![
            AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text {
                        text: format!("deferral={}", ctx.deferred_tool_calls_enabled()),
                    }],
                    agent: None,
                },
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        Ok(stream::iter(events).boxed())
    }
}

/// A one-shot run clears the deferral flag the operator's provider set.
#[tokio::test]
async fn oneshot_run_clears_the_deferral_flag() {
    let server = AgentServer::<()>::builder()
        .context_provider(Arc::new(DeferringProvider))
        .agent(Arc::new(DeferralProbe))
        .build()
        .expect("server builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        server.serve_with_listener(listener).await.unwrap();
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/probe/runs"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hello"}"#)
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["output"], "deferral=false");
}

// ── Turn checkpoint is not supported over HTTP ─────────────────────────────

/// A checkpoint that counts its calls.
struct CountingCheckpoint(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl paigasus_helikon_core::TurnCheckpoint<()> for CountingCheckpoint {
    async fn at_boundary(
        &self,
        _ctx: &paigasus_helikon_core::RunContext<()>,
        _boundary: &paigasus_helikon_core::TurnBoundary,
    ) -> paigasus_helikon_core::CheckpointDecision {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        paigasus_helikon_core::CheckpointDecision::Continue
    }
}

/// A provider that sets a counting checkpoint on every context.
struct CheckpointProvider(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl paigasus_helikon_runtime_axum::ContextProvider<()> for CheckpointProvider {
    async fn build(
        &self,
        _parts: &axum::http::request::Parts,
        session: Arc<dyn paigasus_helikon_core::Session>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<paigasus_helikon_core::RunContext<()>, paigasus_helikon_runtime_axum::ServerError>
    {
        Ok(paigasus_helikon_core::RunContext::ephemeral(())
            .with_session(session)
            .with_cancel(cancel)
            .with_turn_checkpoint(Arc::new(CountingCheckpoint(Arc::clone(&self.0)))))
    }
}

/// An agent that makes one tool turn: it calls the turn checkpoint at the
/// boundary, as the agent loop does, when the context carries one.
struct OneTurnAgent;

#[async_trait::async_trait]
impl paigasus_helikon_core::Agent<()> for OneTurnAgent {
    fn name(&self) -> &str {
        "probe"
    }

    fn description(&self) -> &str {
        "calls the turn checkpoint once"
    }

    async fn run(
        &self,
        ctx: paigasus_helikon_core::RunContext<()>,
        _input: paigasus_helikon_core::AgentInput,
    ) -> Result<
        futures_util::stream::BoxStream<'static, AgentEvent>,
        paigasus_helikon_core::AgentError,
    > {
        use futures_util::StreamExt as _;
        if let Some(cp) = ctx.turn_checkpoint() {
            let boundary = paigasus_helikon_core::TurnBoundary::new(0, 1, "probe", 0);
            cp.at_boundary(&ctx, &boundary).await;
        }
        let events = vec![AgentEvent::RunCompleted {
            usage: paigasus_helikon_core::TokenUsage::default(),
        }];
        Ok(futures_util::stream::iter(events).boxed())
    }
}

/// The HTTP runtime cannot support the checkpoint yet, so it clears it. A
/// provider that sets one sees a counter of 0, and the run still completes.
#[tokio::test]
async fn context_provider_checkpoint_never_fires() {
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server = AgentServer::<()>::builder()
        .context_provider(Arc::new(CheckpointProvider(Arc::clone(&counter))))
        .agent(Arc::new(OneTurnAgent))
        .build()
        .expect("server builds");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        server.serve_with_listener(listener).await.unwrap();
    });
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/probe/runs"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .unwrap();
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["status"], "completed");
    assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 0);
}
