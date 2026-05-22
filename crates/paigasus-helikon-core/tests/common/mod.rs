//! Shared test fixtures for SMA-314 integration tests. Compiled once
//! per test binary via `#[path = "common/mod.rs"] mod common;` at the
//! top of each integration test file.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use std::time::Instant;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;

use paigasus_helikon_core::{
    CancellationToken, ConversationSnapshot, HookRegistry, Model, ModelCapabilities, ModelError,
    ModelEvent, ModelRequest, RunContext, Session, SessionError, SessionEvent, SequenceId,
    Tool, ToolContext, ToolError, ToolOutput, TracerHandle,
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
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn schema(&self) -> &serde_json::Value { &self.schema }

    async fn invoke(
        &self,
        _ctx: &ToolContext<Ctx>,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.invocations.lock().unwrap().push((args, Instant::now()));
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
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn schema(&self) -> &serde_json::Value { &self.schema }

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
    RunContext::new(
        Arc::new(Ctx::default()),
        Arc::new(NoopSession) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
}
