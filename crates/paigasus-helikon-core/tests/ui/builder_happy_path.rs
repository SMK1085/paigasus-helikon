//! Full builder chain exercising every any-state setter at least
//! once, then `.model` and `.build`. Future signature drift on any
//! optional fails here.

use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken, Guardrail, GuardrailError,
    GuardrailInput, GuardrailVerdict, Hook, HookDecision, HookEvent, LlmAgent, Model,
    ModelCapabilities, ModelError, ModelEvent, ModelRequest, ModelSettings, RunContext, Tool,
    ToolContext, ToolError, ToolOutput,
};

struct MockModel;

#[async_trait::async_trait]
impl Model for MockModel {
    async fn invoke(
        &self,
        _r: ModelRequest,
        _c: CancellationToken,
    ) -> Result<
        futures_core::stream::BoxStream<'static, Result<ModelEvent, ModelError>>,
        ModelError,
    > {
        Err(ModelError::Unavailable)
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

struct MockTool;

#[async_trait::async_trait]
impl<Ctx> Tool<Ctx> for MockTool
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str { "mock" }
    fn description(&self) -> &str { "mock tool" }
    fn schema(&self) -> &serde_json::Value {
        static S: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        S.get_or_init(|| serde_json::json!({"type":"object"}))
    }
    async fn invoke(
        &self,
        _c: &ToolContext<Ctx>,
        _a: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(serde_json::Value::String("ok".into())))
    }
}

struct MockHandoff;

#[async_trait::async_trait]
impl Agent<()> for MockHandoff {
    fn name(&self) -> &str { "handoff-target" }
    fn description(&self) -> &str { "handoff target" }
    async fn run(
        &self,
        _ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<futures_core::stream::BoxStream<'static, AgentEvent>, AgentError> {
        unimplemented!()
    }
}

struct MockGuardrail;

#[async_trait::async_trait]
impl<Ctx> Guardrail<Ctx> for MockGuardrail
where
    Ctx: Send + Sync + 'static,
{
    async fn check(
        &self,
        _ctx: &RunContext<Ctx>,
        _input: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError> {
        Ok(GuardrailVerdict::Pass)
    }
}

struct MockHook;

#[async_trait::async_trait]
impl<Ctx> Hook<Ctx> for MockHook
where
    Ctx: Send + Sync + 'static,
{
    async fn on_event(&self, _ctx: &RunContext<Ctx>, _event: &HookEvent) -> HookDecision {
        HookDecision::Allow
    }
}

fn main() {
    let shared_tool: Arc<dyn Tool<()>> = Arc::new(MockTool);
    let _ = LlmAgent::builder::<()>()
        .description("comprehensive coverage")
        .instructions("you are helpful")
        .tool(MockTool)
        .tools(vec![Arc::new(MockTool) as Arc<dyn Tool<()>>])
        .shared_tool(shared_tool)
        .handoff(MockHandoff)
        .hook(MockHook)
        .input_guardrail(MockGuardrail)
        .output_guardrail(MockGuardrail)
        .model_settings(ModelSettings::default())
        .max_turns(8)
        .name("triage")
        .model(MockModel)
        .build();
}
