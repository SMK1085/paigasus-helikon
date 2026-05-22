//! AC #1: single-turn run on a fixture MockModel completes with
//! RunCompleted. AC #2 lock lives in the second test (multi-turn
//! with tool call).

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, FinishReason, Instructions, LlmAgent,
    ModelEvent, ModelSettings, OutputType, RunConfig, RunResultStreaming,
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
    }
}

#[tokio::test]
async fn single_turn_run_completes() {
    let model = MockModel::with_scripts(vec![vec![
        ModelEvent::TokenDelta { text: "hello".into() },
        ModelEvent::Finish { reason: FinishReason::Stop },
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
        "expected RunCompleted as last event, got: {:?}", result.events.last(),
    );
    assert!(
        result.events.iter().any(|e| matches!(e, AgentEvent::TokenDelta { .. })),
        "expected at least one TokenDelta",
    );
    let _ = OutputType::from_schema::<String>; // ensure the import compiles
}
