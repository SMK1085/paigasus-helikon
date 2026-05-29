//! SMA-346: LlmAgent::run records the structured AgentError into the
//! RunContext failure slot at every terminal-failure pathway.

#[path = "common/mod.rs"]
mod common;

use common::{MockModel, MockTool};
use futures_util::stream::StreamExt as _;
use paigasus_helikon_core::{
    Agent, AgentError, AgentInput, CancellationToken, HookRegistry, LlmAgent, RunContext, Session,
    TracerHandle,
};
use std::sync::Arc;

// A no-op session local to this test (common::NoopSession is also available).
use common::NoopSession;

fn ctx() -> RunContext<()> {
    RunContext::new(
        Arc::new(()),
        Arc::new(NoopSession) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
}

/// Drain a raw agent event stream to exhaustion (discarding events).
async fn drain(
    mut stream: futures_core::stream::BoxStream<'static, paigasus_helikon_core::AgentEvent>,
) {
    while stream.next().await.is_some() {}
}

#[tokio::test]
async fn model_invoke_error_is_recorded_as_agenterror_model() {
    // Empty scripts => MockModel::invoke returns Err(ModelError::Other(..)).
    let model = MockModel::with_scripts(vec![]);
    let agent = LlmAgent::builder::<()>()
        .name("t")
        .shared_model(model)
        .instructions("go")
        .build();

    let ctx = ctx();
    let failure = ctx.failure_handle();
    let stream = agent
        .run(ctx, AgentInput::from_user_text("hi"))
        .await
        .expect("run starts");
    drain(stream).await;

    assert!(
        matches!(failure.take(), Some(AgentError::Model(_))),
        "model.invoke failure should land in the slot as AgentError::Model"
    );
}

#[tokio::test]
async fn max_turns_exceeded_is_recorded_as_structured_error() {
    use paigasus_helikon_core::{FinishReason, ModelEvent};
    // Turn 0 emits a tool call; after the tool runs, next_turn (1) >= max_turns
    // (1) => the state machine fails with MaxTurnsExceeded and Terminates.
    let model = MockModel::with_scripts(vec![vec![
        ModelEvent::ToolCallDelta {
            call_id: "c1".into(),
            name: Some("noop".into()),
            args_delta: "{}".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]]);
    let tool = MockTool::new("noop", serde_json::json!({"ok": true}));
    let agent = LlmAgent::builder::<()>()
        .name("t")
        .shared_model(model)
        .instructions("go")
        .shared_tool(tool)
        .max_turns(1)
        .build();

    let ctx = ctx();
    let failure = ctx.failure_handle();
    let stream = agent
        .run(ctx, AgentInput::from_user_text("hi"))
        .await
        .expect("run starts");
    drain(stream).await;

    match failure.take() {
        Some(AgentError::MaxTurnsExceeded(n)) => assert_eq!(n, 1),
        other => panic!("expected MaxTurnsExceeded(1) in slot, got {other:?}"),
    }
}

#[tokio::test]
async fn build_items_parse_error_is_recorded_as_agenterror_other() {
    use paigasus_helikon_core::{FinishReason, ModelEvent};
    // A tool call whose accumulated args are not valid JSON ("{") makes
    // build_items fail; that String error is recorded as AgentError::Other.
    let model = MockModel::with_scripts(vec![vec![
        ModelEvent::ToolCallDelta {
            call_id: "c1".into(),
            name: Some("noop".into()),
            args_delta: "{".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]]);
    let agent = LlmAgent::builder::<()>()
        .name("t")
        .shared_model(model)
        .instructions("go")
        .build();

    let ctx = ctx();
    let failure = ctx.failure_handle();
    let stream = agent
        .run(ctx, AgentInput::from_user_text("hi"))
        .await
        .expect("run starts");
    drain(stream).await;

    assert!(
        matches!(failure.take(), Some(AgentError::Other(_))),
        "build_items parse failure should be recorded as AgentError::Other"
    );
}
