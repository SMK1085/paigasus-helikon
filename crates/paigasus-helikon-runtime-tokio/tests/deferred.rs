//! SMA-803 — `TokioRunner` records a deferral, resumes it, and fails a run
//! whose deferral or resume write is lost.

#[path = "common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::MockModel;
use futures_core::stream::BoxStream;
use futures_util::stream::StreamExt as _;
use paigasus_helikon_core::{
    open_deferrals, Agent, AgentEvent, AgentInput, CancellationToken, ContentPart,
    ConversationSnapshot, FinishReason, Guardrail, GuardrailError, GuardrailInput, GuardrailKind,
    GuardrailVerdict, Hook, HookDecision, HookEvent, InvalidResume, Item, LlmAgent, MemorySession,
    Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest, PermissionDecision,
    PermissionPolicy, RunConfig, RunContext, RunError, RunResult, Runner, SequenceId,
    SequentialAgent, Session, SessionError, SessionEvent, StopReason, Tool, ToolContext, ToolError,
    ToolOutput, ToolResolution,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

const SECRET: &str = "sk-test-SECRET-1234";
const NOT_RUN: &str = "tool call did not run: the run ended before the resume completed";
const CANCELLED: &str = "tool call did not complete (run cancelled/timed out)";

// ── Fixtures ─────────────────────────────────────────────────────────────────

/// Defers every call to `pay`; allows the rest.
struct DeferPolicy;

#[async_trait]
impl PermissionPolicy<()> for DeferPolicy {
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

/// Defers every call to `pay` with a prompt that quotes the args (so the
/// prompt can carry a secret from the args); allows the rest.
struct PromptFromArgs;

#[async_trait]
impl PermissionPolicy<()> for PromptFromArgs {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        tool: &str,
        args: &serde_json::Value,
    ) -> PermissionDecision {
        if tool == "pay" {
            PermissionDecision::Defer {
                prompt: format!("pay {args}?"),
            }
        } else {
            PermissionDecision::Allow
        }
    }
}

/// A [`MockModel`] that keeps every request it gets.
struct RecordingModel {
    inner: Arc<MockModel>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl RecordingModel {
    fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            inner: MockModel::with_scripts(scripts),
            requests: Mutex::new(Vec::new()),
        })
    }
    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Model for RecordingModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.invoke(request, cancel).await
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// The `pay` tool. It records the args of each call. With a gate, it
/// notifies the gate and then never returns.
struct PayTool {
    calls: Mutex<Vec<serde_json::Value>>,
    gate: Option<Arc<tokio::sync::Notify>>,
    schema: serde_json::Value,
}

impl PayTool {
    fn new() -> Arc<Self> {
        Self::build(None)
    }
    fn blocking(gate: Arc<tokio::sync::Notify>) -> Arc<Self> {
        Self::build(Some(gate))
    }
    fn build(gate: Option<Arc<tokio::sync::Notify>>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            gate,
            schema: serde_json::json!({ "type": "object" }),
        })
    }
    fn calls(&self) -> Vec<serde_json::Value> {
        self.calls.lock().unwrap().clone()
    }
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
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.calls.lock().unwrap().push(args);
        if let Some(gate) = &self.gate {
            gate.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(ToolOutput::new(serde_json::json!({ "paid": true })))
    }
}

/// A session whose `append` fails. Reads go to the inner session.
struct FailingAppendSession(Arc<MemorySession>);

#[async_trait]
impl Session for FailingAppendSession {
    async fn append(&self, _events: &[SessionEvent]) -> Result<(), SessionError> {
        Err(SessionError::Unavailable)
    }
    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        self.0.events(since).await
    }
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        self.0.snapshot().await
    }
}

/// A session whose snapshot has no `ToolCall` items (as after a third-party
/// compactor). The rest goes to the inner session.
struct NoToolCallSnapshot(Arc<MemorySession>);

#[async_trait]
impl Session for NoToolCallSnapshot {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        self.0.append(events).await
    }
    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        self.0.events(since).await
    }
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        let mut snapshot = self.0.snapshot().await?;
        snapshot
            .messages
            .retain(|i| !matches!(i, Item::ToolCall { .. }));
        Ok(snapshot)
    }
}

/// Adds the secret to the args of every `pay` call.
struct InjectSecret;

#[async_trait]
impl Hook<()> for InjectSecret {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        match event {
            HookEvent::PreToolUse { tool, .. } if tool == "pay" => HookDecision::ReplaceInput {
                value: serde_json::json!({ "amount": 5, "token": SECRET }),
            },
            _ => HookDecision::Allow,
        }
    }
}

/// Trips on every input.
struct TripAlways;

#[async_trait]
impl Guardrail<()> for TripAlways {
    async fn check(
        &self,
        _: &RunContext<()>,
        _: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError> {
        Ok(GuardrailVerdict::Tripwire {
            kind: GuardrailKind::InputPolicy,
            info: serde_json::Value::Null,
        })
    }
}

fn recorded_args() -> serde_json::Value {
    serde_json::json!({ "amount": 5 })
}

/// A model turn that calls `pay` once per call id, with [`recorded_args`].
fn pay_turn(call_ids: &[&str]) -> Vec<ModelEvent> {
    let mut turn: Vec<ModelEvent> = call_ids
        .iter()
        .map(|id| ModelEvent::ToolCallDelta {
            call_id: (*id).to_owned(),
            name: Some("pay".to_owned()),
            args_delta: recorded_args().to_string(),
        })
        .collect();
    turn.push(ModelEvent::Finish {
        reason: FinishReason::ToolCalls,
    });
    turn
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

fn transfer_turn(call_id: &str, target: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            call_id: call_id.to_owned(),
            name: Some(format!("transfer_to_{target}")),
            args_delta: "{}".to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]
}

/// An agent named `name` with the `pay` tool and the scripted model.
fn pay_agent(
    name: &str,
    model: Arc<RecordingModel>,
    pay: &Arc<PayTool>,
) -> LlmAgent<(), RecordingModel> {
    LlmAgent::builder::<()>()
        .name(name)
        .shared_model(model)
        .shared_tool(Arc::clone(pay) as Arc<dyn Tool<()>>)
        .build()
}

/// A context on `session` with the defer policy and the deferral flag.
fn ctx(session: Arc<dyn Session>, deferral: bool) -> RunContext<()> {
    RunContext::ephemeral(())
        .with_session(session)
        .with_permission_policy(Arc::new(DeferPolicy))
        .with_deferred_tool_calls(deferral)
}

async fn run(
    agent: &(dyn Agent<()> + '_),
    ctx: RunContext<()>,
    input: AgentInput,
) -> Result<RunResult, RunError> {
    TokioRunner
        .run(agent, ctx, input, RunConfig::default())
        .await
}

async fn run_streamed_events(
    agent: &(dyn Agent<()> + '_),
    ctx: RunContext<()>,
    input: AgentInput,
) -> Vec<AgentEvent> {
    TokioRunner
        .run_streamed(agent, ctx, input, RunConfig::default())
        .await
        .expect("stream starts")
        .events
        .collect()
        .await
}

async fn collect_streamed(
    agent: &(dyn Agent<()> + '_),
    ctx: RunContext<()>,
    input: AgentInput,
) -> Result<RunResult, RunError> {
    TokioRunner
        .run_streamed(agent, ctx, input, RunConfig::default())
        .await
        .expect("stream starts")
        .collect()
        .await
}

fn assert_session_write(result: &Result<RunResult, RunError>) {
    assert!(
        matches!(
            result,
            Err(RunError::SessionWrite(SessionError::Unavailable))
        ),
        "{result:?}"
    );
}

async fn log(session: &MemorySession) -> Vec<SessionEvent> {
    session.events(None).await.unwrap()
}

/// The deferral ids of the open deferrals, in log order.
async fn open_ids(session: &MemorySession) -> Vec<String> {
    open_deferrals(&log(session).await)
        .iter()
        .map(|c| c.deferral_id().to_owned())
        .collect()
}

fn text_of(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn user(text: &str) -> AgentInput {
    AgentInput::from_user_text(text)
}

/// A short label for each session event.
fn label(e: &SessionEvent) -> String {
    match e {
        SessionEvent::UserMessage { content, .. } => format!("user_message({})", text_of(content)),
        SessionEvent::AssistantMessage { .. } => "assistant_message".to_owned(),
        SessionEvent::ToolCalled { call_id, .. } => format!("tool_called({call_id})"),
        SessionEvent::ToolReturned {
            call_id, content, ..
        } => format!("tool_returned({call_id}, {})", text_of(content)),
        SessionEvent::ToolDeferred { call_id, .. } => format!("tool_deferred({call_id})"),
        SessionEvent::HandoffOccurred { from, to, .. } => format!("handoff({from}->{to})"),
        _ => "other".to_owned(),
    }
}

async fn labels(session: &MemorySession) -> Vec<String> {
    log(session).await.iter().map(label).collect()
}

/// Run `pay_agent("root")` that defers `pay` once per call id into a new
/// session. Returns the session.
async fn deferred_session(call_ids: &[&str]) -> Arc<MemorySession> {
    let session = Arc::new(MemorySession::new());
    let model = RecordingModel::with_scripts(vec![pay_turn(call_ids)]);
    let agent = pay_agent("root", model, &PayTool::new());
    let result = run(&agent, ctx(session.clone(), true), user("go"))
        .await
        .expect("the deferring run succeeds");
    assert!(
        matches!(&result.stop_reason, StopReason::Deferred { calls } if calls.len() == call_ids.len()),
        "{:?}",
        result.stop_reason
    );
    session
}

/// The input that resolves every open deferral of `session` with
/// `resolution`, then adds the user message `"next"`.
async fn resolve_all(session: &MemorySession, resolution: ToolResolution) -> AgentInput {
    let mut input = user("next");
    for id in open_ids(session).await {
        input = input.with_resolution(id, resolution.clone());
    }
    input
}

fn invalid_resume(result: Result<RunResult, RunError>) -> InvalidResume {
    match result {
        Err(RunError::InvalidResume(reason)) => reason,
        other => panic!("expected InvalidResume, got {other:?}"),
    }
}

/// Each `ToolCall` has a `ToolResult` with the same call id later, with no
/// `UserMessage` or `AssistantMessage` between them. Each `ToolResult` closes
/// one earlier open `ToolCall` with its call id. A call id can repeat only
/// after its earlier call has a result.
///
/// Copy of `assert_tool_calls_paired` in
/// `crates/paigasus-helikon-core/tests/deferred_resume.rs` (pre-flight ruling
/// F5). Keep the two copies the same.
fn assert_tool_calls_paired(messages: &[Item]) {
    // The calls that wait for a result, in call order.
    let mut outstanding: Vec<&str> = Vec::new();
    // A result came after the last call: the next call starts a new round.
    let mut seen_result_since_call = false;
    for item in messages {
        match item {
            Item::ToolCall { call_id, .. } => {
                assert!(
                    !outstanding.contains(&call_id.as_str()),
                    "call {call_id} repeats while it waits for a result: {messages:?}"
                );
                assert!(
                    !(seen_result_since_call && !outstanding.is_empty()),
                    "call {call_id} starts a new round while {outstanding:?} wait for a result: {messages:?}"
                );
                seen_result_since_call = false;
                outstanding.push(call_id);
            }
            Item::ToolResult { call_id, .. } => {
                let Some(p) = outstanding.iter().position(|c| *c == call_id.as_str()) else {
                    panic!("result {call_id} has no open earlier call: {messages:?}");
                };
                // Each result closes one call.
                outstanding.remove(p);
                seen_result_since_call = true;
            }
            Item::UserMessage { .. } | Item::AssistantMessage { .. } => {
                assert!(
                    outstanding.is_empty(),
                    "tool calls {outstanding:?} are not paired: {messages:?}"
                );
            }
            _ => {}
        }
    }
    assert!(
        outstanding.is_empty(),
        "tool calls {outstanding:?} are not paired: {messages:?}"
    );
}

/// The last three items of `messages` are the `pay` call `call_id`, its
/// result and the user message `"next"`.
fn assert_ends_with_result_then_next(messages: &[Item], call_id: &str) {
    let n = messages.len();
    assert!(n >= 3, "{messages:?}");
    assert!(
        matches!(&messages[n - 3], Item::ToolCall { call_id: id, name, .. }
            if id == call_id && name == "pay"),
        "{messages:?}"
    );
    assert!(
        matches!(&messages[n - 2], Item::ToolResult { call_id: id, .. } if id == call_id),
        "{messages:?}"
    );
    assert!(
        matches!(&messages[n - 1], Item::UserMessage { content } if text_of(content) == "next"),
        "{messages:?}"
    );
}

// ── Recording a deferral ─────────────────────────────────────────────────────

#[tokio::test]
async fn deferral_round_trip_stores_raw_args() {
    let session = Arc::new(MemorySession::new());
    let model = RecordingModel::with_scripts(vec![pay_turn(&["call_1"])]);
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(model)
        .shared_tool(PayTool::new() as Arc<dyn Tool<()>>)
        .hook(InjectSecret)
        .build();
    let ctx = ctx(session.clone(), true)
        .with_permission_policy(Arc::new(PromptFromArgs))
        .with_extra_secrets(vec![SECRET.to_owned()]);
    assert!(ctx.redact_output(), "output redaction is on by default");

    let result = run(&agent, ctx, user("go")).await.expect("run succeeds");

    let raw = serde_json::json!({ "amount": 5, "token": SECRET });
    let stored = log(&session).await;
    let deferred_args = stored
        .iter()
        .find_map(|e| match e {
            SessionEvent::ToolDeferred { args, .. } => Some(args.clone()),
            _ => None,
        })
        .expect("a stored ToolDeferred");
    assert_eq!(deferred_args, raw, "the session stores the raw args");
    let deferred_prompt = stored
        .iter()
        .find_map(|e| match e {
            SessionEvent::ToolDeferred { prompt, .. } => Some(prompt.clone()),
            _ => None,
        })
        .expect("a stored ToolDeferred");
    assert!(
        deferred_prompt.contains(SECRET),
        "the session stores the raw prompt: {deferred_prompt}"
    );

    // The event copy comes from a real redaction pass: it keeps the plain
    // field and loses the secret.
    let StopReason::Deferred { calls } = &result.stop_reason else {
        panic!("expected Deferred, got {:?}", result.stop_reason);
    };
    assert_eq!(calls[0].args()["amount"], 5);
    assert_ne!(calls[0].args(), &raw);
    assert!(
        calls[0].prompt().starts_with("pay "),
        "{}",
        calls[0].prompt()
    );
    assert!(!calls[0].prompt().contains(SECRET), "{}", calls[0].prompt());
    let stop = serde_json::to_string(&result.stop_reason).unwrap();
    assert!(!stop.contains(SECRET), "{stop}");
    for ev in &result.events {
        match ev {
            AgentEvent::RunDeferred { calls, .. } => {
                for c in calls {
                    let args = c.args().to_string();
                    assert!(!args.contains(SECRET), "{args}");
                    assert!(!c.prompt().contains(SECRET), "{}", c.prompt());
                }
            }
            AgentEvent::ApprovalRequested { args, .. } => {
                let args = args.to_string();
                assert!(!args.contains(SECRET), "{args}");
            }
            _ => {}
        }
    }
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::ApprovalRequested { .. })),
        "{:?}",
        result.events
    );

    let ls = labels(&session).await;
    assert_eq!(
        ls[ls.len() - 2..],
        ["tool_called(call_1)", "tool_deferred(call_1)"],
        "{ls:?}"
    );
}

#[tokio::test]
async fn run_streamed_ends_with_run_deferred_after_finalize() {
    let session = Arc::new(MemorySession::new());
    let model = RecordingModel::with_scripts(vec![pay_turn(&["call_1"])]);
    let agent = pay_agent("root", model, &PayTool::new());
    let mut stream = TokioRunner
        .run_streamed(
            &agent,
            ctx(session.clone(), true),
            user("go"),
            RunConfig::default(),
        )
        .await
        .expect("stream starts")
        .events;

    let mut last = None;
    while let Some(ev) = stream.next().await {
        if matches!(ev, AgentEvent::RunDeferred { .. }) {
            let stored = log(&session).await;
            let args = stored.iter().find_map(|e| match e {
                SessionEvent::ToolDeferred { args, .. } => Some(args.clone()),
                _ => None,
            });
            assert_eq!(
                args,
                Some(recorded_args()),
                "the session holds the deferral when RunDeferred arrives: {stored:?}"
            );
        }
        last = Some(ev);
    }
    assert!(
        matches!(last, Some(AgentEvent::RunDeferred { .. })),
        "{last:?}"
    );
    assert_eq!(open_ids(&session).await.len(), 1);
}

// ── Resume ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn resume_approved_runs_the_tool_once_with_recorded_args() {
    let session = deferred_session(&["call_1"]).await;
    let pay = PayTool::new();
    let model = RecordingModel::with_scripts(vec![text_turn("done")]);
    let agent = pay_agent("root", Arc::clone(&model), &pay);
    let input = resolve_all(&session, ToolResolution::Approved).await;

    let result = run(&agent, ctx(session.clone(), true), input)
        .await
        .expect("resume succeeds");

    assert_eq!(result.stop_reason, StopReason::Completed);
    assert_eq!(result.final_output, "done");
    assert_eq!(pay.calls(), [recorded_args()], "pay runs once");
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert_tool_calls_paired(&requests[0].messages);
    assert_ends_with_result_then_next(&requests[0].messages, "call_1");
    assert!(open_ids(&session).await.is_empty());
    let ls = labels(&session).await;
    assert_eq!(
        ls[3..],
        [
            r#"tool_returned(call_1, {"paid":true})"#,
            "user_message(next)",
            "assistant_message",
        ],
        "{ls:?}"
    );
}

#[tokio::test]
async fn resume_denied_records_the_denial() {
    let session = deferred_session(&["call_1"]).await;
    let pay = PayTool::new();
    let model = RecordingModel::with_scripts(vec![text_turn("ok")]);
    let agent = pay_agent("root", Arc::clone(&model), &pay);
    let input = resolve_all(
        &session,
        ToolResolution::Denied {
            reason: "too much".to_owned(),
        },
    )
    .await;

    let result = run(&agent, ctx(session.clone(), true), input)
        .await
        .expect("resume succeeds");

    assert_eq!(result.final_output, "ok");
    assert!(pay.calls().is_empty(), "a denied call does not run");
    assert!(open_ids(&session).await.is_empty());
    let ls = labels(&session).await;
    assert_eq!(
        ls[3..5],
        [
            "tool_returned(call_1, permission denied: too much)",
            "user_message(next)",
        ],
        "{ls:?}"
    );
    assert_ends_with_result_then_next(&model.requests()[0].messages, "call_1");
}

#[tokio::test]
async fn repeated_call_id_does_not_match_an_old_deferral() {
    // The provider reuses the call id `fc_0` in every turn.
    let session = deferred_session(&["fc_0"]).await;
    let d1 = open_ids(&session).await.remove(0);
    let pay = PayTool::new();
    let model = RecordingModel::with_scripts(vec![text_turn("paid"), pay_turn(&["fc_0"])]);
    let agent = pay_agent("root", Arc::clone(&model), &pay);

    run(
        &agent,
        ctx(session.clone(), true),
        user("next").with_resolution(&d1, ToolResolution::Approved),
    )
    .await
    .expect("the first resume succeeds");
    let result = run(&agent, ctx(session.clone(), true), user("again"))
        .await
        .expect("the second deferral succeeds");
    assert!(matches!(result.stop_reason, StopReason::Deferred { .. }));
    let open = open_ids(&session).await;
    assert_eq!(open.len(), 1);
    let d2 = open[0].clone();
    assert_ne!(d1, d2);

    let len = log(&session).await.len();
    let reason = invalid_resume(
        run(
            &agent,
            ctx(session.clone(), true),
            user("x").with_resolution(&d1, ToolResolution::Approved),
        )
        .await,
    );
    assert_eq!(reason, InvalidResume::UnknownDeferral { deferral_id: d1 });
    assert_eq!(log(&session).await.len(), len, "nothing is appended");

    let model = RecordingModel::with_scripts(vec![text_turn("paid again")]);
    let agent = pay_agent("root", Arc::clone(&model), &pay);
    run(
        &agent,
        ctx(session.clone(), true),
        user("next").with_resolution(&d2, ToolResolution::Approved),
    )
    .await
    .expect("resolving the new deferral succeeds");
    assert_eq!(pay.calls().len(), 2);
    assert!(open_ids(&session).await.is_empty());
}

/// Run a handoff chain `triage -> budgeting` whose `budgeting` defers `pay`,
/// into a new session. Returns the session.
async fn handoff_session() -> Arc<MemorySession> {
    let session = Arc::new(MemorySession::new());
    let budgeting = pay_agent(
        "budgeting",
        RecordingModel::with_scripts(vec![pay_turn(&["call_1"])]),
        &PayTool::new(),
    );
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(RecordingModel::with_scripts(vec![transfer_turn(
            "t1",
            "budgeting",
        )]))
        .handoff(budgeting)
        .build();
    let result = run(&triage, ctx(session.clone(), true), user("hi"))
        .await
        .expect("the deferring run succeeds");
    assert!(
        matches!(result.stop_reason, StopReason::Deferred { .. }),
        "{:?}",
        result.stop_reason
    );
    session
}

#[tokio::test]
async fn invalid_resume_reasons_reach_the_caller() {
    let agent = pay_agent(
        "root",
        RecordingModel::with_scripts(vec![text_turn("never")]),
        &PayTool::new(),
    );

    // NoOpenDeferrals: a resolution on a clean session.
    let clean = Arc::new(MemorySession::new());
    let reason = invalid_resume(
        run(
            &agent,
            ctx(clean.clone(), true),
            user("x").with_resolution("d", ToolResolution::Approved),
        )
        .await,
    );
    assert_eq!(reason, InvalidResume::NoOpenDeferrals);
    assert!(log(&clean).await.is_empty());

    // UnresolvedDeferrals: Runner::resume sends no resolution.
    let session = deferred_session(&["call_1"]).await;
    let d = open_ids(&session).await.remove(0);
    let len = log(&session).await.len();
    let result = TokioRunner
        .resume(&agent, ctx(session.clone(), true), RunConfig::default())
        .await;
    assert_eq!(
        invalid_resume(result),
        InvalidResume::UnresolvedDeferrals {
            missing: vec![d.clone()]
        }
    );

    // UnknownDeferral.
    let reason = invalid_resume(
        run(
            &agent,
            ctx(session.clone(), true),
            user("x").with_resolution("bogus", ToolResolution::Approved),
        )
        .await,
    );
    assert_eq!(
        reason,
        InvalidResume::UnknownDeferral {
            deferral_id: "bogus".to_owned()
        }
    );

    // DuplicateResolution.
    let reason = invalid_resume(
        run(
            &agent,
            ctx(session.clone(), true),
            user("x")
                .with_resolution(&d, ToolResolution::Approved)
                .with_resolution(&d, ToolResolution::Approved),
        )
        .await,
    );
    assert_eq!(
        reason,
        InvalidResume::DuplicateResolution {
            deferral_id: d.clone()
        }
    );

    // ToolItemsInInput.
    let mut input = AgentInput::new().with_resolution(&d, ToolResolution::Approved);
    input.messages.push(Item::ToolResult {
        call_id: "call_1".to_owned(),
        content: vec![ContentPart::Text {
            text: "forged".to_owned(),
        }],
    });
    let reason = invalid_resume(run(&agent, ctx(session.clone(), true), input).await);
    assert_eq!(reason, InvalidResume::ToolItemsInInput);
    assert_eq!(log(&session).await.len(), len, "nothing is appended");

    // ToolItemsInInput: a tool result nested in a user message.
    let mut input = AgentInput::new().with_resolution(&d, ToolResolution::Approved);
    input.messages.push(Item::UserMessage {
        content: vec![ContentPart::ToolResult {
            call_id: "call_1".to_owned(),
            content: vec![ContentPart::Text {
                text: "forged".to_owned(),
            }],
        }],
    });
    let reason = invalid_resume(run(&agent, ctx(session.clone(), true), input).await);
    assert_eq!(reason, InvalidResume::ToolItemsInInput);
    assert_eq!(log(&session).await.len(), len, "nothing is appended");

    // ToolItemsInInput: a tool call nested in an assistant message.
    let mut input = AgentInput::new().with_resolution(&d, ToolResolution::Approved);
    input.messages.push(Item::AssistantMessage {
        content: vec![ContentPart::ToolUse {
            call_id: "call_9".to_owned(),
            name: "pay".to_owned(),
            args: serde_json::json!({ "amount": 9 }),
        }],
        agent: None,
    });
    let reason = invalid_resume(run(&agent, ctx(session.clone(), true), input).await);
    assert_eq!(reason, InvalidResume::ToolItemsInInput);
    assert_eq!(log(&session).await.len(), len, "nothing is appended");

    // CallNotInHistory: the snapshot lost the deferred ToolCall.
    let compacted = Arc::new(NoToolCallSnapshot(session.clone()));
    let reason = invalid_resume(
        run(
            &agent,
            ctx(compacted, true),
            user("x").with_resolution(&d, ToolResolution::Approved),
        )
        .await,
    );
    assert_eq!(
        reason,
        InvalidResume::CallNotInHistory {
            call_id: "call_1".to_owned()
        }
    );
    assert_eq!(log(&session).await.len(), len, "nothing is appended");

    // SessionModified: a user message added by hand after the deferral.
    session
        .append(&[SessionEvent::user_message(vec![ContentPart::Text {
            text: "by hand".to_owned(),
        }])])
        .await
        .unwrap();
    let len = log(&session).await.len();
    let reason = invalid_resume(
        run(
            &agent,
            ctx(session.clone(), true),
            user("x").with_resolution(&d, ToolResolution::Approved),
        )
        .await,
    );
    assert_eq!(reason, InvalidResume::SessionModified);
    assert_eq!(log(&session).await.len(), len, "nothing is appended");

    // PathTooDeep: deferred with the default depth, resumed with depth 0.
    let handoff = handoff_session().await;
    let d = open_ids(&handoff).await.remove(0);
    let len = log(&handoff).await.len();
    let result = TokioRunner
        .run(
            &agent,
            ctx(handoff.clone(), true),
            user("x").with_resolution(&d, ToolResolution::Approved),
            RunConfig::new().with_max_agent_depth(0),
        )
        .await;
    assert_eq!(
        invalid_resume(result),
        InvalidResume::PathTooDeep {
            len: 2,
            max_agent_depth: 0
        }
    );
    assert_eq!(log(&handoff).await.len(), len, "nothing is appended");
}

#[tokio::test]
async fn renamed_root_denies_and_continues() {
    let session = deferred_session(&["call_1"]).await;
    let pay = PayTool::new();
    let model = RecordingModel::with_scripts(vec![text_turn("ok")]);
    let agent = pay_agent("other", Arc::clone(&model), &pay);
    let input = resolve_all(&session, ToolResolution::Approved).await;

    let result = run(&agent, ctx(session.clone(), true), input)
        .await
        .expect("resume succeeds");

    assert_eq!(result.final_output, "ok");
    assert!(pay.calls().is_empty(), "a denied call does not run");
    assert!(open_ids(&session).await.is_empty());
    assert!(
        labels(&session).await.contains(
            &"tool_returned(call_1, permission denied: agent no longer available: root)".to_owned()
        ),
        "{:?}",
        labels(&session).await
    );
}

#[tokio::test]
async fn input_guardrail_tripwire_closes_the_deferral_before_the_input() {
    let session = deferred_session(&["call_1"]).await;
    let pay = PayTool::new();
    let model = RecordingModel::with_scripts(vec![text_turn("never")]);
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(Arc::clone(&model))
        .shared_tool(Arc::clone(&pay) as Arc<dyn Tool<()>>)
        .input_guardrail(TripAlways)
        .build();
    let input = resolve_all(&session, ToolResolution::Approved).await;

    let result = run(&agent, ctx(session.clone(), true), input).await;

    assert!(matches!(result, Err(RunError::Agent(_))), "{result:?}");
    assert!(pay.calls().is_empty());
    assert!(model.requests().is_empty());
    let ls = labels(&session).await;
    assert_eq!(
        ls[3..],
        [
            format!("tool_returned(call_1, {NOT_RUN})"),
            "user_message(next)".to_owned()
        ],
        "{ls:?}"
    );
    assert!(open_ids(&session).await.is_empty());
}

#[tokio::test]
async fn cancelled_resume_closes_the_deferral() {
    let session = deferred_session(&["call_1"]).await;
    let gate = Arc::new(tokio::sync::Notify::new());
    let pay = PayTool::blocking(Arc::clone(&gate));
    let agent = pay_agent(
        "root",
        RecordingModel::with_scripts(vec![text_turn("never")]),
        &pay,
    );
    let input = resolve_all(&session, ToolResolution::Approved).await;
    let cancel = CancellationToken::new();
    let ctx = ctx(session.clone(), true).with_cancel(cancel.clone());

    // Cancel when the tool starts; a run that does not start the tool is
    // cancelled after a short time, so the test fails and does not hang.
    let (result, ()) = tokio::join!(run(&agent, ctx, input), async {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), gate.notified()).await;
        cancel.cancel();
    });

    assert!(matches!(result, Err(RunError::Cancelled)), "{result:?}");
    assert_eq!(pay.calls().len(), 1);
    let ls = labels(&session).await;
    assert_eq!(
        ls[3..],
        [
            format!("tool_returned(call_1, {CANCELLED})"),
            "user_message(next)".to_owned()
        ],
        "{ls:?}"
    );
    assert!(open_ids(&session).await.is_empty());
}

/// A workflow agent cannot resume deferred calls. Its rejection must leave
/// the session as it was, so a later resume through an `LlmAgent` still works.
#[tokio::test]
async fn workflow_agent_rejecting_a_resume_keeps_the_deferrals_open() {
    let session = deferred_session(&["call_1"]).await;
    let before = serde_json::to_value(log(&session).await).unwrap();
    let open_before = open_ids(&session).await;
    assert_eq!(open_before.len(), 1);

    let step = pay_agent(
        "step",
        RecordingModel::with_scripts(vec![text_turn("never")]),
        &PayTool::new(),
    );
    let workflow = SequentialAgent::<()>::new("seq", "one step").then(step);

    // `run` and `run_streamed` both reject the resume.
    let input = resolve_all(&session, ToolResolution::Approved).await;
    let result = run(&workflow, ctx(session.clone(), true), input).await;
    assert!(matches!(result, Err(RunError::Agent(_))), "{result:?}");
    assert_eq!(
        serde_json::to_value(log(&session).await).unwrap(),
        before,
        "nothing is appended"
    );
    assert_eq!(open_ids(&session).await, open_before);

    let input = resolve_all(&session, ToolResolution::Approved).await;
    let result = collect_streamed(&workflow, ctx(session.clone(), true), input).await;
    assert!(matches!(result, Err(RunError::Agent(_))), "{result:?}");
    assert_eq!(
        serde_json::to_value(log(&session).await).unwrap(),
        before,
        "nothing is appended"
    );
    assert_eq!(open_ids(&session).await, open_before);

    // A later valid resume through an `LlmAgent` runs the call.
    let pay = PayTool::new();
    let agent = pay_agent(
        "root",
        RecordingModel::with_scripts(vec![text_turn("done")]),
        &pay,
    );
    let input = resolve_all(&session, ToolResolution::Approved).await;
    let result = run(&agent, ctx(session.clone(), true), input)
        .await
        .expect("the resume succeeds");
    assert_eq!(result.final_output, "done");
    assert_eq!(pay.calls(), vec![recorded_args()]);
    assert!(open_ids(&session).await.is_empty());
}

// ── Lost writes ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn failed_append_on_deferring_run_returns_session_write() {
    let failing: Arc<dyn Session> = Arc::new(FailingAppendSession(Arc::new(MemorySession::new())));

    let model = RecordingModel::with_scripts(vec![pay_turn(&["call_1"])]);
    let agent = pay_agent("root", model, &PayTool::new());
    let result = run(&agent, ctx(failing.clone(), true), user("go")).await;
    assert!(
        matches!(
            result,
            Err(RunError::SessionWrite(SessionError::Unavailable))
        ),
        "{result:?}"
    );

    let model = RecordingModel::with_scripts(vec![pay_turn(&["call_1"])]);
    let agent = pay_agent("root", model, &PayTool::new());
    let events = run_streamed_events(&agent, ctx(failing.clone(), true), user("go")).await;
    match events.last() {
        Some(AgentEvent::RunFailed { error }) => {
            assert!(error.starts_with("session write failed:"), "{error}");
        }
        other => panic!("expected RunFailed, got {other:?}"),
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::RunDeferred { .. })),
        "{events:?}"
    );

    // A run that does not defer keeps the best-effort write.
    let model = RecordingModel::with_scripts(vec![text_turn("hi")]);
    let agent = pay_agent("root", model, &PayTool::new());
    let result = run(&agent, ctx(failing.clone(), true), user("go")).await;
    assert_eq!(result.expect("best-effort write").final_output, "hi");
    let model = RecordingModel::with_scripts(vec![text_turn("hi")]);
    let agent = pay_agent("root", model, &PayTool::new());
    let events = run_streamed_events(&agent, ctx(failing, true), user("go")).await;
    assert!(
        matches!(events.last(), Some(AgentEvent::RunCompleted { .. })),
        "{events:?}"
    );
}

#[tokio::test]
async fn failed_append_on_resume_returns_session_write() {
    let session = deferred_session(&["call_1"]).await;
    let failing: Arc<dyn Session> = Arc::new(FailingAppendSession(session.clone()));
    let pay = PayTool::new();

    let agent = pay_agent(
        "root",
        RecordingModel::with_scripts(vec![text_turn("done")]),
        &pay,
    );
    let input = resolve_all(&session, ToolResolution::Approved).await;
    let result = run(&agent, ctx(failing.clone(), true), input).await;
    assert!(
        matches!(
            result,
            Err(RunError::SessionWrite(SessionError::Unavailable))
        ),
        "{result:?}"
    );
    assert_eq!(pay.calls().len(), 1, "the approved tool ran");

    let agent = pay_agent(
        "root",
        RecordingModel::with_scripts(vec![text_turn("done")]),
        &pay,
    );
    let input = resolve_all(&session, ToolResolution::Approved).await;
    let events = run_streamed_events(&agent, ctx(failing, true), input).await;
    match events.last() {
        Some(AgentEvent::RunFailed { error }) => {
            assert!(error.starts_with("session write failed:"), "{error}");
        }
        other => panic!("expected RunFailed, got {other:?}"),
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::RunCompleted { .. })),
        "{events:?}"
    );
    assert_eq!(open_ids(&session).await.len(), 1, "the deferral stays open");
}

// ── Plan review focus ────────────────────────────────────────────────────────

#[tokio::test]
async fn resume_works_with_flag_off() {
    let session = deferred_session(&["call_1"]).await;
    let pay = PayTool::new();
    let agent = pay_agent(
        "root",
        RecordingModel::with_scripts(vec![text_turn("done")]),
        &pay,
    );
    let input = resolve_all(&session, ToolResolution::Approved).await;

    let result = run(&agent, ctx(session.clone(), false), input)
        .await
        .expect("resume succeeds with the flag off");

    assert_eq!(result.final_output, "done");
    assert_eq!(pay.calls(), [recorded_args()]);
    assert!(open_ids(&session).await.is_empty());
}

#[tokio::test]
async fn chained_deferral_resumes_twice() {
    let session = deferred_session(&["call_1"]).await;
    let d1 = open_ids(&session).await.remove(0);
    let pay = PayTool::new();
    let model = RecordingModel::with_scripts(vec![pay_turn(&["call_2"]), text_turn("done")]);
    let agent = pay_agent("root", Arc::clone(&model), &pay);

    let result = run(
        &agent,
        ctx(session.clone(), true),
        user("next").with_resolution(&d1, ToolResolution::Approved),
    )
    .await
    .expect("the first resume succeeds");
    let StopReason::Deferred { calls } = &result.stop_reason else {
        panic!("expected a second deferral, got {:?}", result.stop_reason);
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id(), "call_2");
    let open = open_ids(&session).await;
    assert_eq!(open.len(), 1);
    assert_ne!(open[0], d1);
    assert_eq!(open[0], calls[0].deferral_id());
    assert_eq!(pay.calls().len(), 1);

    let result = run(
        &agent,
        ctx(session.clone(), true),
        user("more").with_resolution(&open[0], ToolResolution::Approved),
    )
    .await
    .expect("the second resume succeeds");
    assert_eq!(result.stop_reason, StopReason::Completed);
    assert_eq!(result.final_output, "done");
    assert_eq!(pay.calls().len(), 2);
    assert!(open_ids(&session).await.is_empty());
    for r in model.requests() {
        assert_tool_calls_paired(&r.messages);
    }
}

#[tokio::test]
async fn mixed_resolutions_keep_call_order() {
    let session = deferred_session(&["call_1", "call_2"]).await;
    let open = open_ids(&session).await;
    assert_eq!(open.len(), 2);
    let pay = PayTool::new();
    let model = RecordingModel::with_scripts(vec![text_turn("done")]);
    let agent = pay_agent("root", Arc::clone(&model), &pay);

    let len = log(&session).await.len();
    let reason = invalid_resume(
        run(
            &agent,
            ctx(session.clone(), true),
            user("next").with_resolution(&open[0], ToolResolution::Approved),
        )
        .await,
    );
    assert_eq!(
        reason,
        InvalidResume::UnresolvedDeferrals {
            missing: vec![open[1].clone()]
        }
    );
    assert_eq!(log(&session).await.len(), len, "nothing is appended");

    // The resolutions come in the reverse order; the results keep the call order.
    let input = user("next")
        .with_resolution(
            &open[1],
            ToolResolution::Denied {
                reason: "no".to_owned(),
            },
        )
        .with_resolution(&open[0], ToolResolution::Approved);
    run(&agent, ctx(session.clone(), true), input)
        .await
        .expect("resume succeeds");

    assert_eq!(pay.calls().len(), 1);
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    let m = &requests[0].messages;
    assert_tool_calls_paired(m);
    let n = m.len();
    assert!(
        matches!(&m[n - 3], Item::ToolResult { call_id, .. } if call_id == "call_1"),
        "{m:?}"
    );
    assert!(
        matches!(&m[n - 2], Item::ToolResult { call_id, content }
            if call_id == "call_2" && text_of(content) == "permission denied: no"),
        "{m:?}"
    );
    assert!(
        matches!(&m[n - 1], Item::UserMessage { content } if text_of(content) == "next"),
        "{m:?}"
    );
    assert!(open_ids(&session).await.is_empty());
}

#[tokio::test]
async fn handoff_deferral_round_trip() {
    let session = handoff_session().await;
    let pay = PayTool::new();
    let child_model = RecordingModel::with_scripts(vec![text_turn("done")]);
    let budgeting = pay_agent("budgeting", Arc::clone(&child_model), &pay);
    let root_model = RecordingModel::with_scripts(vec![]);
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(Arc::clone(&root_model))
        .handoff(budgeting)
        .build();
    let input = resolve_all(&session, ToolResolution::Approved).await;

    let result = run(&triage, ctx(session.clone(), true), input)
        .await
        .expect("resume succeeds");

    assert_eq!(result.final_output, "done");
    assert_eq!(pay.calls(), [recorded_args()], "pay runs once on budgeting");
    assert!(
        root_model.requests().is_empty(),
        "triage's model must not run"
    );
    let requests = child_model.requests();
    assert_eq!(requests.len(), 1);
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
    assert_ends_with_result_then_next(&requests[0].messages, "call_1");
    assert!(open_ids(&session).await.is_empty());
}

/// A handoff run `triage -> budgeting`: triage transfers with
/// `transfer_id`, and budgeting calls `pay` with `pay_id`, then answers
/// "done". Returns the result, the session, the `pay` tool and the
/// budgeting model.
async fn handoff_run_with_ids(
    transfer_id: &str,
    pay_id: &str,
) -> (
    RunResult,
    Arc<MemorySession>,
    Arc<PayTool>,
    Arc<RecordingModel>,
) {
    let session = Arc::new(MemorySession::new());
    let pay = PayTool::new();
    let child_model = RecordingModel::with_scripts(vec![pay_turn(&[pay_id]), text_turn("done")]);
    let budgeting = pay_agent("budgeting", Arc::clone(&child_model), &pay);
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(RecordingModel::with_scripts(vec![transfer_turn(
            transfer_id,
            "budgeting",
        )]))
        .handoff(budgeting)
        .build();
    let result = run(&triage, ctx(session.clone(), true), user("hi"))
        .await
        .expect("the run succeeds");
    (result, session, pay, child_model)
}

#[tokio::test]
async fn deferral_with_the_call_id_of_an_open_transfer_call_is_denied() {
    // Gemini makes `fc_{n}` ids per stream, so the transfer call and the
    // child's first call can both be `fc_0`. Such a deferral cannot resume,
    // so the call is denied.
    let (result, session, pay, child_model) = handoff_run_with_ids("fc_0", "fc_0").await;

    assert!(
        matches!(result.stop_reason, StopReason::Completed),
        "{:?}",
        result.stop_reason
    );
    assert_eq!(result.final_output, "done");
    assert!(pay.calls().is_empty(), "a denied call does not run");
    assert!(open_ids(&session).await.is_empty(), "no open deferral");
    let labels = labels(&session).await;
    assert!(
        !labels.iter().any(|l| l.starts_with("tool_deferred")),
        "{labels:?}"
    );
    assert!(
        labels.contains(
            &"tool_returned(fc_0, permission denied: deferral not possible: call id fc_0 repeats in this run)"
                .to_owned()
        ),
        "{labels:?}"
    );
    let requests = child_model.requests();
    assert_eq!(requests.len(), 2);
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
}

#[tokio::test]
async fn deferral_with_a_unique_call_id_after_a_handoff_still_defers() {
    let (result, session, pay, child_model) = handoff_run_with_ids("t1", "fc_0").await;

    assert!(
        matches!(&result.stop_reason, StopReason::Deferred { calls } if calls.len() == 1),
        "{:?}",
        result.stop_reason
    );
    assert!(pay.calls().is_empty());
    assert_eq!(open_ids(&session).await.len(), 1);
    assert!(labels(&session)
        .await
        .contains(&"tool_deferred(fc_0)".to_owned()));
    assert_eq!(child_model.requests().len(), 1);
}

// ── Lost writes through run_streamed(..).collect() ──────────────────────────

#[tokio::test]
async fn failed_append_on_failing_resume_is_session_write_in_every_path() {
    // The agent fails (input guardrail tripwire) and fills its FailureSlot;
    // the lost write must still win.
    let session = deferred_session(&["call_1"]).await;
    let failing: Arc<dyn Session> = Arc::new(FailingAppendSession(session.clone()));
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(RecordingModel::with_scripts(vec![]))
        .shared_tool(PayTool::new() as Arc<dyn Tool<()>>)
        .input_guardrail(TripAlways)
        .build();

    let input = resolve_all(&session, ToolResolution::Approved).await;
    assert_session_write(&collect_streamed(&agent, ctx(failing.clone(), true), input).await);
    let input = resolve_all(&session, ToolResolution::Approved).await;
    assert_session_write(&run(&agent, ctx(failing, true), input).await);
    assert_eq!(open_ids(&session).await.len(), 1, "the deferral stays open");
}

#[tokio::test]
async fn failed_append_on_resume_collects_as_session_write() {
    let session = deferred_session(&["call_1"]).await;
    let failing: Arc<dyn Session> = Arc::new(FailingAppendSession(session.clone()));
    let pay = PayTool::new();
    let agent = pay_agent(
        "root",
        RecordingModel::with_scripts(vec![text_turn("done")]),
        &pay,
    );
    let input = resolve_all(&session, ToolResolution::Approved).await;

    assert_session_write(&collect_streamed(&agent, ctx(failing, true), input).await);
    assert_eq!(pay.calls().len(), 1, "the approved tool ran");

    // A deferring run collects as SessionWrite too.
    let failing: Arc<dyn Session> = Arc::new(FailingAppendSession(Arc::new(MemorySession::new())));
    let agent = pay_agent(
        "root",
        RecordingModel::with_scripts(vec![pay_turn(&["call_1"])]),
        &PayTool::new(),
    );
    assert_session_write(&collect_streamed(&agent, ctx(failing, true), user("go")).await);
}

/// Resume `session` through `failing` with a tool that blocks, cancel the run
/// when the tool starts, and return the result of `streamed` or `run`.
async fn cancelled_resume(
    session: &Arc<MemorySession>,
    failing: Arc<dyn Session>,
    streamed: bool,
) -> Result<RunResult, RunError> {
    let gate = Arc::new(tokio::sync::Notify::new());
    let pay = PayTool::blocking(Arc::clone(&gate));
    let agent = pay_agent(
        "root",
        RecordingModel::with_scripts(vec![text_turn("never")]),
        &pay,
    );
    let input = resolve_all(session, ToolResolution::Approved).await;
    let cancel = CancellationToken::new();
    let ctx = ctx(failing, true).with_cancel(cancel.clone());
    let cancel_on_start = async {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), gate.notified()).await;
        cancel.cancel();
    };
    let (result, ()) = if streamed {
        tokio::join!(collect_streamed(&agent, ctx, input), cancel_on_start)
    } else {
        tokio::join!(run(&agent, ctx, input), cancel_on_start)
    };
    assert_eq!(pay.calls().len(), 1, "the approved tool started");
    result
}

#[tokio::test]
async fn failed_append_on_cancelled_resume_is_session_write() {
    let session = deferred_session(&["call_1"]).await;
    let failing: Arc<dyn Session> = Arc::new(FailingAppendSession(session.clone()));

    assert_session_write(&cancelled_resume(&session, failing.clone(), true).await);
    assert_session_write(&cancelled_resume(&session, failing, false).await);
    assert_eq!(open_ids(&session).await.len(), 1, "the deferral stays open");
}

#[tokio::test]
async fn failed_append_on_plain_run_keeps_its_result() {
    let failing =
        || -> Arc<dyn Session> { Arc::new(FailingAppendSession(Arc::new(MemorySession::new()))) };

    let agent = pay_agent(
        "root",
        RecordingModel::with_scripts(vec![text_turn("hi")]),
        &PayTool::new(),
    );
    let result = collect_streamed(&agent, ctx(failing(), true), user("go")).await;
    assert_eq!(result.expect("best-effort write").final_output, "hi");

    // A plain cancelled run keeps its interrupt result.
    let agent = common::text_agent(Arc::new(common::PendingModel), Vec::new());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = || ctx(failing(), true).with_cancel(cancel.clone());
    let result = run(&agent, ctx(), user("go")).await;
    assert!(matches!(result, Err(RunError::Cancelled)), "{result:?}");
    let events = run_streamed_events(&agent, ctx(), user("go")).await;
    match events.last() {
        Some(AgentEvent::RunFailed { error }) => {
            assert!(!error.starts_with("session write failed"), "{error}");
        }
        other => panic!("expected the interrupt terminal, got {other:?}"),
    }
    let result = collect_streamed(&agent, ctx(), user("go")).await;
    assert!(
        !matches!(result, Err(RunError::SessionWrite(_))),
        "{result:?}"
    );
}
