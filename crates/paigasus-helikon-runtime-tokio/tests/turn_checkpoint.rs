//! SMA-804: turn-boundary checkpoint persistence and cancellation under `TokioRunner`.

#[path = "common/mod.rs"]
mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use paigasus_helikon_core::{
    open_deferrals, AgentEvent, AgentInput, CheckpointDecision, ContentPart, FinishReason,
    Guardrail, GuardrailError, GuardrailInput, GuardrailVerdict, MemorySession, ModelEvent,
    PermissionDecision, PermissionPolicy, RunConfig, RunContext, RunError, Runner, Session,
    SessionEvent, StopReason, Tool, ToolContext, ToolError, ToolOutput, ToolResolution,
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
    tokio::time::timeout(LIMIT, canceller)
        .await
        .expect("canceller notified")
        .unwrap();
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
    tokio::time::timeout(LIMIT, canceller)
        .await
        .expect("canceller notified")
        .unwrap();
    assert!(matches!(r, Err(RunError::Cancelled)), "{r:?}");

    let events = session.events(None).await.unwrap();
    assert_eq!(tool_returned_count(&events), 1, "{events:?}");
    assert!(!events.iter().any(is_injected), "{events:?}");
}

/// Defers every call to `pay`; allows the rest.
struct DeferPay;

#[async_trait]
impl PermissionPolicy<()> for DeferPay {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        tool: &str,
        _args: &serde_json::Value,
    ) -> PermissionDecision {
        if tool == "pay" {
            PermissionDecision::Defer {
                prompt: "pay?".to_owned(),
            }
        } else {
            PermissionDecision::Allow
        }
    }
}

/// The `pay` tool. It counts its calls.
struct PayTool {
    calls: Mutex<usize>,
    schema: serde_json::Value,
}

#[async_trait]
impl Tool<()> for PayTool {
    fn name(&self) -> &str {
        "pay"
    }
    fn description(&self) -> &str {
        "pays"
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext<()>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        *self.calls.lock().unwrap() += 1;
        Ok(ToolOutput::new(serde_json::json!({"paid": true})))
    }
}

/// Pops one decision per call and keeps every boundary it sees.
struct RecordingCheckpoint {
    decisions: Mutex<VecDeque<CheckpointDecision>>,
    seen: Mutex<Vec<TurnBoundary>>,
}

impl RecordingCheckpoint {
    fn new(decisions: Vec<CheckpointDecision>) -> Arc<Self> {
        Arc::new(Self {
            decisions: Mutex::new(decisions.into()),
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl TurnCheckpoint<()> for RecordingCheckpoint {
    async fn at_boundary(&self, _: &RunContext<()>, b: &TurnBoundary) -> CheckpointDecision {
        self.seen.lock().unwrap().push(b.clone());
        self.decisions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(CheckpointDecision::Continue)
    }
}

fn pay_turn(call_id: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            call_id: call_id.into(),
            name: Some("pay".into()),
            args_delta: "{}".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]
}

/// A run injects at a boundary and then defers `pay`. The resume runs `pay`
/// once. A later boundary on the resumed run calls the checkpoint, which stops
/// the run. The checkpoint is not called at the deferred boundary or at the
/// resume step.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inject_then_defer_then_resume() {
    let session = Arc::new(MemorySession::new());
    let pay = Arc::new(PayTool {
        calls: Mutex::new(0),
        schema: serde_json::json!({"type": "object"}),
    });
    let tools = || vec![ok_tool(), pay.clone() as Arc<dyn Tool<()>>];
    let ctx_for = |cp: Arc<RecordingCheckpoint>| {
        run_context_with_session(session.clone() as Arc<dyn Session>)
            .with_permission_policy(Arc::new(DeferPay))
            .with_deferred_tool_calls(true)
            .with_turn_checkpoint(cp)
    };

    // Run 1: a noop turn, an injected "news", then a `pay` turn that defers.
    let model = MockModel::with_scripts(vec![tool_turn(), pay_turn("call_1")]);
    let agent = text_agent(model, tools());
    let cp1 = RecordingCheckpoint::new(vec![CheckpointDecision::Inject(vec![text("news")])]);
    let r1 = tokio::time::timeout(
        LIMIT,
        TokioRunner.run(
            &agent,
            ctx_for(cp1.clone()),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("no hang")
    .expect("run 1");
    assert!(
        matches!(r1.stop_reason, StopReason::Deferred { .. }),
        "{:?}",
        r1.stop_reason
    );
    assert_eq!(
        cp1.seen.lock().unwrap().len(),
        1,
        "the checkpoint is not called at the deferred boundary"
    );
    let events = session.events(None).await.unwrap();
    assert!(events.iter().any(is_injected), "{events:?}");
    assert_eq!(*pay.calls.lock().unwrap(), 0, "pay is deferred, not run");

    // Run 2: approve the deferral. The checkpoint stops at the first boundary.
    let open = open_deferrals(&events);
    assert_eq!(open.len(), 1, "{open:?}");
    let input = AgentInput::from_user_text("next")
        .with_resolution(open[0].deferral_id().to_owned(), ToolResolution::Approved);
    let model2 = MockModel::with_scripts(vec![tool_turn(), text_turn("never")]);
    let agent2 = text_agent(model2, tools());
    let cp2 = RecordingCheckpoint::new(vec![CheckpointDecision::Stop {
        reason: "halt".into(),
    }]);
    let r2 = tokio::time::timeout(
        LIMIT,
        TokioRunner.run(&agent2, ctx_for(cp2.clone()), input, RunConfig::default()),
    )
    .await
    .expect("no hang")
    .expect("resume");
    assert_eq!(
        r2.stop_reason,
        StopReason::Stopped {
            reason: "halt".into()
        }
    );
    assert_eq!(*pay.calls.lock().unwrap(), 1, "pay runs once on the resume");
    let seen = cp2.seen.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        1,
        "no checkpoint call at the resume step: {seen:?}"
    );
    assert_eq!((seen[0].completed_turn, seen[0].next_turn), (0, 1));
    let events = session.events(None).await.unwrap();
    assert!(open_deferrals(&events).is_empty(), "{events:?}");
}
