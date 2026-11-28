//! A cancelled model turn ends as `Cancelled`, never as a completed run (SMA-869).

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    project, AgentEvent, AgentInput, CancellationToken, CheckpointDecision, ContentPart,
    FinishReason, Hook, HookDecision, HookEvent, Item, LlmAgent, MemorySession, ModelError,
    ModelEvent, RunConfig, RunContext, RunError, Runner, Session, StopReason, Tool, ToolContext,
    ToolError, ToolOutput, TurnBoundary, TurnCheckpoint,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{
    run_context_with_cancel, run_context_with_cancel_and_hooks,
    run_context_with_session_and_cancel, text_agent, CancelScriptModel, Step,
};

fn delta(text: &str) -> Step {
    Step::Emit(ModelEvent::TokenDelta { text: text.into() })
}

fn finish() -> Step {
    Step::Emit(ModelEvent::Finish {
        reason: FinishReason::Stop,
    })
}

fn cancel_mid_text() -> Vec<Step> {
    vec![delta("par"), Step::Cancel, delta("tial"), finish()]
}

fn assistant_texts(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::AssistantMessage { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_text_stream_is_cancelled() {
    let token = CancellationToken::new();
    let model = CancelScriptModel::new(token.clone(), vec![cancel_mid_text()]);
    let agent = text_agent(model, Vec::new());
    let res = TokioRunner
        .run(
            &agent,
            run_context_with_cancel(token),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_text_stream_is_cancelled_streamed() {
    let token = CancellationToken::new();
    let model = CancelScriptModel::new(token.clone(), vec![cancel_mid_text()]);
    let agent = text_agent(model, Vec::new());
    let rs = TokioRunner
        .run_streamed(
            &agent,
            run_context_with_cancel(token),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("stream starts");
    // The streamed path has no `RunError`. A cancel ends the stream with the
    // synthetic `RunFailed { "run cancelled" }` terminal, never `RunCompleted`.
    let events: Vec<AgentEvent> = rs.events.collect().await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::RunCompleted { .. })),
        "a cancelled run must not complete: {events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::RunFailed { error }) if error == "run cancelled"
        ),
        "last event must be the cancel terminal: {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_text_turn_is_not_stored() {
    let token = CancellationToken::new();
    let model = CancelScriptModel::new(token.clone(), vec![cancel_mid_text()]);
    let agent = text_agent(model, Vec::new());
    let session = Arc::new(MemorySession::new());
    let res = TokioRunner
        .run(
            &agent,
            run_context_with_session_and_cancel(session.clone() as Arc<dyn Session>, token),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
    let events = session.events(None).await.unwrap();
    let snap = project(&events);
    let texts = assistant_texts(&snap.messages);
    assert!(
        !texts.iter().any(|t| t.contains("par")),
        "cut turn must not be stored: {texts:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_invoke_error_after_cancel_is_cancelled() {
    let token = CancellationToken::new();
    let model =
        CancelScriptModel::invoke_error_after_cancel(token.clone(), ModelError::Unavailable);
    let agent = text_agent(model, Vec::new());
    let res = TokioRunner
        .run(
            &agent,
            run_context_with_cancel(token),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_stream_error_after_cancel_is_cancelled() {
    let token = CancellationToken::new();
    let model = CancelScriptModel::new(
        token.clone(),
        vec![vec![
            delta("a"),
            Step::Cancel,
            Step::Fail(ModelError::Other(anyhow::anyhow!("reset"))),
        ]],
    );
    let agent = text_agent(model, Vec::new());
    let res = TokioRunner
        .run(
            &agent,
            run_context_with_cancel(token),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
}

/// A cancel after `Finish` must not undo a turn that already ended (goal 12).
///
/// This test is deterministic only while nothing returns `Pending` between the
/// end of the stream and `RunCompleted`. A later async step there lets the
/// cancel win. The failure then looks like a regression of check 6.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_after_finish_still_completes() {
    let token = CancellationToken::new();
    let model = CancelScriptModel::new(
        token.clone(),
        vec![vec![delta("done"), finish(), Step::Cancel]],
    );
    let agent = text_agent(model, Vec::new());
    let session = Arc::new(MemorySession::new());
    let res = TokioRunner
        .run(
            &agent,
            run_context_with_session_and_cancel(session.clone() as Arc<dyn Session>, token),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    let r = res.expect("a turn that finished must complete");
    assert_eq!(r.stop_reason, StopReason::Completed);
    assert_eq!(r.final_output, "done");
    let events = session.events(None).await.unwrap();
    let snap = project(&events);
    assert!(
        assistant_texts(&snap.messages).iter().any(|t| t == "done"),
        "finished turn must be stored: {:?}",
        snap.messages
    );
}

/// Regression guard: a stream with no `Finish` and no cancel still completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_turn_without_finish_and_without_cancel_completes() {
    let token = CancellationToken::new();
    let model = CancelScriptModel::new(token.clone(), vec![vec![delta("ok")]]);
    let agent = text_agent(model, Vec::new());
    let res = TokioRunner
        .run(
            &agent,
            run_context_with_cancel(token),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    let r = res.expect("must complete");
    assert_eq!(r.final_output, "ok");
}

/// Counts calls. Fires `cancel` when set, then returns `Ok` at once.
struct CountingTool {
    schema: serde_json::Value,
    calls: AtomicUsize,
    cancel: Option<CancellationToken>,
}

impl CountingTool {
    fn new(cancel: Option<CancellationToken>) -> Arc<Self> {
        Arc::new(Self {
            schema: serde_json::json!({"type": "object"}),
            calls: AtomicUsize::new(0),
            cancel,
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Tool<()> for CountingTool {
    fn name(&self) -> &str {
        "count"
    }
    fn description(&self) -> &str {
        "counts calls"
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext<()>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(c) = &self.cancel {
            c.cancel();
        }
        Ok(ToolOutput::new(serde_json::json!({"ok": true})))
    }
}

/// Fires the run token after a tool returned.
struct CancelAfterToolHook;

#[async_trait]
impl Hook<()> for CancelAfterToolHook {
    async fn on_event(&self, ctx: &RunContext<()>, event: &HookEvent) -> HookDecision {
        if matches!(event, HookEvent::PostToolUse { .. }) {
            ctx.cancel().cancel();
        }
        HookDecision::Allow
    }
}

/// Fires the run token when the first turn starts.
struct CancelOnTurnStartHook;

#[async_trait]
impl Hook<()> for CancelOnTurnStartHook {
    async fn on_event(&self, ctx: &RunContext<()>, event: &HookEvent) -> HookDecision {
        if matches!(event, HookEvent::OnTurnStart { .. }) {
            ctx.cancel().cancel();
        }
        HookDecision::Allow
    }
}

/// Counts `at_boundary` calls.
struct CountingCheckpoint {
    calls: AtomicUsize,
}

#[async_trait]
impl TurnCheckpoint<()> for CountingCheckpoint {
    async fn at_boundary(&self, _: &RunContext<()>, _: &TurnBoundary) -> CheckpointDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        CheckpointDecision::Continue
    }
}

fn tool_call_turn(name: &str) -> Vec<Step> {
    vec![
        Step::Emit(ModelEvent::ToolCallDelta {
            call_id: "c0".into(),
            name: Some(name.into()),
            args_delta: "{}".into(),
        }),
        Step::Emit(ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        }),
    ]
}

fn late_text_turn() -> Vec<Step> {
    vec![delta("late"), finish()]
}

/// Check 2 (at `TurnStarted`) stops this run before the second model call.
/// The tool-results batch holds `TurnStarted`. The test fails on `main`:
/// there the second turn starts and `model.calls()` is 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_between_turns_makes_no_second_model_call() {
    let token = CancellationToken::new();
    let model = CancelScriptModel::new(
        token.clone(),
        vec![tool_call_turn("count"), late_text_turn()],
    );
    let tool = CountingTool::new(None);
    let agent = text_agent(model.clone(), vec![tool as Arc<dyn Tool<()>>]);
    let ctx = run_context_with_cancel_and_hooks(token, vec![Arc::new(CancelAfterToolHook)]);
    let res = TokioRunner
        .run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
    assert_eq!(model.calls(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_between_turns_skips_the_turn_checkpoint() {
    let token = CancellationToken::new();
    let model = CancelScriptModel::new(
        token.clone(),
        vec![tool_call_turn("count"), late_text_turn()],
    );
    let tool = CountingTool::new(None);
    let agent = text_agent(model.clone(), vec![tool as Arc<dyn Tool<()>>]);
    let cp = Arc::new(CountingCheckpoint {
        calls: AtomicUsize::new(0),
    });
    let ctx = run_context_with_cancel_and_hooks(token, vec![Arc::new(CancelAfterToolHook)])
        .with_turn_checkpoint(cp.clone() as Arc<dyn TurnCheckpoint<()>>);
    let res = TokioRunner
        .run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
    assert_eq!(
        cp.calls.load(Ordering::SeqCst),
        0,
        "checkpoint must not run"
    );
}

/// A cancel in the `OnTurnStart` hook fires after check 2 passed. Check 3 (at
/// the model call) alone stops this run, so the model gets no call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_in_turn_start_hook_makes_no_model_call() {
    let token = CancellationToken::new();
    let model = CancelScriptModel::new(token.clone(), vec![late_text_turn()]);
    let agent = text_agent(model.clone(), Vec::new());
    let ctx = run_context_with_cancel_and_hooks(token, vec![Arc::new(CancelOnTurnStartHook)]);
    let res = TokioRunner
        .run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
    assert_eq!(model.calls(), 0);
}

/// Check 2 (at `TurnStarted`) stops this run before the second model call.
/// This is not a guard: the test fails on `main`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_while_a_tool_runs_is_cancelled() {
    let token = CancellationToken::new();
    let model = CancelScriptModel::new(
        token.clone(),
        vec![tool_call_turn("count"), late_text_turn()],
    );
    let tool = CountingTool::new(Some(token.clone()));
    let agent = text_agent(model.clone(), vec![tool as Arc<dyn Tool<()>>]);
    let res = TokioRunner
        .run(
            &agent,
            run_context_with_cancel(token),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
    assert_eq!(model.calls(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_after_tool_call_finish_runs_no_tool() {
    let token = CancellationToken::new();
    let mut turn = tool_call_turn("count");
    turn.push(Step::Cancel);
    let model = CancelScriptModel::new(token.clone(), vec![turn]);
    let tool = CountingTool::new(None);
    let agent = text_agent(model, vec![tool.clone() as Arc<dyn Tool<()>>]);
    let res = TokioRunner
        .run(
            &agent,
            run_context_with_cancel(token),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
    assert_eq!(tool.calls(), 0, "no tool may run after a cancel");
}

/// Through `run_streamed` a cancel is the synthetic `RunFailed { "run cancelled" }`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_after_transfer_finish_does_no_handoff() {
    let token = CancellationToken::new();
    let mut turn = tool_call_turn("transfer_to_target");
    turn.push(Step::Cancel);
    let model = CancelScriptModel::new(token.clone(), vec![turn]);
    let target_model = CancelScriptModel::new(token.clone(), vec![late_text_turn()]);
    let target = LlmAgent::builder::<()>()
        .name("target")
        .shared_model(target_model)
        .build();
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(model)
        .handoff(target)
        .build();
    let rs = TokioRunner
        .run_streamed(
            &agent,
            run_context_with_cancel(token),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("stream starts");
    let events: Vec<AgentEvent> = rs.events.collect().await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::HandoffItem { .. })),
        "no handoff after a cancel: {events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::RunFailed { error }) if error == "run cancelled"
        ),
        "last event must be the cancel terminal: {events:?}"
    );
}

/// A consumer can cancel while `HandoffItem` is delivered. The live handoff
/// path checks the token again, so the target does not start and no
/// "transferred to" result appears.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_on_handoff_item_starts_no_target() {
    let token = CancellationToken::new();
    let model = CancelScriptModel::new(token.clone(), vec![tool_call_turn("transfer_to_target")]);
    let target_model = CancelScriptModel::new(token.clone(), vec![late_text_turn()]);
    let target = LlmAgent::builder::<()>()
        .name("target")
        .shared_model(target_model)
        .build();
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(model)
        .handoff(target)
        .build();
    let rs = TokioRunner
        .run_streamed(
            &agent,
            run_context_with_cancel(token.clone()),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("stream starts");
    let mut events = Vec::new();
    let mut stream = rs.events;
    while let Some(ev) = stream.next().await {
        if matches!(ev, AgentEvent::HandoffItem { .. }) {
            token.cancel();
        }
        events.push(ev);
    }
    let handoff_at = events
        .iter()
        .position(|e| matches!(e, AgentEvent::HandoffItem { .. }))
        .expect("the handoff item is emitted");
    assert!(
        !events[handoff_at..]
            .iter()
            .any(|e| matches!(e, AgentEvent::AgentUpdated { .. })),
        "the target must not start: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolOutputItem { item: Item::ToolResult { content, .. } }
                if content.iter().any(|p| matches!(
                    p, ContentPart::Text { text } if text.contains("transferred to")
                ))
        )),
        "no transfer result for a target that did not run: {events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::RunFailed { error }) if error == "run cancelled"
        ),
        "last event must be the cancel terminal: {events:?}"
    );
}
