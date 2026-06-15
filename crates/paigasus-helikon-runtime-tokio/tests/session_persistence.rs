//! Session persistence wired into the run lifecycle (SMA-392).

#[path = "common/mod.rs"]
mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    AgentInput, CancellationToken, ContentPart, FinishReason, Item, MemorySession, Model,
    ModelCapabilities, ModelError, ModelEvent, ModelRequest, RunConfig, Runner, Session,
    SessionEvent,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{run_context_with_session, text_agent};

/// Model that records each request's messages and replays one scripted turn.
struct RecordingModel {
    requests: Arc<Mutex<Vec<Vec<Item>>>>,
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl RecordingModel {
    fn new(requests: Arc<Mutex<Vec<Vec<Item>>>>, scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            requests,
            scripts: Mutex::new(scripts.into()),
        })
    }
}

#[async_trait]
impl Model for RecordingModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.requests.lock().unwrap().push(request.messages.clone());
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ModelError::Other(anyhow::anyhow!("no more scripted responses")))?;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

fn say(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta { text: text.into() },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

fn content_text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_turn_round_trip_sees_prior_messages() {
    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = RecordingModel::new(requests.clone(), vec![say("first"), say("second")]);
    let agent = text_agent(model, Vec::new());

    let r1 = TokioRunner
        .run(
            &agent,
            run_context_with_session(session.clone()),
            AgentInput::from_user_text("hello"),
            RunConfig::default(),
        )
        .await;
    assert!(r1.is_ok(), "turn 1: {r1:?}");

    let r2 = TokioRunner
        .run(
            &agent,
            run_context_with_session(session.clone()),
            AgentInput::from_user_text("again"),
            RunConfig::default(),
        )
        .await;
    assert!(r2.is_ok(), "turn 2: {r2:?}");

    // Acceptance #1: turn 2's model request contains turn 1's user + assistant.
    // Drop the lock guard before the subsequent `await` to satisfy clippy's
    // `await_holding_lock` lint (std::sync::MutexGuard must not cross an await).
    {
        let reqs = requests.lock().unwrap();
        assert_eq!(reqs.len(), 2, "one model call per turn");
        let turn2 = &reqs[1];
        assert!(
            turn2.iter().any(
                |m| matches!(m, Item::UserMessage { content } if content_text(content) == "hello")
            ),
            "turn 2 request must include turn 1's user message: {turn2:?}"
        );
        assert!(
            turn2.iter().any(|m| matches!(m, Item::AssistantMessage { content, .. } if content_text(content) == "first")),
            "turn 2 request must include turn 1's assistant reply: {turn2:?}"
        );
    } // lock guard dropped here, before the await below

    // Acceptance #2: the persisted log is [User, Asst, User, Asst].
    let events = session.events(None).await.unwrap();
    assert_eq!(events.len(), 4, "{events:?}");
    assert!(matches!(&events[0], SessionEvent::UserMessage { .. }));
    assert!(matches!(&events[1], SessionEvent::AssistantMessage { agent, .. } if agent == "test"));
    assert!(matches!(&events[2], SessionEvent::UserMessage { .. }));
    assert!(matches!(&events[3], SessionEvent::AssistantMessage { .. }));
}
