//! SMA-324 — AgentAsTool: round-trip, isolation, depth guard.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::MockModel;
use paigasus_helikon_core::{
    Agent, AgentAsTool, AgentEvent, AgentInput, CancellationToken, FinishReason, HookRegistry,
    Item, LlmAgent, MemorySession, ModelEvent, RunContext, RunResultStreaming, Session, Tool,
    ToolContext, TracerHandle,
};

fn ctx() -> RunContext<()> {
    RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
}

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
    assert!(
        events.is_empty(),
        "sub-agent turns must not touch parent session"
    );
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

#[tokio::test]
async fn agent_as_tool_round_trips_through_parent_loop() {
    // Sub-agent: responds with "42" as its final output.
    let sub = LlmAgent::builder::<()>()
        .name("calculator")
        .description("Answers arithmetic.")
        .shared_model(MockModel::with_scripts(vec![text_turn("42")]))
        .build();

    // Parent: turn 1 calls the "calculator" tool; turn 2 emits the final answer.
    let parent_scripts = vec![
        // Turn 1: emit a tool call to "calculator".
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "c1".to_owned(),
                name: Some("calculator".to_owned()),
                args_delta: "{\"input\":\"6*7\"}".to_owned(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        // Turn 2: emit the final answer text.
        vec![
            ModelEvent::TokenDelta {
                text: "The answer is 42.".to_owned(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ];

    let parent = LlmAgent::builder::<()>()
        .name("parent")
        .shared_model(MockModel::with_scripts(parent_scripts))
        .shared_tool(Arc::new(AgentAsTool::new(sub)) as Arc<dyn Tool<()>>)
        .build();

    let stream = parent
        .run(ctx(), AgentInput::from_user_text("compute"))
        .await
        .expect("run starts");
    let result = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect("run completes");

    // The parent's final output is the turn-2 text, proving the loop resumed after
    // the sub-agent's tool result was injected.
    assert_eq!(
        result.final_output, "The answer is 42.",
        "parent final output must be the turn-2 text"
    );

    // At least one ToolOutputItem in the event stream must carry "42", confirming
    // the sub-agent's final_output flowed back as the tool result.
    let tool_result_carries_42 = result.events.iter().any(|ev| match ev {
        AgentEvent::ToolOutputItem {
            item: Item::ToolResult { content, .. },
        } => content.iter().any(|p| match p {
            paigasus_helikon_core::ContentPart::Text { text } => text.contains("42"),
            _ => false,
        }),
        _ => false,
    });
    assert!(
        tool_result_carries_42,
        "a ToolOutputItem must carry the sub-agent's '42' output"
    );
}
