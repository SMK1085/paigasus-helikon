//! Shared test helpers for the `paigasus-helikon-runtime-axum` integration tests.
//!
//! This module is compiled into every integration-test binary; not every helper
//! is used by every binary, so dead-code is allowed module-wide.
#![allow(dead_code)]

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunConfig, RunContext, RunError,
    RunResult, RunResultStreaming, Runner, TokenUsage,
};
use paigasus_helikon_runtime_axum::AgentServer;

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

/// Build an [`AgentServer`] mounting a single `echo` [`ScriptedAgent`], bind it
/// to an ephemeral loopback port, spawn the serve loop, and return the bound
/// address.
pub async fn spawn_echo_server() -> SocketAddr {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(ScriptedAgent {
            name: "echo".into(),
            events: echo_script(),
        }))
        .build()
        .expect("server builds");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        server
            .serve_with_listener(listener)
            .await
            .expect("serve loop");
    });

    addr
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
pub async fn create_async_run(addr: std::net::SocketAddr, agent_name: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/{agent_name}/runs?mode=async"))
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
