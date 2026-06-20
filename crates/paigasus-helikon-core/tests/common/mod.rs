//! Shared test fixtures for SMA-314 integration tests. Compiled once
//! per test binary via `#[path = "common/mod.rs"] mod common;` at the
//! top of each integration test file.

#![allow(dead_code, clippy::type_complexity)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;

use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken, ContentPart,
    ConversationSnapshot, Item, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
    RunContext, SequenceId, Session, SessionError, SessionEvent, TokenUsage, Tool, ToolContext,
    ToolError, ToolOutput,
};

/// A scripted [`Model`] that emits a pre-recorded sequence of
/// [`ModelEvent`]s per call to [`Model::invoke`]. Pop one script per
/// invocation; running out of scripts yields a `ModelError`.
pub struct MockModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl MockModel {
    pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(VecDeque::from(scripts)),
        })
    }
}

#[async_trait]
impl Model for MockModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ModelError::Other(anyhow::anyhow!("no more scripted responses")))?;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// A [`Tool`] that records every invocation and returns a static
/// `serde_json::Value` as its output.
pub struct MockTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    invocations: Mutex<Vec<(serde_json::Value, Instant)>>,
    output: serde_json::Value,
}

impl MockTool {
    pub fn new(name: &str, output: serde_json::Value) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            description: format!("mock tool {name}"),
            schema: serde_json::json!({"type": "object"}),
            invocations: Mutex::new(Vec::new()),
            output,
        })
    }

    pub fn invocations(&self) -> Vec<(serde_json::Value, Instant)> {
        self.invocations.lock().unwrap().clone()
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for MockTool
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }

    async fn invoke(
        &self,
        _ctx: &ToolContext<Ctx>,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.invocations
            .lock()
            .unwrap()
            .push((args, Instant::now()));
        Ok(ToolOutput::new(self.output.clone()))
    }
}

/// A [`Tool`] that synchronizes its invocations through a
/// [`tokio::sync::Barrier`]. Use with `Barrier::new(N)` and N tool
/// instances to verify concurrent execution: if the tools run
/// serially, the first invocation blocks forever waiting for the
/// second waiter.
pub struct MockToolBarrier {
    name: String,
    description: String,
    schema: serde_json::Value,
    barrier: Arc<tokio::sync::Barrier>,
}

impl MockToolBarrier {
    pub fn new(name: &str, barrier: Arc<tokio::sync::Barrier>) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            description: format!("barrier-synced mock tool {name}"),
            schema: serde_json::json!({"type": "object"}),
            barrier,
        })
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for MockToolBarrier
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }

    async fn invoke(
        &self,
        _ctx: &ToolContext<Ctx>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.barrier.wait().await;
        Ok(ToolOutput::new(serde_json::json!({"ok": true})))
    }
}

/// A no-op [`Session`] implementation. `append` discards;
/// `events` / `snapshot` return empty.
pub struct NoopSession;

#[async_trait]
impl Session for NoopSession {
    async fn append(&self, _events: &[SessionEvent]) -> Result<(), SessionError> {
        Ok(())
    }

    async fn events(&self, _since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        Ok(Vec::new())
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(ConversationSnapshot::default())
    }
}

/// Build a minimal [`RunContext`] suitable for integration tests.
pub fn noop_run_context<Ctx>() -> RunContext<Ctx>
where
    Ctx: Default + Send + Sync + 'static,
{
    RunContext::ephemeral(Ctx::default()).with_session(Arc::new(NoopSession))
}

use std::sync::atomic::{AtomicUsize, Ordering};

/// A [`Tool`] that tracks how many instances run concurrently. Each
/// invocation bumps `current`, records the running peak into `max`, yields
/// several times (so the scheduler can interleave peers), then decrements.
pub struct ConcurrencyProbe {
    name: String,
    description: String,
    schema: serde_json::Value,
    current: Arc<AtomicUsize>,
    max: Arc<AtomicUsize>,
}

impl ConcurrencyProbe {
    pub fn new(name: &str, current: Arc<AtomicUsize>, max: Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            description: format!("concurrency probe {name}"),
            schema: serde_json::json!({"type": "object"}),
            current,
            max,
        })
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for ConcurrencyProbe
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }

    async fn invoke(
        &self,
        _ctx: &ToolContext<Ctx>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(now, Ordering::SeqCst);
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        self.current.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::new(serde_json::json!({"ok": true})))
    }
}

/// Build a `TokenUsage` with `input_tokens == total_tokens == total` (the other
/// fields zero). Constructed via `default()` + field assignment because
/// `TokenUsage` is `#[non_exhaustive]` (no struct-literal construction off-crate).
pub fn usage_total(total: u64) -> TokenUsage {
    let mut u = TokenUsage::default();
    u.input_tokens = total;
    u.total_tokens = total;
    u
}

/// An `AgentEvent::MessageOutput` carrying an assistant text message.
pub fn assistant_msg(agent: &str, text: &str) -> AgentEvent {
    AgentEvent::MessageOutput {
        item: Item::AssistantMessage {
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
            agent: Some(agent.to_owned()),
        },
    }
}

/// The canonical "ran and finished" event sequence: `RunStarted`, one
/// `MessageOutput` with `text`, then `RunCompleted` carrying `usage_total(total)`.
pub fn msg_and_complete(agent: &str, text: &str, total: u64) -> Vec<AgentEvent> {
    vec![
        AgentEvent::RunStarted {
            agent: agent.to_owned(),
        },
        assistant_msg(agent, text),
        AgentEvent::RunCompleted {
            usage: usage_total(total),
        },
    ]
}

/// A scripted [`Agent`]: its `run` evaluates `behavior(&ctx)` once (which may read
/// `ctx.state()`, call `ctx.actions().escalate()`, or set `ctx.failure_handle()`),
/// then streams the returned events. No model required.
pub struct MockAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    name: String,
    description: String,
    behavior: Arc<dyn Fn(&RunContext<Ctx>) -> Vec<AgentEvent> + Send + Sync>,
}

impl<Ctx> MockAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    pub fn new(
        name: &str,
        behavior: impl Fn(&RunContext<Ctx>) -> Vec<AgentEvent> + Send + Sync + 'static,
    ) -> MockAgent<Ctx> {
        MockAgent {
            name: name.to_owned(),
            description: format!("mock agent {name}"),
            behavior: Arc::new(behavior),
        }
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for MockAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let events = (self.behavior)(&ctx);
        Ok(Box::pin(stream::iter(events)))
    }
}

/// A [`Tool`] that calls `ctx.actions().escalate()` and returns `{"escalated": true}`.
pub struct EscalatingTool {
    name: String,
    schema: serde_json::Value,
}

impl EscalatingTool {
    pub fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            schema: serde_json::json!({"type": "object"}),
        })
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for EscalatingTool
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "Signals the enclosing loop to stop."
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    async fn invoke(
        &self,
        ctx: &ToolContext<Ctx>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        ctx.actions().escalate();
        Ok(ToolOutput::new(serde_json::json!({"escalated": true})))
    }
}
