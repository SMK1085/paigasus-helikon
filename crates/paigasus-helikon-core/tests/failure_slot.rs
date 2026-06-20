//! SMA-346: LlmAgent::run records the structured AgentError into the
//! RunContext failure slot at every terminal-failure pathway.

#[path = "common/mod.rs"]
mod common;

use common::{MockModel, MockTool};
use futures_util::stream::StreamExt as _;
use paigasus_helikon_core::{Agent, AgentError, AgentInput, LlmAgent, RunContext};
use std::sync::Arc;

// A no-op session local to this test (common::NoopSession is also available).
use common::NoopSession;

fn ctx() -> RunContext<()> {
    RunContext::ephemeral(()).with_session(Arc::new(NoopSession))
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

// ── Boundary: RunResultStreaming surfaces the structured error ──────────────

use futures_util::stream;
// AgentError is already imported at the top of this file.
use paigasus_helikon_core::{AgentEvent, FailureSlot, RunError, RunResultStreaming};

fn run_failed_stream() -> futures_core::stream::BoxStream<'static, AgentEvent> {
    Box::pin(stream::iter(vec![AgentEvent::RunFailed {
        error: "max turns (1) exceeded".into(),
    }]))
}

#[tokio::test]
async fn collect_prefers_slot_over_string() {
    let slot = FailureSlot::new();
    slot.set(AgentError::MaxTurnsExceeded(1));
    let err = RunResultStreaming::with_failure(run_failed_stream(), slot)
        .collect()
        .await
        .expect_err("run failed");
    assert!(
        matches!(err, RunError::Agent(AgentError::MaxTurnsExceeded(1))),
        "expected RunError::Agent(MaxTurnsExceeded(1)), got {err:?}"
    );
}

#[tokio::test]
async fn collect_without_slot_falls_back_to_string() {
    let err = RunResultStreaming::new(run_failed_stream())
        .collect()
        .await
        .expect_err("run failed");
    assert!(
        matches!(err, RunError::Other(_)),
        "no slot => opaque string error, got {err:?}"
    );
}

#[tokio::test]
async fn collect_typed_prefers_slot() {
    #[derive(Debug, serde::Deserialize)]
    struct Answer {
        #[allow(dead_code)]
        value: u32,
    }
    let slot = FailureSlot::new();
    slot.set(AgentError::NotImplemented { feature: "handoff" });
    let err = RunResultStreaming::with_failure(run_failed_stream(), slot)
        .collect_typed::<Answer>()
        .await
        .expect_err("run failed");
    assert!(
        matches!(err, AgentError::NotImplemented { feature: "handoff" }),
        "expected NotImplemented, got {err:?}"
    );
}

/// Cross-carrier invariant: for InvalidStructuredOutput, the slot (primary) and
/// the StructuredOutputFailed-event fallback yield the same error at the boundary.
#[tokio::test]
async fn collect_typed_slot_matches_event_fallback() {
    #[derive(Debug, serde::Deserialize)]
    struct Answer {
        #[allow(dead_code)]
        value: u32,
    }
    let errs = vec!["missing field `value`".to_string()];
    let text = "{}".to_string();

    let events = || {
        Box::pin(stream::iter(vec![
            AgentEvent::StructuredOutputFailed {
                schema_errors: errs.clone(),
                final_text: text.clone(),
            },
            AgentEvent::RunFailed {
                error: "invalid structured output".into(),
            },
        ]))
    };

    // Primary: slot carries the structured error.
    let slot = FailureSlot::new();
    slot.set(AgentError::InvalidStructuredOutput {
        schema_errors: errs.clone(),
        final_text: text.clone(),
    });
    let from_slot = RunResultStreaming::with_failure(events(), slot)
        .collect_typed::<Answer>()
        .await
        .expect_err("err");

    // Fallback: no slot => reconstruct from the StructuredOutputFailed event.
    let from_event = RunResultStreaming::new(events())
        .collect_typed::<Answer>()
        .await
        .expect_err("err");

    for err in [from_slot, from_event] {
        match err {
            AgentError::InvalidStructuredOutput {
                schema_errors,
                final_text,
            } => {
                assert_eq!(schema_errors, errs);
                assert_eq!(final_text, text);
            }
            other => panic!("expected InvalidStructuredOutput, got {other:?}"),
        }
    }
}

/// End-to-end drain-then-read: a real max-turns run, collected via with_failure,
/// surfaces the structured error. A naive early-return collect() regresses this
/// to RunError::Other (the state-machine error is recorded after RunFailed).
#[tokio::test]
async fn end_to_end_max_turns_collects_as_run_error_agent() {
    use paigasus_helikon_core::{FinishReason, ModelEvent};
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
    let err = RunResultStreaming::with_failure(stream, failure)
        .collect()
        .await
        .expect_err("run failed");
    assert!(
        matches!(err, RunError::Agent(AgentError::MaxTurnsExceeded(1))),
        "expected RunError::Agent(MaxTurnsExceeded(1)), got {err:?}"
    );
}
