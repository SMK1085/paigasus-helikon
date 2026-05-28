//! AC #1: single-turn run on a fixture MockModel completes with
//! RunCompleted. AC #2 lock lives in the second test (multi-turn
//! with tool call).

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, FinishReason, Instructions, LlmAgent, ModelEvent, ModelSettings,
    OutputType, RunConfig, RunResultStreaming,
};

use common::{noop_run_context, MockModel};

fn build_agent<M>(model: Arc<M>) -> LlmAgent<(), M>
where
    M: paigasus_helikon_core::Model + 'static,
{
    LlmAgent {
        name: "test".into(),
        description: "test agent".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools: Vec::new(),
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
async fn single_turn_run_completes() {
    let model = MockModel::with_scripts(vec![vec![
        ModelEvent::TokenDelta {
            text: "hello".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]]);
    let agent = build_agent(model);
    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("hi"))
        .await
        .expect("agent.run should succeed");

    let result = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect("collect should succeed");

    assert_eq!(result.final_output, "hello");
    assert!(
        matches!(result.events.last(), Some(AgentEvent::RunCompleted { .. })),
        "expected RunCompleted as last event, got: {:?}",
        result.events.last(),
    );
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::TokenDelta { .. })),
        "expected at least one TokenDelta",
    );
    let _ = OutputType::from_schema::<String>; // ensure the import compiles
}

fn event_kind(ev: &AgentEvent) -> &'static str {
    match ev {
        AgentEvent::RunStarted { .. } => "RunStarted",
        AgentEvent::TurnStarted { .. } => "TurnStarted",
        AgentEvent::TokenDelta { .. } => "TokenDelta",
        AgentEvent::ReasoningDelta { .. } => "ReasoningDelta",
        AgentEvent::ToolCallDelta { .. } => "ToolCallDelta",
        AgentEvent::MessageOutput { .. } => "MessageOutput",
        AgentEvent::ToolCallItem { .. } => "ToolCallItem",
        AgentEvent::ToolOutputItem { .. } => "ToolOutputItem",
        AgentEvent::HandoffItem { .. } => "HandoffItem",
        AgentEvent::AgentUpdated { .. } => "AgentUpdated",
        AgentEvent::GuardrailTriggered { .. } => "GuardrailTriggered",
        AgentEvent::ApprovalRequested { .. } => "ApprovalRequested",
        AgentEvent::RunCompleted { .. } => "RunCompleted",
        AgentEvent::RunFailed { .. } => "RunFailed",
        _ => "Unknown",
    }
}

#[tokio::test]
async fn multi_turn_with_tool_call() {
    use common::MockTool;

    // Script: turn 0 = model emits one ToolCall; turn 1 = model emits final text.
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "1".into(),
                name: Some("echo".into()),
                args_delta: "{\"msg\":\"hi\"}".into(),
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
    let tool = MockTool::new("echo", serde_json::json!("ok"));
    let mut agent = build_agent(model);
    agent.tools = vec![tool.clone() as std::sync::Arc<dyn paigasus_helikon_core::Tool<()>>];

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .expect("agent.run should succeed");
    let result = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect("collect");

    assert_eq!(result.final_output, "done");
    assert_eq!(tool.invocations().len(), 1);

    let kinds: Vec<&'static str> = result.events.iter().map(event_kind).collect();
    insta::assert_yaml_snapshot!(kinds);
}
