//! SMA-326: lifecycle hooks.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use async_trait::async_trait;
use paigasus_helikon_core::{
    Agent, AgentInput, FinishReason, Hook, HookDecision, HookEvent, Instructions, LlmAgent,
    ModelEvent, ModelSettings, RunConfig, RunContext, RunResultStreaming, Tool,
};

use common::{noop_run_context, MockModel, MockTool};

/// A hook that rewrites `PreToolUse` args to `{"replaced": true}`.
struct ReplaceArgs;
#[async_trait]
impl Hook<()> for ReplaceArgs {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        match event {
            HookEvent::PreToolUse { .. } => HookDecision::ReplaceInput {
                value: serde_json::json!({"replaced": true}),
            },
            _ => HookDecision::Allow,
        }
    }
}

/// A hook that records the `output` value it observes on `PostToolUse`.
struct RecordPostOutput(Arc<std::sync::Mutex<Option<serde_json::Value>>>);
#[async_trait]
impl Hook<()> for RecordPostOutput {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        if let HookEvent::PostToolUse { output, .. } = event {
            *self.0.lock().unwrap() = Some(output.clone());
        }
        HookDecision::Allow
    }
}

fn agent(
    model: Arc<MockModel>,
    tools: Vec<Arc<dyn Tool<()>>>,
    hooks: Vec<Arc<dyn Hook<()>>>,
) -> LlmAgent<(), MockModel> {
    LlmAgent::<(), _> {
        name: "h".into(),
        description: "hook test".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools,
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks,
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    }
}

#[tokio::test]
async fn pre_tool_use_replace_input_modifies_invocation() {
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "1".into(),
                name: Some("t".into()),
                args_delta: "{\"original\":true}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            ModelEvent::TokenDelta {
                text: "done".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);
    let tool = MockTool::new("t", serde_json::json!({"ok": true}));
    let agent = agent(
        model,
        vec![Arc::clone(&tool) as Arc<dyn Tool<()>>],
        vec![Arc::new(ReplaceArgs) as Arc<dyn Hook<()>>],
    );

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .unwrap();
    let _ = RunResultStreaming::new(stream).collect().await.unwrap();

    let invocations = tool.invocations();
    assert_eq!(invocations.len(), 1);
    assert_eq!(
        invocations[0].0,
        serde_json::json!({"replaced": true}),
        "AC2: args replaced"
    );
}

#[tokio::test]
async fn post_tool_use_sees_structured_json_output() {
    let seen = Arc::new(std::sync::Mutex::new(None));
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "1".into(),
                name: Some("t".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            ModelEvent::TokenDelta {
                text: "done".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);
    // The tool returns a structured object; the PostToolUse hook must see it
    // as JSON, not a stringified form.
    let tool = MockTool::new("t", serde_json::json!({"k": "v", "n": 1}));
    let agent = agent(
        model,
        vec![Arc::clone(&tool) as Arc<dyn Tool<()>>],
        vec![Arc::new(RecordPostOutput(Arc::clone(&seen))) as Arc<dyn Hook<()>>],
    );

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .unwrap();
    let _ = RunResultStreaming::new(stream).collect().await.unwrap();

    let got = seen.lock().unwrap().clone().expect("PostToolUse fired");
    assert_eq!(
        got,
        serde_json::json!({"k": "v", "n": 1}),
        "PostToolUse receives the tool's structured JSON, not a stringified form"
    );
}
