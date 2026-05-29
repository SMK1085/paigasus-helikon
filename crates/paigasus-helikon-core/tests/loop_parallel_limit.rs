//! `parallel_tool_call_limit` bounds concurrent tool execution; `None` runs
//! all tool calls at once. Verified with a peak-concurrency probe.

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentInput, FinishReason, Instructions, LlmAgent, ModelEvent, ModelSettings, RunConfig,
    RunResultStreaming, Tool,
};

use common::{noop_run_context, ConcurrencyProbe, MockModel};

fn four_call_model() -> Arc<MockModel> {
    let mut calls = Vec::new();
    for i in 1..=4 {
        calls.push(ModelEvent::ToolCallDelta {
            call_id: i.to_string(),
            name: Some(format!("p{i}")),
            args_delta: "{}".into(),
        });
    }
    calls.push(ModelEvent::Finish {
        reason: FinishReason::ToolCalls,
    });
    MockModel::with_scripts(vec![
        calls,
        vec![
            ModelEvent::TokenDelta {
                text: "done".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ])
}

fn probe_agent(current: Arc<AtomicUsize>, max: Arc<AtomicUsize>) -> LlmAgent<(), MockModel> {
    let tools: Vec<Arc<dyn Tool<()>>> = (1..=4)
        .map(|i| {
            ConcurrencyProbe::new(&format!("p{i}"), current.clone(), max.clone())
                as Arc<dyn Tool<()>>
        })
        .collect();
    LlmAgent::<(), _> {
        name: "test".into(),
        description: "probe".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model: four_call_model(),
        tools,
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limit_two_caps_concurrency() {
    let current = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let agent = probe_agent(current.clone(), max.clone());

    let ctx = noop_run_context::<()>().with_run_config(
        RunConfig::new().with_parallel_tool_call_limit(std::num::NonZeroUsize::new(2).unwrap()),
    );
    let stream = agent
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .expect("run starts");
    RunResultStreaming::new(stream).collect().await.expect("ok");

    assert_eq!(
        max.load(Ordering::SeqCst),
        2,
        "peak concurrency must be capped at 2"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unbounded_runs_all_four() {
    let current = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let agent = probe_agent(current.clone(), max.clone());

    // No run_config => falls back to agent.config (limit None => unbounded).
    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .expect("run starts");
    RunResultStreaming::new(stream).collect().await.expect("ok");

    assert_eq!(
        max.load(Ordering::SeqCst),
        4,
        "unbounded must run all four at once"
    );
}
