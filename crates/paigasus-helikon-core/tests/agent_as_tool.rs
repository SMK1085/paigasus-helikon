//! SMA-324 — AgentAsTool: round-trip, isolation, depth guard.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::MockModel;
use paigasus_helikon_core::{
    AgentAsTool, CancellationToken, FinishReason, HookRegistry, LlmAgent, MemorySession,
    ModelEvent, RunContext, Session, Tool, ToolContext, TracerHandle,
};

fn text_turn(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta {
            text: text.to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

#[tokio::test]
async fn agent_as_tool_round_trips_final_output() {
    let sub = LlmAgent::builder::<()>()
        .name("calculator")
        .description("Answers arithmetic.")
        .shared_model(MockModel::with_scripts(vec![text_turn("42")]))
        .build();
    let tool = AgentAsTool::new(sub);

    let tc: ToolContext<()> = ToolContext::new(
        Arc::new(()),
        TracerHandle::default(),
        CancellationToken::new(),
        0,
        8,
    );
    let out = tool
        .invoke(&tc, serde_json::json!({ "input": "what is 6*7?" }))
        .await
        .expect("invoke ok");
    assert_eq!(out.content, serde_json::Value::String("42".to_owned()));
}

#[tokio::test]
async fn agent_as_tool_isolates_session() {
    let parent_session = Arc::new(MemorySession::new());
    let sub = LlmAgent::builder::<()>()
        .name("sub")
        .shared_model(MockModel::with_scripts(vec![text_turn("done")]))
        .build();
    let tool = AgentAsTool::new(sub);

    let run_ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        parent_session.clone() as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );
    let tc = run_ctx.to_tool_context();
    let _ = tool
        .invoke(&tc, serde_json::json!({ "input": "go" }))
        .await
        .expect("invoke ok");

    let events = parent_session.events(None).await.expect("events");
    assert!(events.is_empty(), "sub-agent turns must not touch parent session");
}

#[tokio::test]
async fn agent_as_tool_depth_guard_trips_on_cycle() {
    let sub = LlmAgent::builder::<()>()
        .name("sub")
        .shared_model(MockModel::with_scripts(vec![]))
        .build();
    let tool = AgentAsTool::new(sub);

    // agent_depth == max_agent_depth → depth+1 > max → reject without running.
    let tc: ToolContext<()> = ToolContext::new(
        Arc::new(()),
        TracerHandle::default(),
        CancellationToken::new(),
        8,
        8,
    );
    let err = tool
        .invoke(&tc, serde_json::json!({ "input": "go" }))
        .await
        .expect_err("depth guard refuses");
    assert!(err.to_string().contains("nesting depth"), "got: {err}");
}

#[tokio::test]
async fn agent_as_tool_sub_failure_becomes_tool_error() {
    let sub = LlmAgent::builder::<()>()
        .name("sub")
        .shared_model(MockModel::with_scripts(vec![])) // empty → model errors when run
        .build();
    let tool = AgentAsTool::new(sub);

    // Depth 0/8 → guard passes, so the sub-agent actually runs and fails.
    let tc: ToolContext<()> = ToolContext::new(
        Arc::new(()),
        TracerHandle::default(),
        CancellationToken::new(),
        0,
        8,
    );
    let err = tool
        .invoke(&tc, serde_json::json!({ "input": "go" }))
        .await
        .expect_err("sub-agent failure surfaces as a tool error");
    // Distinct from the depth-guard path: this must NOT be a nesting-depth error.
    assert!(
        !err.to_string().contains("nesting depth"),
        "should be a sub-run failure, not the depth guard: {err}"
    );
}
