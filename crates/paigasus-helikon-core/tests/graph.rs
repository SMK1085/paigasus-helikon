//! GraphAgent integration tests (SMA-333).

#[path = "common/mod.rs"]
mod common;

use futures_util::StreamExt as _;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, FinishReason, GraphAgent, GraphBuildError, LlmAgent, ModelEvent,
    RunContext, RunResultStreaming,
};

fn node_agent(name: &str, reply: &str) -> LlmAgent<(), common::MockModel> {
    LlmAgent::builder::<()>()
        .name(name)
        .description(format!("node {name}"))
        .shared_model(common::MockModel::with_scripts(vec![vec![
            ModelEvent::TokenDelta {
                text: reply.to_owned(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ]]))
        .instructions("test")
        .build()
}

#[test]
fn graph_build_rejects_cycle() {
    let err = GraphAgent::builder()
        .name("g")
        .node("a", node_agent("a", "x"))
        .node("b", node_agent("b", "x"))
        .edge("a", "b")
        .edge("b", "a")
        .build()
        .unwrap_err();
    assert!(matches!(err, GraphBuildError::Cycle(nodes) if nodes.contains(&"a".to_owned())));
}

#[test]
fn graph_build_rejects_unknown_edge_endpoint() {
    let err = GraphAgent::builder()
        .name("g")
        .node("a", node_agent("a", "x"))
        .edge("a", "ghost")
        .build()
        .unwrap_err();
    assert!(matches!(err, GraphBuildError::UnknownNode(n) if n == "ghost"));
}

#[test]
fn graph_build_rejects_duplicate_node_and_empty() {
    let err = GraphAgent::builder()
        .name("g")
        .node("a", node_agent("a", "x"))
        .node("a", node_agent("a", "x"))
        .build()
        .unwrap_err();
    assert!(matches!(err, GraphBuildError::DuplicateNode(n) if n == "a"));
    let err = GraphAgent::<()>::builder().name("g").build().unwrap_err();
    assert!(matches!(err, GraphBuildError::Empty));
}

fn failing_agent(name: &str) -> LlmAgent<(), common::MockModel> {
    // MockModel with zero scripts: the first `invoke` call errors, so the
    // node's run fails (surfaces as a `RunFailed` event, not a start error).
    LlmAgent::builder::<()>()
        .name(name)
        .description("fails")
        .shared_model(common::MockModel::with_scripts(vec![]))
        .instructions("test")
        .build()
}

#[tokio::test]
async fn graph_diamond_runs_in_dependency_order() {
    // a → b, a → c, b → d, c → d ; d is the single sink.
    let graph = GraphAgent::builder()
        .name("diamond")
        .node("a", node_agent("a", "A-out"))
        .node("b", node_agent("b", "B-out"))
        .node("c", node_agent("c", "C-out"))
        .node("d", node_agent("d", "D-final"))
        .edge("a", "b")
        .edge("a", "c")
        .edge("b", "d")
        .edge("c", "d")
        .build()
        .unwrap();

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let state = ctx.state().clone();
    let stream = graph
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .unwrap();
    let result = RunResultStreaming::new(stream).collect().await.unwrap();

    assert_eq!(result.final_output, "D-final"); // single sink: verbatim
    assert_eq!(state.get("a"), Some(serde_json::json!("A-out")));
    assert_eq!(state.get("d"), Some(serde_json::json!("D-final")));

    // A simple chain shows nodes only start once their sole predecessor
    // completed: check the `AgentUpdated` order.
    let ctx: RunContext<()> = RunContext::ephemeral(());
    let graph2 = GraphAgent::builder()
        .name("chain")
        .node("first", node_agent("first", "1"))
        .node("second", node_agent("second", "2"))
        .edge("first", "second")
        .build()
        .unwrap();
    let events: Vec<AgentEvent> = graph2
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .unwrap()
        .collect()
        .await;
    let order: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AgentUpdated { agent } => Some(agent.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(order, vec!["first".to_owned(), "second".to_owned()]);
}

#[tokio::test]
async fn graph_multi_sink_merges_deterministically() {
    // a → b, a → c ; sinks b and c.
    let graph = GraphAgent::builder()
        .name("fanout")
        .node("a", node_agent("a", "A"))
        .node("b", node_agent("b", "B"))
        .node("c", node_agent("c", "C"))
        .edge("a", "b")
        .edge("a", "c")
        .build()
        .unwrap();
    let ctx: RunContext<()> = RunContext::ephemeral(());
    let result = RunResultStreaming::new(
        graph
            .run(ctx, AgentInput::from_user_text("go"))
            .await
            .unwrap(),
    )
    .collect()
    .await
    .unwrap();
    assert_eq!(result.final_output, r#"{"b":"B","c":"C"}"#);
}

#[tokio::test]
async fn graph_failure_skips_descendants_but_completes_independent_branch() {
    // bad → child ; solo is independent.
    let graph = GraphAgent::builder()
        .name("partial")
        .node("bad", failing_agent("bad"))
        .node("child", node_agent("child", "never"))
        .node("solo", node_agent("solo", "solo-out"))
        .edge("bad", "child")
        .build()
        .unwrap();
    let ctx: RunContext<()> = RunContext::ephemeral(());
    let state = ctx.state().clone();
    let err = RunResultStreaming::new(
        graph
            .run(ctx, AgentInput::from_user_text("go"))
            .await
            .unwrap(),
    )
    .collect()
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("bad"), "failed node named: {msg}");
    assert!(msg.contains("child"), "skipped node named: {msg}");
    assert_eq!(state.get("solo"), Some(serde_json::json!("solo-out"))); // independent branch ran
    assert_eq!(state.get("child"), None); // descendant skipped
}

#[tokio::test]
async fn graph_duplicate_edge_declaration_runs_dependent_node_exactly_once() {
    // The chain still runs "b" (and only once) despite the doubled edge
    // declaration — locks in the builder's edge dedup (Task 3 review minor).
    let graph = GraphAgent::builder()
        .name("chain-dup")
        .node("a", node_agent("a", "A-out"))
        .node("b", node_agent("b", "B-out"))
        .edge("a", "b")
        .edge("a", "b")
        .build()
        .unwrap();
    let ctx: RunContext<()> = RunContext::ephemeral(());
    let state = ctx.state().clone();
    let events: Vec<AgentEvent> = graph
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .unwrap()
        .collect()
        .await;
    let updates_for_b = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::AgentUpdated { agent } if agent == "b"))
        .count();
    assert_eq!(updates_for_b, 1, "b must be scheduled exactly once");
    assert_eq!(state.get("b"), Some(serde_json::json!("B-out")));
}
