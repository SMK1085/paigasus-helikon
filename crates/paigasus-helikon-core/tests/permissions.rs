//! SMA-326: permission gating.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, FinishReason, Instructions, LlmAgent, ModelEvent, ModelSettings,
    PermissionMode, RunConfig, RunResultStreaming, Tool,
};

use common::{noop_run_context, MockModel, MockTool};

fn agent(model: Arc<MockModel>, tools: Vec<Arc<dyn Tool<()>>>) -> LlmAgent<(), MockModel> {
    LlmAgent::<(), _> {
        name: "p".into(),
        description: "permission test".into(),
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

#[tokio::test]
async fn plan_mode_denies_side_effecting_tool() {
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "1".into(),
                name: Some("writer".into()),
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
    let tool = MockTool::new("writer", serde_json::json!({"ok": true}));
    let agent = agent(model, vec![Arc::clone(&tool) as Arc<dyn Tool<()>>]);

    let ctx = noop_run_context::<()>().with_permission_mode(PermissionMode::Plan);
    let stream = agent
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .unwrap();
    let result = RunResultStreaming::new(stream).collect().await.unwrap();

    assert_eq!(
        tool.invocations().len(),
        0,
        "Plan denied the side-effecting tool (AC3)"
    );
    assert!(result
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::PermissionDenied { tool, .. } if tool == "writer")));
}
