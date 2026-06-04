//! SMA-325 — ParallelAgent: concurrent branches, disjoint state keys,
//! deterministic final_output, collect-all failure.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::{msg_and_complete, MockAgent};
use futures_util::StreamExt as _;
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken, HookRegistry, MemorySession,
    ParallelAgent, RunContext, RunError, RunResultStreaming, Session, TracerHandle,
};
use serde_json::json;

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
async fn writes_disjoint_keys_sums_usage_deterministic_output() {
    let pa = ParallelAgent::new("fetch", "fetch A and B")
        .add(MockAgent::new("fetchA", |_| {
            msg_and_complete("fetchA", "data-A", 3)
        }))
        .add(MockAgent::new("fetchB", |_| {
            msg_and_complete("fetchB", "data-B", 4)
        }));

    let ctx = ctx();
    let state = ctx.state().clone();
    let result =
        RunResultStreaming::new(pa.run(ctx, AgentInput::from_user_text("go")).await.unwrap())
            .collect()
            .await
            .unwrap();

    assert_eq!(state.get("fetchA"), Some(json!("data-A")));
    assert_eq!(state.get("fetchB"), Some(json!("data-B")));
    assert_eq!(result.usage.total_tokens, 7, "summed across branches");
    assert_eq!(
        result.final_output, r#"{"fetchA":"data-A","fetchB":"data-B"}"#,
        "deterministic sorted-key JSON"
    );
}

#[tokio::test]
async fn one_branch_fails_emits_single_aggregate_run_failed() {
    let ok = MockAgent::new("ok", |_| msg_and_complete("ok", "fine", 0));
    let bad = MockAgent::new("bad", |ctx| {
        ctx.failure_handle()
            .set(AgentError::Other(anyhow::anyhow!("nope")));
        vec![
            AgentEvent::RunStarted {
                agent: "bad".to_owned(),
            },
            AgentEvent::RunFailed {
                error: "nope".to_owned(),
            },
        ]
    });
    let pa = ParallelAgent::new("p", "").add(ok).add(bad);

    // Drain manually to assert exactly one aggregate RunFailed (child's swallowed).
    let mut stream = pa
        .run(ctx(), AgentInput::from_user_text("go"))
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(e) = stream.next().await {
        events.push(e);
    }
    let fails = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::RunFailed { .. }))
        .count();
    assert_eq!(
        fails, 1,
        "one aggregate RunFailed; child RunFailed swallowed"
    );

    // And the structured error reaches collect via the failure slot.
    let ctx = ctx();
    let failure = ctx.failure_handle();
    let pa2 = ParallelAgent::new("p", "")
        .add(MockAgent::new("ok", |_| msg_and_complete("ok", "fine", 0)))
        .add(MockAgent::new("bad", |ctx| {
            ctx.failure_handle()
                .set(AgentError::Other(anyhow::anyhow!("nope")));
            vec![
                AgentEvent::RunStarted {
                    agent: "bad".to_owned(),
                },
                AgentEvent::RunFailed {
                    error: "nope".to_owned(),
                },
            ]
        }));
    let err = RunResultStreaming::with_failure(
        pa2.run(ctx, AgentInput::from_user_text("go"))
            .await
            .unwrap(),
        failure,
    )
    .collect()
    .await
    .expect_err("aggregate failure");
    assert!(
        matches!(err, RunError::Agent(AgentError::Other(_))),
        "got {err:?}"
    );
}

#[tokio::test]
async fn duplicate_branch_keys_fail_fast() {
    // Two branches keyed "dup" would write the same state key by completion order
    // (nondeterministic) — the agent must reject this before starting any branch.
    let pa = ParallelAgent::new("p", "")
        .branch(
            "dup",
            MockAgent::new("a", |_| msg_and_complete("a", "A", 0)),
        )
        .branch(
            "dup",
            MockAgent::new("b", |_| msg_and_complete("b", "B", 0)),
        );

    let ctx = ctx();
    let failure = ctx.failure_handle();
    let err = RunResultStreaming::with_failure(
        pa.run(ctx, AgentInput::from_user_text("go")).await.unwrap(),
        failure,
    )
    .collect()
    .await
    .expect_err("duplicate branch keys rejected");
    assert!(
        matches!(err, RunError::Agent(AgentError::Other(_))),
        "got {err:?}"
    );
}
