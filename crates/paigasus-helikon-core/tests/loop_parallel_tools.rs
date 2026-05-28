//! AC #3: two parallel tool calls execute concurrently. Verified via
//! tokio::sync::Barrier — serial execution would deadlock the first
//! waiter; tokio::time::timeout surfaces that as a clear failure.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, FinishReason, Instructions, LlmAgent, ModelEvent, ModelSettings,
    RunConfig, RunResultStreaming,
};

use common::{noop_run_context, MockModel, MockToolBarrier};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_tool_calls_execute_concurrently() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let tool_a = MockToolBarrier::new("a", Arc::clone(&barrier));
    let tool_b = MockToolBarrier::new("b", Arc::clone(&barrier));

    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "1".into(),
                name: Some("a".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::ToolCallDelta {
                call_id: "2".into(),
                name: Some("b".into()),
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

    let agent = LlmAgent::<(), _> {
        name: "test".into(),
        description: "parallel test".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools: vec![
            tool_a as Arc<dyn paigasus_helikon_core::Tool<()>>,
            tool_b as Arc<dyn paigasus_helikon_core::Tool<()>>,
        ],
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    };

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .expect("agent.run should succeed");

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        RunResultStreaming::new(stream).collect(),
    )
    .await
    .expect("timeout — tools likely ran serially (Barrier deadlocked within 10 s)")
    .expect("collect should succeed");

    assert!(
        matches!(result.events.last(), Some(AgentEvent::RunCompleted { .. })),
        "expected RunCompleted as last event, got: {:?}",
        result.events.last(),
    );
}
