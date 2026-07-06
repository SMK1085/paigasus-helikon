//! SwarmAgent integration tests (SMA-333).

#[path = "common/mod.rs"]
mod common;

use futures_util::StreamExt as _;
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, FinishReason, LlmAgent, ModelEvent, RunContext,
    RunResultStreaming, SwarmAgent, SwarmBuildError,
};

#[test]
fn max_handoffs_error_displays_limit() {
    let err = AgentError::MaxHandoffsExceeded { limit: 3 };
    assert_eq!(err.to_string(), "max handoffs (3) exceeded");
}

fn member(name: &str, scripts: Vec<Vec<ModelEvent>>) -> LlmAgent<(), common::MockModel> {
    LlmAgent::builder::<()>()
        .name(name)
        .description(format!("swarm member {name}"))
        .shared_model(common::MockModel::with_scripts(scripts))
        .instructions("test")
        .build()
}

fn text_final(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta {
            text: text.to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

/// A script turn that calls the transfer tool for `target` (slugged).
fn transfer_turn(target_slug: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            call_id: "call-1".to_owned(),
            name: Some(format!("transfer_to_{target_slug}")),
            args_delta: "{}".to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]
}

#[test]
fn swarm_build_rejects_empty() {
    let err = SwarmAgent::<()>::builder().name("s").build().unwrap_err();
    assert!(matches!(err, SwarmBuildError::Empty));
}

#[test]
fn swarm_build_rejects_duplicate_member() {
    let err = SwarmAgent::builder()
        .name("s")
        .member(member("a", vec![]))
        .member(member("a", vec![]))
        .build()
        .unwrap_err();
    assert!(matches!(err, SwarmBuildError::DuplicateMember(n) if n == "a"));
}

#[test]
fn swarm_build_rejects_unknown_entry() {
    let err = SwarmAgent::builder()
        .name("s")
        .member(member("a", vec![]))
        .entry("nope")
        .build()
        .unwrap_err();
    assert!(matches!(err, SwarmBuildError::UnknownEntry(n) if n == "nope"));
}

#[tokio::test]
async fn swarm_converges_on_winner() {
    // triage hands off to budgeting; budgeting answers.
    let triage = member("triage", vec![transfer_turn("budgeting")]);
    let budgeting = member("budgeting", vec![text_final("Cut subscriptions by $40.")]);
    let investing = member("investing", vec![]);

    let swarm = SwarmAgent::builder()
        .name("support_swarm")
        .description("finance pool")
        .member(triage)
        .member(budgeting)
        .member(investing)
        .entry("triage")
        .max_handoffs(4)
        .build()
        .unwrap();

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let stream = swarm
        .run(ctx, AgentInput::from_user_text("help me budget"))
        .await
        .unwrap();
    let events: Vec<AgentEvent> = stream.collect().await;

    assert!(matches!(&events[0], AgentEvent::RunStarted { agent } if agent == "support_swarm"));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::HandoffItem { from, to } if from == "triage" && to == "budgeting")));
    // exactly one RunStarted (the swarm's own; children swallowed)
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::RunStarted { .. }))
            .count(),
        1
    );

    // Re-run through collect() to check final output attribution.
    let triage = member("triage", vec![transfer_turn("budgeting")]);
    let budgeting = member("budgeting", vec![text_final("Cut subscriptions by $40.")]);
    let swarm = SwarmAgent::builder()
        .name("support_swarm")
        .member(triage)
        .member(budgeting)
        .build()
        .unwrap();
    let ctx: RunContext<()> = RunContext::ephemeral(());
    let stream = swarm
        .run(ctx, AgentInput::from_user_text("help me budget"))
        .await
        .unwrap();
    let result = RunResultStreaming::new(stream).collect().await.unwrap();
    assert_eq!(result.final_output, "Cut subscriptions by $40.");
}

#[tokio::test]
async fn swarm_ping_pong_hits_max_handoffs() {
    // a and b transfer to each other forever (each gets plenty of scripts).
    let a = member("a", vec![transfer_turn("b"); 8]);
    let b = member("b", vec![transfer_turn("a"); 8]);
    let swarm = SwarmAgent::builder()
        .name("pingpong")
        .member(a)
        .member(b)
        .max_handoffs(3)
        .build()
        .unwrap();

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let stream = swarm
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .unwrap();
    let err = RunResultStreaming::new(stream).collect().await.unwrap_err();
    assert!(
        err.to_string().contains("max handoffs (3) exceeded"),
        "got: {err}"
    );
}

#[tokio::test]
async fn swarm_ping_pong_without_budget_hits_depth_bound() {
    let a = member("a", vec![transfer_turn("b"); 12]);
    let b = member("b", vec![transfer_turn("a"); 12]);
    let swarm = SwarmAgent::builder()
        .name("pingpong")
        .member(a)
        .member(b)
        .build()
        .unwrap();

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let stream = swarm
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .unwrap();
    let err = RunResultStreaming::new(stream).collect().await.unwrap_err();
    assert!(err.to_string().contains("nesting depth"), "got: {err}");
}

#[tokio::test]
async fn swarm_stream_survives_dropping_the_swarm() {
    let triage = member("triage", vec![transfer_turn("budgeting")]);
    let budgeting = member("budgeting", vec![text_final("done")]);
    let swarm = SwarmAgent::builder()
        .name("s")
        .member(triage)
        .member(budgeting)
        .build()
        .unwrap();

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let stream = swarm
        .run(ctx, AgentInput::from_user_text("x"))
        .await
        .unwrap();
    drop(swarm); // stream must own the members
    let result = RunResultStreaming::new(stream).collect().await.unwrap();
    assert_eq!(result.final_output, "done");
}
