//! SMA-325 — SequentialAgent: order, state threading, usage, fail-fast.

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use common::{msg_and_complete, MockAgent};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, RunContext, RunError, RunResultStreaming,
    SequentialAgent,
};

fn ctx() -> RunContext<()> {
    RunContext::ephemeral(())
}

#[tokio::test]
async fn threads_output_via_state() {
    let producer = MockAgent::new("producer", |_| msg_and_complete("producer", "hello", 0));
    let consumer = MockAgent::new("consumer", |ctx| {
        let upstream = ctx
            .state()
            .get("producer")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "MISSING".to_owned());
        msg_and_complete("consumer", &format!("got: {upstream}"), 0)
    });
    let seq = SequentialAgent::new("seq", "produce then consume")
        .then(producer)
        .then(consumer);

    let result = RunResultStreaming::new(
        seq.run(ctx(), AgentInput::from_user_text("go"))
            .await
            .unwrap(),
    )
    .collect()
    .await
    .unwrap();

    assert_eq!(
        result.final_output, "got: hello",
        "A->B threading via state"
    );
}

#[tokio::test]
async fn order_and_usage_and_single_outer_lifecycle() {
    let a = MockAgent::new("a", |_| msg_and_complete("a", "A", 10));
    let b = MockAgent::new("b", |_| msg_and_complete("b", "B", 5));
    let seq = SequentialAgent::new("seq", "").then(a).then(b);

    let result = RunResultStreaming::new(
        seq.run(ctx(), AgentInput::from_user_text("go"))
            .await
            .unwrap(),
    )
    .collect()
    .await
    .unwrap();

    let updates: Vec<String> = result
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AgentUpdated { agent } => Some(agent.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(updates, vec!["a".to_owned(), "b".to_owned()], "in order");

    let starts = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::RunStarted { .. }))
        .count();
    let completes = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::RunCompleted { .. }))
        .count();
    assert_eq!(starts, 1, "only the outer RunStarted surfaces");
    assert_eq!(completes, 1, "only the outer RunCompleted surfaces");
    assert_eq!(result.usage.total_tokens, 15, "summed across steps");
    assert_eq!(result.final_output, "B", "last step's output");
}

#[tokio::test]
async fn fail_fast_stops_later_steps_and_surfaces_structured_error() {
    let ran_second = Arc::new(AtomicBool::new(false));
    let flag = ran_second.clone();
    let boom = MockAgent::new("boom", |ctx| {
        ctx.failure_handle()
            .set(AgentError::Other(anyhow::anyhow!("kaboom")));
        vec![
            AgentEvent::RunStarted {
                agent: "boom".to_owned(),
            },
            AgentEvent::RunFailed {
                error: "kaboom".to_owned(),
            },
        ]
    });
    let never = MockAgent::new("never", move |_| {
        flag.store(true, Ordering::SeqCst);
        msg_and_complete("never", "x", 0)
    });
    let seq = SequentialAgent::new("seq", "").then(boom).then(never);

    let ctx = ctx();
    let failure = ctx.failure_handle();
    let err = RunResultStreaming::with_failure(
        seq.run(ctx, AgentInput::from_user_text("go"))
            .await
            .unwrap(),
        failure,
    )
    .collect()
    .await
    .expect_err("first step fails");

    assert!(
        matches!(err, RunError::Agent(AgentError::Other(_))),
        "structured error: {err:?}"
    );
    assert!(
        !ran_second.load(Ordering::SeqCst),
        "fail-fast: second step must not run"
    );
}

#[tokio::test]
async fn sequential_agent_rejects_resolutions() {
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let r = runs.clone();
    let child = MockAgent::new("child", move |_| {
        r.fetch_add(1, Ordering::SeqCst);
        msg_and_complete("child", "ran", 0)
    });
    let seq = SequentialAgent::new("seq", "").then(child);
    let input = AgentInput::from_user_text("x")
        .with_resolution("d1", paigasus_helikon_core::ToolResolution::Approved);
    let events: Vec<AgentEvent> =
        futures_util::StreamExt::collect(seq.run(ctx(), input).await.unwrap()).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::RunFailed { error }) if error == "this agent cannot resume deferred calls"
    ));
    assert_eq!(runs.load(Ordering::SeqCst), 0, "no child ran");
}

fn cancelled_ctx() -> RunContext<()> {
    let token = paigasus_helikon_core::CancellationToken::new();
    token.cancel();
    RunContext::ephemeral(()).with_cancel(token)
}

#[tokio::test]
async fn cancelled_child_ends_sequence_with_no_terminal() {
    let second_runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let s = second_runs.clone();
    let first = MockAgent::new("first", |_| vec![common::assistant_msg("first", "partial")]);
    let second = MockAgent::new("second", move |_| {
        s.fetch_add(1, Ordering::SeqCst);
        vec![common::assistant_msg("second", "partial")]
    });
    let seq = SequentialAgent::new("seq", "").then(first).then(second);
    let events: Vec<AgentEvent> = futures_util::StreamExt::collect(
        seq.run(cancelled_ctx(), AgentInput::from_user_text("go"))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        !events.iter().any(AgentEvent::is_terminal),
        "a cancelled run ends with no terminal event"
    );
    assert_eq!(second_runs.load(Ordering::SeqCst), 0, "second did not run");
}

/// Regression guard: a child with no terminal event and no cancel still lets
/// the sequence continue and complete.
#[tokio::test]
async fn child_without_terminal_and_without_cancel_keeps_running() {
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (r1, r2) = (runs.clone(), runs.clone());
    let first = MockAgent::new("first", move |_| {
        r1.fetch_add(1, Ordering::SeqCst);
        vec![common::assistant_msg("first", "partial")]
    });
    let second = MockAgent::new("second", move |_| {
        r2.fetch_add(1, Ordering::SeqCst);
        vec![common::assistant_msg("second", "partial")]
    });
    let seq = SequentialAgent::new("seq", "").then(first).then(second);
    let events: Vec<AgentEvent> = futures_util::StreamExt::collect(
        seq.run(ctx(), AgentInput::from_user_text("go"))
            .await
            .unwrap(),
    )
    .await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::RunCompleted { .. })
    ));
    assert_eq!(runs.load(Ordering::SeqCst), 2, "both children ran");
}

/// Regression guard: a token that fires after every child emitted its terminal
/// event must not suppress the workflow's own `RunCompleted`.
#[tokio::test]
async fn cancel_after_every_child_terminal_still_completes() {
    let first = MockAgent::new("first", |ctx| {
        ctx.cancel().cancel();
        common::msg_and_complete("first", "A", 0)
    });
    let second = MockAgent::new("second", |_| common::msg_and_complete("second", "B", 0));
    let seq = SequentialAgent::new("seq", "").then(first).then(second);
    let events: Vec<AgentEvent> = futures_util::StreamExt::collect(
        seq.run(ctx(), AgentInput::from_user_text("go"))
            .await
            .unwrap(),
    )
    .await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::RunCompleted { .. })
    ));
}
