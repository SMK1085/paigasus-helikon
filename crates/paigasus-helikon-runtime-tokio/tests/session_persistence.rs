//! Session persistence wired into the run lifecycle (SMA-392).

#[path = "common/mod.rs"]
mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    project, AgentInput, CancellationToken, ContentPart, FinishReason, Item, MemorySession, Model,
    ModelCapabilities, ModelError, ModelEvent, ModelRequest, RunConfig, Runner, Session,
    SessionEvent, Tool, ToolContext, ToolError, ToolOutput,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{run_context_with_session, run_context_with_session_and_cancel, text_agent};

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

/// Tool whose invocation never returns — lets a run be cancelled mid-execution.
struct BlockingTool {
    name: String,
    schema: serde_json::Value,
}

#[async_trait]
impl Tool<()> for BlockingTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "blocks forever"
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext<()>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        std::future::pending::<()>().await;
        unreachable!("pending() never resolves")
    }
}

fn call_tool(call_id: &str, name: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            call_id: call_id.into(),
            name: Some(name.into()),
            args_delta: "{}".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_mid_tool_persists_provider_valid_log() {
    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = RecordingModel::new(requests, vec![call_tool("c1", "blocker")]);
    let tool: Arc<dyn Tool<()>> = Arc::new(BlockingTool {
        name: "blocker".into(),
        schema: serde_json::json!({"type": "object"}),
    });
    let agent = text_agent(model, vec![tool]);

    let cancel = CancellationToken::new();
    let ctx = run_context_with_session_and_cancel(session.clone(), cancel.clone());

    let res = tokio::time::timeout(Duration::from_secs(5), async {
        let run_fut = TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        );
        let canceller = async {
            // 50ms is ample for the in-memory model stream to deliver ToolCallItem
            // (so the recorder observes the call); `controlled`'s `biased` select
            // drains ready stream events before checking the cancel flag, so the
            // call is always recorded before the cancel lands mid-tool.
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        };
        let (r, _) = tokio::join!(run_fut, canceller);
        r
    })
    .await
    .expect("cancel within 5s");
    assert!(
        matches!(res, Err(paigasus_helikon_core::RunError::Cancelled)),
        "{res:?}"
    );

    // The persisted log pairs the tool call with a synthesized result.
    let events = session.events(None).await.unwrap();
    let calls: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolCalled { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    let results: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolReturned { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(calls, vec!["c1"], "tool call persisted: {events:?}");
    assert_eq!(
        results,
        vec!["c1"],
        "synthesized result paired with the call: {events:?}"
    );

    // project() => no dangling tool call (the last message is the tool result).
    let snap = project(&events);
    assert!(
        matches!(snap.messages.last(), Some(Item::ToolResult { call_id, .. }) if call_id == "c1"),
        "projection must end in the matched tool result: {:?}",
        snap.messages
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_continues_from_history_without_new_turn() {
    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = RecordingModel::new(requests.clone(), vec![say("first"), say("second")]);
    let agent = text_agent(model, Vec::new());

    // Turn 1: a normal run with a new user turn.
    TokioRunner
        .run(
            &agent,
            run_context_with_session(session.clone()),
            AgentInput::from_user_text("hello"),
            RunConfig::default(),
        )
        .await
        .unwrap();

    // resume(): no new turn — continues from persisted history.
    TokioRunner
        .resume(
            &agent,
            run_context_with_session(session.clone()),
            RunConfig::default(),
        )
        .await
        .unwrap();

    // The resume request saw turn 1's messages, and added NO new user message.
    // Drop the lock guard before the subsequent `await` to satisfy clippy's
    // `await_holding_lock` lint (std::sync::MutexGuard must not cross an await).
    {
        let reqs = requests.lock().unwrap();
        let resume_req = &reqs[1];
        assert!(
            resume_req.iter().any(
                |m| matches!(m, Item::UserMessage { content } if content_text(content) == "hello")
            ),
            "resume must load prior history: {resume_req:?}"
        );
        let user_count = resume_req
            .iter()
            .filter(|m| matches!(m, Item::UserMessage { .. }))
            .count();
        assert_eq!(
            user_count, 1,
            "resume adds no new user message: {resume_req:?}"
        );
    } // lock guard dropped here, before the await below

    // Persisted log: [User hello, Asst first, Asst second] — no second user message.
    let events = session.events(None).await.unwrap();
    let user_events = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::UserMessage { .. }))
        .count();
    assert_eq!(
        user_events, 1,
        "resume persisted no extra user message: {events:?}"
    );
    assert_eq!(
        events.len(),
        3,
        "persisted log must be [User, Asst first, Asst second]: {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_streamed_persists_when_drained() {
    use futures_util::StreamExt as _;

    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model = RecordingModel::new(requests, vec![say("hi")]);
    let agent = text_agent(model, Vec::new());

    let handle = TokioRunner
        .run_streamed(
            &agent,
            run_context_with_session(session.clone()),
            AgentInput::from_user_text("yo"),
            RunConfig::default(),
        )
        .await
        .unwrap();

    // Drain the stream to its terminal so finalize runs.
    let mut events = handle.events;
    while events.next().await.is_some() {}

    let persisted = session.events(None).await.unwrap();
    assert_eq!(
        persisted.len(),
        2,
        "user + assistant persisted: {persisted:?}"
    );
    assert!(matches!(&persisted[0], SessionEvent::UserMessage { .. }));
    assert!(
        matches!(&persisted[1], SessionEvent::AssistantMessage { agent, .. } if agent == "test")
    );
}
