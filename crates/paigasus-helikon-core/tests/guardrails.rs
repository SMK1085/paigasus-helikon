//! SMA-326: input/output guardrail gates.

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, FinishReason, Guardrail, GuardrailError,
    GuardrailInput, GuardrailKind, GuardrailVerdict, Instructions, LlmAgent, Model,
    ModelCapabilities, ModelError, ModelEvent, ModelRequest, ModelSettings, RunConfig, RunContext,
    RunResultStreaming,
};

use common::noop_run_context;

/// A model that counts every `invoke` so a test can assert zero calls.
struct CountingModel {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Model for CountingModel {
    async fn invoke(
        &self,
        _r: ModelRequest,
        _c: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(
            Vec::<Result<ModelEvent, ModelError>>::new(),
        )))
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

struct AlwaysTrip;
#[async_trait]
impl Guardrail<()> for AlwaysTrip {
    async fn check(
        &self,
        _: &RunContext<()>,
        _: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError> {
        Ok(GuardrailVerdict::Tripwire {
            kind: GuardrailKind::InputPolicy,
            info: serde_json::json!({"why": "test"}),
        })
    }
}

fn agent_with_input_guardrail(calls: Arc<AtomicUsize>) -> LlmAgent<(), CountingModel> {
    LlmAgent::<(), _> {
        name: "g".into(),
        description: "guardrail test".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model: Arc::new(CountingModel { calls }),
        tools: Vec::new(),
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: vec![Arc::new(AlwaysTrip) as Arc<dyn Guardrail<()>>],
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    }
}

#[tokio::test]
async fn input_guardrail_aborts_before_any_model_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = agent_with_input_guardrail(Arc::clone(&calls));
    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("hi"))
        .await
        .expect("stream starts");
    let result = RunResultStreaming::new(stream).collect().await;

    assert!(result.is_err(), "tripwire must fail the run");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "zero model calls (AC1)");
}

#[tokio::test]
async fn output_guardrail_suppresses_run_completed() {
    use common::MockModel;

    let model = MockModel::with_scripts(vec![vec![
        ModelEvent::TokenDelta {
            text: "final answer".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]]);

    let agent = LlmAgent::<(), _> {
        name: "og".into(),
        description: "output guardrail".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools: Vec::new(),
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: vec![Arc::new(AlwaysTrip) as Arc<dyn Guardrail<()>>],
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    };

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .unwrap();
    use futures_util::StreamExt as _;
    let events: Vec<_> = stream.collect().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::GuardrailTriggered { .. })),
        "a GuardrailTriggered event is emitted on the output tripwire"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::RunFailed { .. })),
        "the run fails"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::RunCompleted { .. })),
        "RunCompleted is suppressed on an output tripwire"
    );
}
