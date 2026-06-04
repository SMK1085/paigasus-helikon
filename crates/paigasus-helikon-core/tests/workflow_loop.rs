//! SMA-325 — LoopAgent: escalate exits; exhaustion fails.

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use common::{msg_and_complete, EscalatingTool, MockAgent, MockModel};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken, FinishReason, HookRegistry,
    LlmAgent, LoopAgent, MemorySession, ModelEvent, RunContext, RunError, RunResultStreaming,
    Session, Tool, TracerHandle,
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

#[tokio::test]
async fn escalate_stops_after_that_iteration() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let worker = MockAgent::new("worker", move |ctx| {
        let n = c.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= 2 {
            ctx.actions().escalate();
        }
        msg_and_complete("worker", &format!("iter {n}"), 0)
    });
    let la = LoopAgent::new("loop", "until escalate", 5).then(worker);

    let result = RunResultStreaming::new(
        la.run(ctx(), AgentInput::from_user_text("go"))
            .await
            .unwrap(),
    )
    .collect()
    .await
    .unwrap();

    let runs = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::AgentUpdated { agent } if agent == "worker"))
        .count();
    assert_eq!(runs, 2, "escalate on iteration 2 → exactly 2 runs");
    assert!(result
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::RunCompleted { .. })));
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::RunFailed { .. })));
}

#[tokio::test]
async fn exhausting_max_iterations_fails() {
    let worker = MockAgent::new("worker", |_| msg_and_complete("worker", "again", 0));
    let la = LoopAgent::new("loop", "never escalates", 3).then(worker);

    let ctx = ctx();
    let failure = ctx.failure_handle();
    let err = RunResultStreaming::with_failure(
        la.run(ctx, AgentInput::from_user_text("go")).await.unwrap(),
        failure,
    )
    .collect()
    .await
    .expect_err("exhausted");

    match err {
        RunError::Agent(AgentError::MaxIterationsExceeded { max }) => assert_eq!(max, 3),
        other => panic!("expected MaxIterationsExceeded, got {other:?}"),
    }
}

#[tokio::test]
async fn escalate_from_real_tool_stops_the_loop() {
    // The looped agent: turn 1 calls the "done" tool (which escalates); turn 2
    // emits final text.
    let worker = LlmAgent::builder::<()>()
        .name("worker")
        .description("does one unit of work")
        .shared_model(MockModel::with_scripts(vec![
            vec![
                ModelEvent::ToolCallDelta {
                    call_id: "c1".to_owned(),
                    name: Some("done".to_owned()),
                    args_delta: "{}".to_owned(),
                },
                ModelEvent::Finish {
                    reason: FinishReason::ToolCalls,
                },
            ],
            vec![
                ModelEvent::TokenDelta {
                    text: "finished".to_owned(),
                },
                ModelEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ],
        ]))
        .shared_tool(EscalatingTool::new("done") as Arc<dyn Tool<()>>)
        .build();

    let la = LoopAgent::new("refine", "loop until the tool escalates", 5).then(worker);

    let result = RunResultStreaming::new(
        la.run(ctx(), AgentInput::from_user_text("go"))
            .await
            .unwrap(),
    )
    .collect()
    .await
    .unwrap();

    let worker_runs = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::AgentUpdated { agent } if agent == "worker"))
        .count();
    assert_eq!(
        worker_runs, 1,
        "tool escalate stops after the first iteration"
    );
    assert!(result
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::RunCompleted { .. })));
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::RunFailed { .. })));
}
