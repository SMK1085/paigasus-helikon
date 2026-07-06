//! GraphAgent integration tests (SMA-333).

#[path = "common/mod.rs"]
mod common;

use paigasus_helikon_core::{FinishReason, GraphAgent, GraphBuildError, LlmAgent, ModelEvent};

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
