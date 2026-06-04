//! SMA-325 — acceptance criterion 1 (Sequential([Parallel, summarize])) and the
//! agent-nesting depth bound.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::{msg_and_complete, MockAgent};
use paigasus_helikon_core::{
    Agent, AgentInput, CancellationToken, HookRegistry, MemorySession, ParallelAgent, RunConfig,
    RunContext, RunResultStreaming, SequentialAgent, Session, TracerHandle,
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
async fn sequential_parallel_summarize_pipeline() {
    let fetch = ParallelAgent::new("fetch", "fetch A and B")
        .add(MockAgent::new("fetchA", |_| {
            msg_and_complete("fetchA", "data-A", 0)
        }))
        .add(MockAgent::new("fetchB", |_| {
            msg_and_complete("fetchB", "data-B", 0)
        }));

    let summarize = MockAgent::new("summarize", |ctx| {
        let a = ctx
            .state()
            .get("fetchA")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let b = ctx
            .state()
            .get("fetchB")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        msg_and_complete("summarize", &format!("A={a};B={b}"), 0)
    });

    let pipeline = SequentialAgent::new("pipeline", "fetch then summarize")
        .then(fetch)
        .then(summarize);

    let ctx = ctx();
    let state = ctx.state().clone();
    let result = RunResultStreaming::new(
        pipeline
            .run(ctx, AgentInput::from_user_text("go"))
            .await
            .unwrap(),
    )
    .collect()
    .await
    .unwrap();

    // summarize ran AFTER the parallel block (it observed both keys).
    assert_eq!(result.final_output, "A=data-A;B=data-B");
    assert_eq!(state.get("fetchA"), Some(json!("data-A")));
    assert_eq!(state.get("fetchB"), Some(json!("data-B")));
}

#[tokio::test]
async fn nested_workflow_agents_respect_max_agent_depth() {
    let inner = SequentialAgent::new("inner", "")
        .then(MockAgent::new("leaf", |_| msg_and_complete("leaf", "x", 0)));
    let outer = SequentialAgent::new("outer", "").then(inner);

    // max_agent_depth = 1: outer(0)->inner(1) ok; inner(1)->leaf(2) exceeds.
    let ctx = ctx().with_run_config(RunConfig::new().with_max_agent_depth(1));
    let err = RunResultStreaming::new(
        outer
            .run(ctx, AgentInput::from_user_text("go"))
            .await
            .unwrap(),
    )
    .collect()
    .await
    .expect_err("depth exceeded");
    assert!(err.to_string().contains("nesting depth"), "got: {err}");
}
