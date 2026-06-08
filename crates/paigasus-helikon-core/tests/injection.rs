//! SMA-326: hook system-message injection reaches the model request.

#[path = "common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    Agent, AgentInput, CancellationToken, ContentPart, FinishReason, Hook, HookDecision, HookEvent,
    Instructions, Item, LlmAgent, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
    ModelSettings, RunConfig, RunContext, RunResultStreaming,
};

use common::noop_run_context;

/// Records the messages of the first model request it sees, then returns a
/// trivial stop response.
struct RecordingModel {
    seen: Arc<Mutex<Vec<Item>>>,
}
#[async_trait]
impl Model for RecordingModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        {
            let mut g = self.seen.lock().unwrap();
            if g.is_empty() {
                *g = request.messages.clone();
            }
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ModelEvent::TokenDelta {
                text: "done".into(),
            }),
            Ok(ModelEvent::Finish {
                reason: FinishReason::Stop,
            }),
        ])))
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// Injects a system message on OnTurnStart.
struct InjectOnTurn;
#[async_trait]
impl Hook<()> for InjectOnTurn {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        match event {
            HookEvent::OnTurnStart { .. } => HookDecision::InjectSystemMessage {
                text: "INJECTED".into(),
            },
            _ => HookDecision::Allow,
        }
    }
}

#[tokio::test]
async fn on_turn_start_injection_reaches_the_model_request() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = LlmAgent::<(), _> {
        name: "i".into(),
        description: "injection test".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model: Arc::new(RecordingModel {
            seen: Arc::clone(&seen),
        }),
        tools: Vec::new(),
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: vec![Arc::new(InjectOnTurn) as Arc<dyn Hook<()>>],
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    };

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .unwrap();
    let _ = RunResultStreaming::new(stream).collect().await.unwrap();

    let msgs = seen.lock().unwrap().clone();
    let injected = msgs.iter().any(|i| {
        matches!(i, Item::System { content }
            if content.iter().any(|p| matches!(p, ContentPart::Text { text } if text == "INJECTED")))
    });
    assert!(
        injected,
        "OnTurnStart-injected system message must reach the model request"
    );
}
