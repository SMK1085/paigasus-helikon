//! Shared test helpers for the `paigasus-helikon-runtime-actix` integration tests.
//!
//! This module is compiled into every integration-test binary; not every helper
//! is used by every binary, so dead-code is allowed module-wide.
#![allow(dead_code)]

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunConfig, RunContext, RunError,
    RunResult, RunResultStreaming, Runner, TokenUsage,
};
use paigasus_helikon_runtime_actix::AgentServer;

/// A test [`Agent`] that emits a fixed sequence of events rather than
/// talking to any real model.
pub struct ScriptedAgent {
    /// Agent name returned by [`Agent::name`].
    pub name: String,
    /// Events to emit on each [`Agent::run`] call.
    pub events: Vec<AgentEvent>,
}

#[async_trait]
impl<Ctx: Send + Sync + 'static> Agent<Ctx> for ScriptedAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "scripted test agent"
    }

    async fn run(
        &self,
        _ctx: RunContext<Ctx>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        Ok(stream::iter(self.events.clone()).boxed())
    }
}

/// Returns a minimal event sequence: one assistant "echo" message followed by
/// [`AgentEvent::RunCompleted`].
pub fn echo_script() -> Vec<AgentEvent> {
    vec![
        AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: vec![ContentPart::Text {
                    text: "echo".to_owned(),
                }],
                agent: None,
            },
        },
        AgentEvent::RunCompleted {
            usage: TokenUsage::default(),
        },
    ]
}

/// Spawn `server` on a dedicated OS thread driving its own single-threaded
/// actix [`actix_web::rt::System`], and return the bound base URL
/// (`http://127.0.0.1:<port>`).
///
/// Unlike the tokio-runtime-axum harness — which spawns the serve loop as a
/// task on the caller's existing tokio runtime — actix-web owns its own
/// (non-`Send`) per-worker runtime, so the serve loop must be driven from a
/// `System` created on a thread of its own; the calling test's `#[tokio::test]`
/// runtime plays no part in it.
pub fn spawn_actix_server<Ctx: Send + Sync + 'static>(server: AgentServer<Ctx>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        actix_web::rt::System::new().block_on(async move {
            server.serve_with_listener(listener).await.expect("serve");
        });
    });
    // Brief readiness wait: the accept loop starts asynchronously on the
    // spawned thread, so give it a moment before the first connection attempt.
    std::thread::sleep(std::time::Duration::from_millis(200));
    format!("http://{addr}")
}

/// Build an [`AgentServer`] mounting a single `echo` [`ScriptedAgent`] and
/// spawn it via [`spawn_actix_server`].
pub fn spawn_echo_server() -> String {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(ScriptedAgent {
            name: "echo".into(),
            events: echo_script(),
        }))
        .build()
        .expect("server builds");
    spawn_actix_server(server)
}

/// Parse the `data:` lines of a Server-Sent-Events body back into a
/// `Vec<AgentEvent>`, in order. Non-`data:` lines (blank separators, `event:`
/// type tags) are ignored.
pub fn parse_sse(text: &str) -> Vec<AgentEvent> {
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty())
        .map(|data| serde_json::from_str::<AgentEvent>(data).expect("valid AgentEvent JSON"))
        .collect()
}

/// Create an async run via `POST /agents/{name}/runs?mode=async` and return the
/// run id as a `String`.
///
/// Takes `base` (as returned by [`spawn_actix_server`]/[`spawn_echo_server`])
/// rather than a `SocketAddr`, since the actix harness hands back a base URL.
pub async fn create_async_run(base: &str, agent_name: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{base}/agents/{agent_name}/runs?mode=async"))
        .header("content-type", "application/json")
        .body(r#"{"input":"test"}"#)
        .send()
        .await
        .expect("async run request");
    assert_eq!(resp.status(), 202, "expected 202 Accepted");
    let v: serde_json::Value = resp.json().await.expect("async run response body");
    v["run_id"]
        .as_str()
        .expect("run_id field in response")
        .to_owned()
}

/// Parse a JSON text string (received from a WebSocket frame) into an [`AgentEvent`].
pub fn parse_event(text: &str) -> AgentEvent {
    serde_json::from_str(text).expect("valid AgentEvent JSON")
}

// ── FailingRunner ──────────────────────────────────────────────────────────────

/// A test [`Runner`] whose `run_streamed` returns `Err` immediately, simulating
/// an agent that fails before emitting any event.
pub struct FailingRunner;

#[async_trait]
impl<Ctx: Send + Sync + 'static> Runner<Ctx> for FailingRunner {
    async fn run(
        &self,
        _agent: &(dyn Agent<Ctx> + '_),
        _ctx: RunContext<Ctx>,
        _input: AgentInput,
        _config: RunConfig,
    ) -> Result<RunResult, RunError> {
        Err(RunError::MaxIterations)
    }

    async fn run_streamed(
        &self,
        _agent: &(dyn Agent<Ctx> + '_),
        _ctx: RunContext<Ctx>,
        _input: AgentInput,
        _config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        Err(RunError::MaxIterations)
    }
}

// ── PartialThenEndRunner ────────────────────────────────────────────────────────

/// A test [`Runner`] whose `run_streamed` succeeds and yields exactly one
/// non-terminal event (`TokenDelta { "hi" }`), then ends the stream WITHOUT a
/// terminal `RunCompleted`/`RunFailed`. Exercises the streaming transports'
/// synthetic-terminal-frame path for a run that produced real events first, so
/// `saw_terminal` must stay false and the generic message is used.
pub struct PartialThenEndRunner;

#[async_trait]
impl<Ctx: Send + Sync + 'static> Runner<Ctx> for PartialThenEndRunner {
    async fn run(
        &self,
        _agent: &(dyn Agent<Ctx> + '_),
        _ctx: RunContext<Ctx>,
        _input: AgentInput,
        _config: RunConfig,
    ) -> Result<RunResult, RunError> {
        unimplemented!("PartialThenEndRunner is only used through run_streamed")
    }

    async fn run_streamed(
        &self,
        _agent: &(dyn Agent<Ctx> + '_),
        _ctx: RunContext<Ctx>,
        _input: AgentInput,
        _config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        let stream = stream::iter(vec![AgentEvent::TokenDelta {
            text: "hi".to_owned(),
        }])
        .boxed();
        Ok(RunResultStreaming::new(stream))
    }
}

// ── OrderingAgent ─────────────────────────────────────────────────────────────

/// Tick byte pushed by [`OrderingAgent`] when a run **starts** (before the first
/// event is returned).
pub const TICK_START: u8 = 0;

/// Tick byte pushed by [`OrderingAgent`] when a run **ends** (just before the
/// terminal event is returned).
pub const TICK_END: u8 = 1;

/// A test [`Agent`] that records start/end tick bytes into a shared buffer and
/// sleeps briefly between them.
///
/// Used by `concurrent_same_session_serialize` to verify that two concurrent
/// one-shot requests with the same `X-Session-Id` are fully serialized: the
/// expected tick sequence is `[TICK_START, TICK_END, TICK_START, TICK_END]`.
pub struct OrderingAgent {
    /// Agent name returned by [`Agent::name`].
    pub name: String,
    /// Shared tick log; each run appends `[TICK_START, TICK_END]`.
    pub ticks: Arc<Mutex<Vec<u8>>>,
}

#[async_trait]
impl<Ctx: Send + Sync + 'static> Agent<Ctx> for OrderingAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "ordering test agent"
    }

    async fn run(
        &self,
        _ctx: RunContext<Ctx>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        // Record start tick — happens in the writer task, under the session lock.
        self.ticks.lock().unwrap().push(TICK_START);
        // Sleep briefly so the writer task holds the session lock long enough
        // for a concurrent same-session request to block on it before we finish.
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Record end tick — still inside the writer task, still under the session lock.
        self.ticks.lock().unwrap().push(TICK_END);
        Ok(stream::iter(vec![AgentEvent::RunCompleted {
            usage: TokenUsage::default(),
        }])
        .boxed())
    }
}

// ── SignallingHangingAgent ──────────────────────────────────────────────────────

/// A test [`Agent`] that signals on `started` from its FIRST stream element,
/// then hangs for 30s before it would emit `RunCompleted`.
///
/// Used to model a client that walks away mid-run. The signal exists because a
/// one-shot client receives nothing until the run ends, so there is no frame to
/// key a disconnect off and a fixed sleep would race the run's start.
pub struct SignallingHangingAgent {
    /// Fires once, from the first stream element, when the run has started.
    pub started: tokio::sync::mpsc::UnboundedSender<()>,
}

#[async_trait]
impl<Ctx: Send + Sync + 'static> Agent<Ctx> for SignallingHangingAgent {
    fn name(&self) -> &str {
        "hanging"
    }

    fn description(&self) -> &str {
        "test agent that signals run start then hangs"
    }

    async fn run(
        &self,
        _ctx: RunContext<Ctx>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let started = self.started.clone();
        let first = stream::once(async move {
            let _ = started.send(());
            AgentEvent::RunStarted {
                agent: "hanging".to_owned(),
            }
        });
        let hangs = stream::once(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            }
        });
        Ok(first.chain(hangs).boxed())
    }
}
