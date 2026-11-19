//! SMA-804: turn-boundary checkpoint persistence and cancellation under `TokioRunner`.

#[path = "common/mod.rs"]
mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use paigasus_helikon_core::{
    AgentEvent, AgentInput, CheckpointDecision, ContentPart, FinishReason, Guardrail,
    GuardrailError, GuardrailInput, GuardrailVerdict, MemorySession, ModelEvent, RunConfig,
    RunContext, RunError, Runner, Session, SessionEvent, Tool, ToolContext, ToolError, ToolOutput,
    TurnBoundary, TurnCheckpoint,
};
use paigasus_helikon_runtime_tokio::TokioRunner;
use tokio::sync::Notify;

use common::{
    run_context_with_session, run_context_with_session_and_cancel, text_agent, MockModel,
};

const LIMIT: Duration = Duration::from_secs(10);

/// Tool that returns `{"ok": true}` at once.
struct OkTool {
    schema: serde_json::Value,
}

#[async_trait]
impl Tool<()> for OkTool {
    fn name(&self) -> &str {
        "noop"
    }
    fn description(&self) -> &str {
        "returns ok"
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext<()>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(serde_json::json!({"ok": true})))
    }
}

fn ok_tool() -> Arc<dyn Tool<()>> {
    Arc::new(OkTool {
        schema: serde_json::json!({"type": "object"}),
    })
}

fn tool_turn() -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            call_id: "c0".into(),
            name: Some("noop".into()),
            args_delta: "{}".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]
}

fn text_turn(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta { text: text.into() },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

fn text(s: &str) -> ContentPart {
    ContentPart::Text { text: s.into() }
}

/// Pops one decision per call; `Continue` when empty.
struct ScriptedCheckpoint {
    decisions: Mutex<VecDeque<CheckpointDecision>>,
}

impl ScriptedCheckpoint {
    fn new(decisions: Vec<CheckpointDecision>) -> Arc<Self> {
        Arc::new(Self {
            decisions: Mutex::new(decisions.into()),
        })
    }
}

#[async_trait]
impl TurnCheckpoint<()> for ScriptedCheckpoint {
    async fn at_boundary(&self, _: &RunContext<()>, _: &TurnBoundary) -> CheckpointDecision {
        self.decisions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(CheckpointDecision::Continue)
    }
}

/// Signals `reached`, then waits forever.
struct HangingCheckpoint {
    reached: Arc<Notify>,
}

#[async_trait]
impl TurnCheckpoint<()> for HangingCheckpoint {
    async fn at_boundary(&self, _: &RunContext<()>, _: &TurnBoundary) -> CheckpointDecision {
        self.reached.notify_one();
        std::future::pending::<()>().await;
        CheckpointDecision::Continue
    }
}

/// Passes every input except the injected text "news", where it signals and hangs.
struct HangOnInjectedGuardrail {
    reached: Arc<Notify>,
}

#[async_trait]
impl Guardrail<()> for HangOnInjectedGuardrail {
    async fn check(
        &self,
        _: &RunContext<()>,
        input: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError> {
        if let GuardrailInput::UserText("news") = input {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(GuardrailVerdict::Pass)
    }
}

fn is_injected(e: &SessionEvent) -> bool {
    matches!(e, SessionEvent::UserMessage { content, .. }
        if matches!(content.as_slice(), [ContentPart::Text { text }] if text == "news"))
}

fn tool_returned_count(events: &[SessionEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, SessionEvent::ToolReturned { .. }))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn injected_message_is_persisted_after_tool_result() {
    let session = Arc::new(MemorySession::new());
    let model = MockModel::with_scripts(vec![tool_turn(), text_turn("done")]);
    let agent = text_agent(model, vec![ok_tool()]);
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Inject(vec![text("news")])]);
    let ctx =
        run_context_with_session(session.clone() as Arc<dyn Session>).with_turn_checkpoint(cp);

    let r = tokio::time::timeout(
        LIMIT,
        TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("hi"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("no hang");
    assert!(r.is_ok(), "{r:?}");

    let events = session.events(None).await.unwrap();
    assert!(
        matches!(events[0], SessionEvent::UserMessage { .. }),
        "{events:?}"
    );
    assert!(
        matches!(events[1], SessionEvent::ToolCalled { .. }),
        "{events:?}"
    );
    assert!(
        matches!(events[2], SessionEvent::ToolReturned { .. }),
        "{events:?}"
    );
    assert!(is_injected(&events[3]), "{events:?}");
    assert!(
        matches!(events[4], SessionEvent::AssistantMessage { .. }),
        "{events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_streamed_stop_finalizes_session() {
    let session = Arc::new(MemorySession::new());
    let model = MockModel::with_scripts(vec![tool_turn(), text_turn("never")]);
    let agent = text_agent(model, vec![ok_tool()]);
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Stop {
        reason: "enough".into(),
    }]);
    let ctx =
        run_context_with_session(session.clone() as Arc<dyn Session>).with_turn_checkpoint(cp);

    let mut rs = TokioRunner
        .run_streamed(
            &agent,
            ctx,
            AgentInput::from_user_text("hi"),
            RunConfig::default(),
        )
        .await
        .expect("run starts");
    let mut last = None;
    tokio::time::timeout(LIMIT, async {
        while let Some(e) = rs.events.next().await {
            last = Some(e);
        }
    })
    .await
    .expect("no hang");
    assert!(
        matches!(last, Some(AgentEvent::RunStopped { ref reason, .. }) if reason == "enough"),
        "{last:?}"
    );

    let events = session.events(None).await.unwrap();
    assert_eq!(tool_returned_count(&events), 1, "{events:?}");
    assert!(!events.iter().any(is_injected), "{events:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_while_checkpoint_waits() {
    let session = Arc::new(MemorySession::new());
    let cancel = paigasus_helikon_core::CancellationToken::new();
    let reached = Arc::new(Notify::new());
    let model = MockModel::with_scripts(vec![tool_turn(), text_turn("never")]);
    let agent = text_agent(model, vec![ok_tool()]);
    let cp = Arc::new(HangingCheckpoint {
        reached: reached.clone(),
    });
    let ctx =
        run_context_with_session_and_cancel(session.clone() as Arc<dyn Session>, cancel.clone())
            .with_turn_checkpoint(cp);

    let canceller = tokio::spawn(async move {
        reached.notified().await;
        cancel.cancel();
    });
    let r = tokio::time::timeout(
        LIMIT,
        TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("hi"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("no hang");
    canceller.await.unwrap();
    assert!(matches!(r, Err(RunError::Cancelled)), "{r:?}");

    let events = session.events(None).await.unwrap();
    assert_eq!(tool_returned_count(&events), 1, "{events:?}");
    assert!(!events.iter().any(is_injected), "{events:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_while_injection_guardrail_waits() {
    let session = Arc::new(MemorySession::new());
    let cancel = paigasus_helikon_core::CancellationToken::new();
    let reached = Arc::new(Notify::new());
    let model = MockModel::with_scripts(vec![tool_turn(), text_turn("never")]);
    let mut agent = text_agent(model, vec![ok_tool()]);
    agent.input_guardrails = vec![Arc::new(HangOnInjectedGuardrail {
        reached: reached.clone(),
    })];
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Inject(vec![text("news")])]);
    let ctx =
        run_context_with_session_and_cancel(session.clone() as Arc<dyn Session>, cancel.clone())
            .with_turn_checkpoint(cp);

    let canceller = tokio::spawn(async move {
        reached.notified().await;
        cancel.cancel();
    });
    let r = tokio::time::timeout(
        LIMIT,
        TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("hi"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("no hang");
    canceller.await.unwrap();
    assert!(matches!(r, Err(RunError::Cancelled)), "{r:?}");

    let events = session.events(None).await.unwrap();
    assert_eq!(tool_returned_count(&events), 1, "{events:?}");
    assert!(!events.iter().any(is_injected), "{events:?}");
}
