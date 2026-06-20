//! Shared mocks for TokioRunner integration tests.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;

use paigasus_helikon_core::{
    CancellationToken, ConversationSnapshot, Hook, HookDecision, HookEvent, HookRegistry,
    Instructions, LlmAgent, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
    ModelSettings, RunConfig, RunContext, SequenceId, Session, SessionError, SessionEvent, Tool,
    ToolContext, ToolError, ToolOutput,
};

/// Scripted model: one `Vec<ModelEvent>` per `invoke`; empty queue => error.
pub struct MockModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl MockModel {
    pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(VecDeque::from(scripts)),
        })
    }

    /// One quick assistant turn: "hi" then stop.
    pub fn quick_hi() -> Arc<Self> {
        Self::with_scripts(vec![vec![
            ModelEvent::TokenDelta { text: "hi".into() },
            ModelEvent::Finish {
                reason: paigasus_helikon_core::FinishReason::Stop,
            },
        ]])
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

/// A model whose response stream never completes — for cancellation/timeout.
pub struct PendingModel;

#[async_trait]
impl Model for PendingModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        Ok(Box::pin(stream::pending::<Result<ModelEvent, ModelError>>()))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// On `OnRunComplete`: cancel the run from inside the hook, then suspend. The
/// agent yields the terminal event BEFORE firing `OnRunComplete`, so this
/// deterministically reproduces the "terminal already out, cancel fires during
/// the post-terminal hook await" window (SMA-421) in a single synchronous poll —
/// no sleeps, no timing races. The suspended hook is dropped when the cancel
/// tears down the agent stream.
pub struct CancelOnRunCompleteHook;

#[async_trait]
impl<Ctx> Hook<Ctx> for CancelOnRunCompleteHook
where
    Ctx: Send + Sync + 'static,
{
    async fn on_event(&self, ctx: &RunContext<Ctx>, event: &HookEvent) -> HookDecision {
        if matches!(event, HookEvent::OnRunComplete) {
            ctx.cancel().cancel();
            // Suspend until the cancel tears down the stream; the `Allow` below is
            // unreachable on this branch.
            std::future::pending::<()>().await;
        }
        HookDecision::Allow
    }
}

/// Barrier-synced tool: N instances on a `Barrier::new(N)` deadlock unless
/// they run concurrently.
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
            description: format!("barrier tool {name}"),
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

/// Session that counts `append` calls — lets tests assert `finalize` ran.
#[derive(Default)]
pub struct CountingSession {
    appends: AtomicUsize,
}

impl CountingSession {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn append_count(&self) -> usize {
        self.appends.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Session for CountingSession {
    async fn append(&self, _events: &[SessionEvent]) -> Result<(), SessionError> {
        self.appends.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn events(&self, _since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        Ok(Vec::new())
    }
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(ConversationSnapshot::default())
    }
}

/// No-op session.
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

pub fn noop_run_context() -> RunContext<()> {
    RunContext::ephemeral(()).with_session(Arc::new(NoopSession))
}

pub fn run_context_with_cancel(cancel: CancellationToken) -> RunContext<()> {
    RunContext::ephemeral(())
        .with_session(Arc::new(NoopSession))
        .with_cancel(cancel)
}

pub fn run_context_with_cancel_and_hooks(
    cancel: CancellationToken,
    hooks: Vec<Arc<dyn Hook<()>>>,
) -> RunContext<()> {
    let mut registry = HookRegistry::new();
    for h in hooks {
        registry.push(h);
    }
    RunContext::ephemeral(())
        .with_session(Arc::new(NoopSession))
        .with_hooks(registry)
        .with_cancel(cancel)
}

pub fn run_context_with_session(session: Arc<dyn Session>) -> RunContext<()> {
    RunContext::ephemeral(()).with_session(session)
}

pub fn run_context_with_session_and_cancel(
    session: Arc<dyn Session>,
    cancel: CancellationToken,
) -> RunContext<()> {
    RunContext::ephemeral(())
        .with_session(session)
        .with_cancel(cancel)
}

/// Build an `LlmAgent<(), M>` with the given model and tools.
pub fn text_agent<M: Model + 'static>(
    model: Arc<M>,
    tools: Vec<Arc<dyn Tool<()>>>,
) -> LlmAgent<(), M> {
    LlmAgent::<(), _> {
        name: "test".into(),
        description: "test agent".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools,
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    }
}
