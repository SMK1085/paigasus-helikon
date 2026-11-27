//! SMA-864 — a cut model turn (`Length` / `ContentFilter`) never runs its tool
//! calls, also when the cut leaves the call arguments as partial JSON.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::{MockModel, MockTool};
use futures_util::StreamExt as _;
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken, FinishReason, HookRegistry,
    LlmAgent, MemorySession, ModelEvent, RunContext, RunError, RunResultStreaming, Session,
    SessionEvent, SessionRecorder, Tool, TracerHandle,
};

fn cut_call(args_delta: &str, reason: FinishReason) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            call_id: "c1".into(),
            name: Some("search".into()),
            args_delta: args_delta.into(),
        },
        ModelEvent::Finish { reason },
    ]
}

fn agent_for(script: Vec<ModelEvent>) -> (impl Agent<()>, Arc<MockTool>) {
    let tool = MockTool::new("search", serde_json::json!({}));
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(MockModel::with_scripts(vec![script]))
        .shared_tool(Arc::clone(&tool) as Arc<dyn Tool<()>>)
        .build();
    (agent, tool)
}

async fn run_error(agent: &impl Agent<()>) -> RunError {
    let ctx = RunContext::ephemeral(());
    let failure = ctx.failure_handle();
    let stream = agent
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .unwrap();
    RunResultStreaming::with_failure(stream, failure)
        .collect()
        .await
        .expect_err("the run must fail")
}

#[tokio::test]
async fn length_turn_with_empty_args_runs_no_tool() {
    let (agent, tool) = agent_for(cut_call("", FinishReason::Length));
    let err = run_error(&agent).await;
    assert!(tool.invocations().is_empty());
    assert!(
        matches!(
            err,
            RunError::Agent(AgentError::ToolCallsNotRun {
                finish_reason: Some(FinishReason::Length),
                ..
            })
        ),
        "got {err:?}"
    );
}

#[tokio::test]
async fn length_turn_with_partial_json_runs_no_tool() {
    let (agent, tool) = agent_for(cut_call("{\"q\": \"ab", FinishReason::Length));
    let err = run_error(&agent).await;
    assert!(tool.invocations().is_empty());
    match err {
        RunError::Agent(AgentError::ToolCallsNotRun {
            finish_reason: Some(FinishReason::Length),
            tool_names,
            ..
        }) => assert_eq!(tool_names, vec!["search".to_string()]),
        other => panic!("expected ToolCallsNotRun, got {other:?}"),
    }
}

#[tokio::test]
async fn content_filter_turn_with_partial_json_runs_no_tool() {
    let (agent, tool) = agent_for(cut_call("{\"q\": \"ab", FinishReason::ContentFilter));
    let err = run_error(&agent).await;
    assert!(tool.invocations().is_empty());
    assert!(
        matches!(
            err,
            RunError::Agent(AgentError::ToolCallsNotRun {
                finish_reason: Some(FinishReason::ContentFilter),
                ..
            })
        ),
        "got {err:?}"
    );
}

#[tokio::test]
async fn reassembly_error_under_stop_keeps_other() {
    let (agent, tool) = agent_for(cut_call("{\"q\": \"ab", FinishReason::Stop));
    let err = run_error(&agent).await;
    assert!(tool.invocations().is_empty());
    assert!(
        matches!(err, RunError::Agent(AgentError::Other(_))),
        "got {err:?}"
    );
}

#[tokio::test]
async fn cut_call_is_not_recorded_in_the_session() {
    let (agent, _tool) = agent_for(cut_call("", FinishReason::Length));
    let session = Arc::new(MemorySession::new());
    let ctx = RunContext::new(
        Arc::new(()),
        Arc::clone(&session) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );
    let mut recorder = SessionRecorder::new("root");
    let input = AgentInput::from_user_text("go");
    recorder.record_input(&input.messages);
    let events: Vec<AgentEvent> = agent.run(ctx, input).await.unwrap().collect().await;
    for ev in &events {
        recorder.observe(ev);
    }
    session.append(&recorder.drain()).await.unwrap();
    let log = session.events(None).await.unwrap();
    assert!(
        !log.iter().any(|e| matches!(
            e,
            SessionEvent::ToolCalled { .. } | SessionEvent::ToolReturned { .. }
        )),
        "{log:?}"
    );
}
