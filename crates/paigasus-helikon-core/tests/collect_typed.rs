//! collect_typed deserializes the terminal assistant text into T, and maps
//! a StructuredOutputFailed event to AgentError::InvalidStructuredOutput.

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken, CheckpointDecision, ContentPart,
    FinishReason, Item, LlmAgent, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
    RunContext, RunResultStreaming, StopReason, TokenUsage, Tool, TurnBoundary, TurnCheckpoint,
};

#[derive(Debug, PartialEq, serde::Deserialize, schemars::JsonSchema)]
struct Answer {
    #[allow(dead_code)]
    value: u32,
}

#[tokio::test]
async fn collect_typed_returns_struct() {
    let events = vec![
        AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: vec![ContentPart::Text {
                    text: "{\"value\":7}".into(),
                }],
                agent: None,
            },
        },
        AgentEvent::RunCompleted {
            usage: TokenUsage::default(),
        },
    ];
    let stream = Box::pin(stream::iter(events));
    let result = RunResultStreaming::new(stream)
        .collect_typed::<Answer>()
        .await
        .expect("collect_typed should succeed");
    assert_eq!(result.final_output, Answer { value: 7 });
}

#[tokio::test]
async fn collect_typed_maps_structured_failure() {
    let events = vec![
        AgentEvent::StructuredOutputFailed {
            schema_errors: vec!["missing field `value`".into()],
            final_text: "{}".into(),
        },
        AgentEvent::RunFailed {
            error: "invalid structured output".into(),
        },
    ];
    let stream = Box::pin(stream::iter(events));
    let err = RunResultStreaming::new(stream)
        .collect_typed::<Answer>()
        .await
        .expect_err("should be an error");
    match err {
        AgentError::InvalidStructuredOutput {
            schema_errors,
            final_text,
        } => {
            assert_eq!(schema_errors, vec!["missing field `value`".to_string()]);
            assert_eq!(final_text, "{}");
        }
        other => panic!("expected InvalidStructuredOutput, got {other:?}"),
    }
}

#[tokio::test]
async fn collect_typed_maps_plain_run_failure_to_other() {
    // A RunFailed with no preceding StructuredOutputFailed (e.g. MaxTurnsExceeded)
    // surfaces as AgentError::Other, not InvalidStructuredOutput.
    let events = vec![AgentEvent::RunFailed {
        error: "max turns (1) exceeded".into(),
    }];
    let stream = Box::pin(stream::iter(events));
    let err = RunResultStreaming::new(stream)
        .collect_typed::<Answer>()
        .await
        .expect_err("should be an error");
    match err {
        AgentError::Other(e) => assert!(e.to_string().contains("max turns")),
        other => panic!("expected Other, got {other:?}"),
    }
}

#[tokio::test]
async fn collect_typed_returns_incomplete_on_run_deferred() {
    let call = paigasus_helikon_core::PendingToolCall::new(
        "d1",
        "c1",
        "pay",
        serde_json::json!({}),
        "Pay?",
        vec![],
    );
    let events = vec![AgentEvent::RunDeferred {
        calls: vec![call],
        usage: TokenUsage::default(),
    }];
    let stream = Box::pin(stream::iter(events));
    let err = RunResultStreaming::new(stream)
        .collect_typed::<Answer>()
        .await
        .expect_err("a deferred run has no final output");
    assert!(matches!(
        err,
        AgentError::Incomplete {
            stop_reason: paigasus_helikon_core::StopReason::Deferred { .. }
        }
    ));
}

/// A [`common::MockModel`] that counts its invocations.
struct CountingModel {
    inner: Arc<common::MockModel>,
    invocations: Arc<AtomicUsize>,
}

#[async_trait]
impl Model for CountingModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        self.inner.invoke(request, cancel).await
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// Stops the run at the first boundary.
struct StopAtBoundary;

#[async_trait]
impl TurnCheckpoint<()> for StopAtBoundary {
    async fn at_boundary(&self, _: &RunContext<()>, _: &TurnBoundary) -> CheckpointDecision {
        CheckpointDecision::Stop {
            reason: "operator".into(),
        }
    }
}

/// SMA-804: a structured-output run that the turn checkpoint stops has no
/// final output. No `Finalizing` model call happens, and `collect_typed`
/// returns `AgentError::Incomplete` with `StopReason::Stopped`.
#[tokio::test]
async fn stop_with_output_type_is_incomplete() {
    let invocations = Arc::new(AtomicUsize::new(0));
    let model = Arc::new(CountingModel {
        inner: common::MockModel::with_scripts(vec![
            vec![
                ModelEvent::ToolCallDelta {
                    call_id: "c1".into(),
                    name: Some("noop".into()),
                    args_delta: "{}".into(),
                },
                ModelEvent::Finish {
                    reason: FinishReason::ToolCalls,
                },
            ],
            vec![
                ModelEvent::TokenDelta {
                    text: "{\"value\":1}".into(),
                },
                ModelEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ],
        ]),
        invocations: Arc::clone(&invocations),
    });
    let agent = LlmAgent::builder::<()>()
        .name("typed")
        .shared_model(model)
        .shared_tool(
            common::MockTool::new("noop", serde_json::json!({"ok": true})) as Arc<dyn Tool<()>>,
        )
        .output_type::<Answer>()
        .build();
    let ctx = RunContext::ephemeral(()).with_turn_checkpoint(Arc::new(StopAtBoundary));
    let slot = ctx.failure_handle();
    let stream = agent
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .expect("run starts");

    let err = RunResultStreaming::with_failure(stream, slot)
        .collect_typed::<Answer>()
        .await
        .expect_err("a stopped run has no final output");

    match err {
        AgentError::Incomplete {
            stop_reason: StopReason::Stopped { reason },
        } => assert_eq!(reason, "operator"),
        other => panic!("expected Incomplete(Stopped), got {other:?}"),
    }
    assert_eq!(
        invocations.load(Ordering::SeqCst),
        1,
        "the model is called once; no Finalizing turn"
    );
}
