//! SMA-324 — end-to-end handoff: 3-agent finance triage, collisions, depth guard.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::MockModel;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, FinishReason, HookRegistry, LlmAgent,
    MemorySession, ModelEvent, RunConfig, RunContext, RunResultStreaming, Session, TracerHandle,
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

fn transfer_turn(tool: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            call_id: "c1".to_owned(),
            name: Some(tool.to_owned()),
            args_delta: "{}".to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]
}

fn text_turn(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta {
            text: text.to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

#[tokio::test]
async fn triage_routes_to_budgeting_not_investing() {
    let budgeting = LlmAgent::builder::<()>()
        .name("budgeting specialist")
        .description("Handles budgeting questions.")
        .shared_model(MockModel::with_scripts(vec![text_turn(
            "Cut dining by $60.",
        )]))
        .build();
    let investing = LlmAgent::builder::<()>()
        .name("investing specialist")
        .description("Handles investing questions.")
        .shared_model(MockModel::with_scripts(vec![]))
        .build();

    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(MockModel::with_scripts(vec![transfer_turn(
            "transfer_to_budgeting_specialist",
        )]))
        .handoff(budgeting)
        .handoff(investing)
        .build();

    let stream = triage
        .run(ctx(), AgentInput::from_user_text("How do I budget?"))
        .await
        .expect("run starts");
    let result = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect("run completes");

    assert_eq!(result.final_output, "Cut dining by $60.");

    let starts = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::RunStarted { .. }))
        .count();
    assert_eq!(starts, 1, "exactly one RunStarted across the chain");

    assert!(result.events.iter().any(|e| matches!(
        e,
        AgentEvent::HandoffItem { from, to }
            if from == "triage" && to == "budgeting specialist"
    )));
    assert!(result.events.iter().any(|e| matches!(
        e,
        AgentEvent::AgentUpdated { agent } if agent == "budgeting specialist"
    )));
}

#[tokio::test]
async fn slug_collision_between_handoffs_fails_fast() {
    let a = LlmAgent::builder::<()>()
        .name("Budgeting Specialist")
        .shared_model(MockModel::with_scripts(vec![]))
        .build();
    let b = LlmAgent::builder::<()>()
        .name("budgeting specialist")
        .shared_model(MockModel::with_scripts(vec![]))
        .build();
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(MockModel::with_scripts(vec![text_turn("hi")]))
        .handoff(a)
        .handoff(b)
        .build();

    let stream = triage
        .run(ctx(), AgentInput::from_user_text("x"))
        .await
        .expect("run starts");
    let err = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect_err("collision fails the run");
    assert!(err.to_string().contains("collision"), "got: {err}");
}

#[tokio::test]
async fn handoff_cycle_hits_depth_guard() {
    // `a` transfers to `b`; `b` transfers back to `a`. Each lists the other so
    // the model's transfer call always resolves. Bounded by max_agent_depth = 1.
    let a_for_b = LlmAgent::builder::<()>()
        .name("a")
        .shared_model(MockModel::with_scripts(vec![transfer_turn(
            "transfer_to_b",
        )]))
        .build();
    let b = LlmAgent::builder::<()>()
        .name("b")
        .description("b")
        .shared_model(MockModel::with_scripts(vec![transfer_turn(
            "transfer_to_a",
        )]))
        .handoff(a_for_b)
        .build();
    let a = LlmAgent::builder::<()>()
        .name("a")
        .description("a")
        .shared_model(MockModel::with_scripts(vec![transfer_turn(
            "transfer_to_b",
        )]))
        .handoff(b)
        .build();

    let run_ctx = ctx().with_run_config(RunConfig::new().with_max_agent_depth(1));
    let stream = a
        .run(run_ctx, AgentInput::from_user_text("loop"))
        .await
        .expect("run starts");
    let err = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect_err("depth guard trips");
    assert!(
        err.to_string().contains("nesting depth"),
        "expected depth error, got: {err}"
    );
}

fn transfer_turn_with_usage(tool: &str, input_tokens: u32, output_tokens: u32) -> Vec<ModelEvent> {
    vec![
        ModelEvent::Usage {
            input_tokens,
            output_tokens,
            cached_input_tokens: None,
            reasoning_tokens: None,
        },
        ModelEvent::ToolCallDelta {
            call_id: "c1".to_owned(),
            name: Some(tool.to_owned()),
            args_delta: "{}".to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]
}

fn text_turn_with_usage(text: &str, input_tokens: u32, output_tokens: u32) -> Vec<ModelEvent> {
    vec![
        ModelEvent::Usage {
            input_tokens,
            output_tokens,
            cached_input_tokens: None,
            reasoning_tokens: None,
        },
        ModelEvent::TokenDelta {
            text: text.to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

#[tokio::test]
async fn handoff_usage_sums_across_chain() {
    // triage: 10 input + 5 output; budgeting: 20 input + 7 output
    // expected chain total: 30 input + 12 output
    let budgeting = LlmAgent::builder::<()>()
        .name("budgeting specialist")
        .description("Handles budgeting questions.")
        .shared_model(MockModel::with_scripts(vec![text_turn_with_usage(
            "Spend less.",
            20,
            7,
        )]))
        .build();

    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(MockModel::with_scripts(vec![transfer_turn_with_usage(
            "transfer_to_budgeting_specialist",
            10,
            5,
        )]))
        .handoff(budgeting)
        .build();

    let stream = triage
        .run(ctx(), AgentInput::from_user_text("How do I budget?"))
        .await
        .expect("run starts");
    let result = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect("run completes");

    assert_eq!(
        result.usage.input_tokens, 30,
        "input_tokens should be 10+20"
    );
    assert_eq!(
        result.usage.output_tokens, 12,
        "output_tokens should be 5+7"
    );
}

#[tokio::test]
async fn handoff_target_failure_propagates() {
    // Target has an empty script — model.invoke errors with "no more scripted responses".
    let failing_target = LlmAgent::builder::<()>()
        .name("failing target")
        .description("Always fails.")
        .shared_model(MockModel::with_scripts(vec![]))
        .build();

    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(MockModel::with_scripts(vec![transfer_turn(
            "transfer_to_failing_target",
        )]))
        .handoff(failing_target)
        .build();

    let stream = triage
        .run(ctx(), AgentInput::from_user_text("trigger failure"))
        .await
        .expect("run starts");
    let err = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect_err("target failure should propagate as run error");
    assert!(
        !err.to_string().is_empty(),
        "error message should be non-empty, got: {err}"
    );
}
