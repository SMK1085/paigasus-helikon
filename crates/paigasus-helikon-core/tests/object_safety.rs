//! Locks acceptance criterion #2 of SMA-312: every object-safe trait can
//! be held behind `Box<dyn _>` and `Vec<Arc<dyn _>>`. A compile failure
//! here is an AC regression.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures_core::stream::{BoxStream, Stream};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken, ConversationSnapshot, Guardrail,
    GuardrailError, GuardrailInput, GuardrailVerdict, Hook, HookDecision, HookEvent, Model,
    ModelCapabilities, ModelError, ModelEvent, ModelRequest, RunConfig, RunContext, RunError,
    RunResult, RunResultStreaming, Runner, SequenceId, Session, SessionError, SessionEvent, Tool,
    ToolContext, ToolError, ToolOutput,
};
use serde_json::{json, Value};

/// An empty stream over `AgentEvent`. Inline because `futures-core` 0.3
/// only exposes the `Stream` trait, not constructors like `empty()`.
struct EmptyAgentEvents;

impl Stream for EmptyAgentEvents {
    type Item = AgentEvent;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(None)
    }
}

struct NoopModel;

#[async_trait]
impl Model for NoopModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        Err(ModelError::Unavailable)
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

struct NoopTool {
    schema: Value,
}

#[async_trait]
impl Tool<()> for NoopTool {
    fn name(&self) -> &str {
        "noop"
    }
    fn description(&self) -> &str {
        "Does nothing."
    }
    fn schema(&self) -> &Value {
        &self.schema
    }

    async fn invoke(&self, _ctx: &ToolContext<()>, _args: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::default())
    }
}

struct NoopSession;

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

struct NoopGuardrail;

#[async_trait]
impl Guardrail<()> for NoopGuardrail {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        _input: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError> {
        Ok(GuardrailVerdict::Pass)
    }
}

struct NoopHook;

#[async_trait]
impl Hook<()> for NoopHook {
    async fn on_event(&self, _ctx: &RunContext<()>, _event: &HookEvent) -> HookDecision {
        HookDecision::Allow
    }
}

struct NoopAgent;

#[async_trait]
impl Agent<()> for NoopAgent {
    fn name(&self) -> &str {
        "noop"
    }
    fn description(&self) -> &str {
        "Does nothing."
    }

    async fn run(
        &self,
        _ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        Ok(Box::pin(EmptyAgentEvents))
    }
}

struct NoopRunner;

#[async_trait]
impl Runner<()> for NoopRunner {
    async fn run(
        &self,
        _agent: &(dyn Agent<()> + '_),
        _ctx: RunContext<()>,
        _input: AgentInput,
        _config: RunConfig,
    ) -> Result<RunResult, RunError> {
        Ok(RunResult::default())
    }

    async fn run_streamed(
        &self,
        _agent: &(dyn Agent<()> + '_),
        _ctx: RunContext<()>,
        _input: AgentInput,
        _config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        Ok(RunResultStreaming::default())
    }
}

#[test]
fn trait_objects_construct() {
    let _: Box<dyn Model> = Box::new(NoopModel);

    let _: Vec<Arc<dyn Tool<()>>> = vec![Arc::new(NoopTool {
        schema: json!({ "type": "object" }),
    })];

    let _: Box<dyn Session> = Box::new(NoopSession);
    let _: Box<dyn Guardrail<()>> = Box::new(NoopGuardrail);
    let _: Box<dyn Hook<()>> = Box::new(NoopHook);
    let _: Box<dyn Agent<()>> = Box::new(NoopAgent);
    let _: Box<dyn Runner<()>> = Box::new(NoopRunner);
}
