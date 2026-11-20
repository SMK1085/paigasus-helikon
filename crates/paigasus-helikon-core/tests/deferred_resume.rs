//! SMA-803 — `LlmAgent` resumes the resolved deferred calls of a session.

#[path = "common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{MockModel, MockTool};
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use paigasus_helikon_core::{
    open_deferrals, Agent, AgentEvent, AgentInput, CancellationToken, ContentPart, DenyRule,
    FinishReason, Guardrail, GuardrailError, GuardrailInput, GuardrailKind, GuardrailVerdict, Hook,
    HookDecision, HookEvent, HookRegistry, Item, LlmAgent, MemorySession, Model, ModelCapabilities,
    ModelError, ModelEvent, ModelRequest, PermissionDecision, PermissionPolicy, ResumePlan,
    RunContext, Session, SessionEvent, SessionRecorder, Tool, ToolResolution, TracerHandle,
};

const SECRET: &str = "sk-test-SECRET-1234";
const NOT_RUN: &str = "tool call did not run: the run ended before the resume completed";

/// Defers every call to tool `b`; allows the rest.
struct DeferB;

#[async_trait]
impl PermissionPolicy<()> for DeferB {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        tool: &str,
        _args: &serde_json::Value,
    ) -> PermissionDecision {
        if tool == "b" {
            PermissionDecision::Defer {
                prompt: "pay?".to_owned(),
            }
        } else {
            PermissionDecision::Allow
        }
    }
}

/// A [`MockModel`] that keeps every request it gets.
struct RequestLog {
    inner: Arc<MockModel>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl RequestLog {
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
impl Model for RequestLog {
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

fn recorded_args() -> serde_json::Value {
    serde_json::json!({ "amount": 5 })
}

fn call_b_turn() -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            call_id: "call_b".to_owned(),
            name: Some("b".to_owned()),
            args_delta: recorded_args().to_string(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]
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

fn session_ctx(session: &Arc<MemorySession>) -> RunContext<()> {
    RunContext::new(
        Arc::new(()),
        Arc::clone(session) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
    .with_permission_policy(Arc::new(DeferB))
}

async fn run_events(
    agent: &impl Agent<()>,
    ctx: RunContext<()>,
    input: AgentInput,
) -> Vec<AgentEvent> {
    agent
        .run(ctx, input)
        .await
        .expect("run starts")
        .collect()
        .await
}

/// Run a root agent that defers its call to `b`, and store the run in a new
/// session. Returns the session.
async fn deferred_session(b: Arc<MockTool>) -> Arc<MemorySession> {
    let session = Arc::new(MemorySession::new());
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(MockModel::with_scripts(vec![call_b_turn()]))
        .shared_tool(b as Arc<dyn Tool<()>>)
        .build();
    let ctx = session_ctx(&session).with_deferred_tool_calls(true);
    let mut recorder = SessionRecorder::new("root").with_deferral_slot(ctx.deferral_handle());
    let input = AgentInput::from_user_text("go");
    recorder.record_input(&input.messages);
    let events = run_events(&agent, ctx, input).await;
    assert!(
        matches!(events.last(), Some(AgentEvent::RunDeferred { .. })),
        "{events:?}"
    );
    for ev in &events {
        recorder.observe(ev);
    }
    session.append(&recorder.drain()).await.unwrap();
    session
}

/// The resume input: the resolution of the one open deferral, then `"next"`.
async fn resume_input(
    session: &MemorySession,
    root: &str,
    resolution: ToolResolution,
) -> (AgentInput, ResumePlan) {
    let events = session.events(None).await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let open = open_deferrals(&events);
    assert_eq!(open.len(), 1, "{open:?}");
    let input =
        AgentInput::from_user_text("next").with_resolution(open[0].deferral_id(), resolution);
    let plan = ResumePlan::from_events(&events, &snapshot, root, &input, 8).expect("valid resume");
    (input.with_resume_plan(plan.clone()), plan)
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

/// The text of the `ToolOutputItem` for `call_id`.
fn tool_output_text(events: &[AgentEvent], call_id: &str) -> String {
    events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolOutputItem {
                item:
                    Item::ToolResult {
                        call_id: id,
                        content,
                    },
            } if id == call_id => Some(text_of(content)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no ToolOutputItem for {call_id}: {events:?}"))
}

/// A short label for each event.
fn kind(ev: &AgentEvent) -> String {
    match ev {
        AgentEvent::RunStarted { .. } => "RunStarted".to_owned(),
        AgentEvent::TurnStarted { turn } => format!("TurnStarted({turn})"),
        AgentEvent::ToolOutputItem {
            item: Item::ToolResult { call_id, .. },
        } => format!("ToolOutputItem({call_id})"),
        AgentEvent::PermissionDenied { tool, .. } => format!("PermissionDenied({tool})"),
        AgentEvent::GuardrailTriggered { .. } => "GuardrailTriggered".to_owned(),
        AgentEvent::RunCompleted { .. } => "RunCompleted".to_owned(),
        AgentEvent::RunFailed { error } => format!("RunFailed({error})"),
        _ => "other".to_owned(),
    }
}

fn kinds(events: &[AgentEvent]) -> Vec<String> {
    events.iter().map(kind).collect()
}

fn resume_agent(
    name: &str,
    model: Arc<RequestLog>,
    b: Arc<MockTool>,
) -> paigasus_helikon_core::LlmAgentBuilder<
    (),
    RequestLog,
    String,
    paigasus_helikon_core::HasName,
    paigasus_helikon_core::HasModel,
> {
    LlmAgent::builder::<()>()
        .name(name)
        .shared_model(model)
        .shared_tool(b as Arc<dyn Tool<()>>)
}

#[tokio::test]
async fn approved_call_runs_once_with_recorded_args_before_the_new_input() {
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let session = deferred_session(Arc::clone(&b)).await;
    assert!(
        b.invocations().is_empty(),
        "the deferring run must not run b"
    );
    let (input, plan) = resume_input(&session, "root", ToolResolution::Approved).await;

    let model = RequestLog::with_scripts(vec![text_turn("done")]);
    let agent = resume_agent("root", Arc::clone(&model), Arc::clone(&b)).build();
    let ctx = session_ctx(&session);
    let mut recorder = SessionRecorder::new("root").with_resume_plan(&plan);
    recorder.record_input(&input.messages);
    let events = run_events(&agent, ctx, input).await;

    let invocations = b.invocations();
    assert_eq!(invocations.len(), 1, "b runs exactly once");
    assert_eq!(invocations[0].0, recorded_args());

    let ks = kinds(&events);
    assert_eq!(
        ks[..3],
        ["RunStarted", "ToolOutputItem(call_b)", "TurnStarted(0)"],
        "{ks:?}"
    );
    assert_eq!(
        ks.last().map(String::as_str),
        Some("RunCompleted"),
        "{ks:?}"
    );
    assert_eq!(tool_output_text(&events, "call_b"), r#"{"paid":true}"#);

    let requests = model.requests();
    let messages = &requests[0].messages;
    let n = messages.len();
    assert!(n >= 3, "{messages:?}");
    assert!(
        matches!(&messages[n - 3], Item::ToolCall { call_id, name, .. }
            if call_id == "call_b" && name == "b"),
        "{messages:?}"
    );
    assert!(
        matches!(&messages[n - 2], Item::ToolResult { call_id, .. } if call_id == "call_b"),
        "{messages:?}"
    );
    assert!(
        matches!(&messages[n - 1], Item::UserMessage { content } if text_of(content) == "next"),
        "{messages:?}"
    );

    // The recorder puts the result before the new input, and the resume
    // closes the deferral.
    for ev in &events {
        recorder.observe(ev);
    }
    let resumed = recorder.drain();
    assert!(
        matches!(&resumed[0], SessionEvent::ToolReturned { call_id, .. } if call_id == "call_b"),
        "{resumed:?}"
    );
    assert!(
        matches!(&resumed[1], SessionEvent::UserMessage { .. }),
        "{resumed:?}"
    );
    session.append(&resumed).await.unwrap();
    assert!(open_deferrals(&session.events(None).await.unwrap()).is_empty());
}

#[tokio::test]
async fn denied_call_gets_the_denial_result_and_the_tool_does_not_run() {
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let session = deferred_session(Arc::clone(&b)).await;
    let (input, _) = resume_input(
        &session,
        "root",
        ToolResolution::Denied {
            reason: "nope".to_owned(),
        },
    )
    .await;

    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let agent = resume_agent("root", model, Arc::clone(&b)).build();
    let events = run_events(&agent, session_ctx(&session), input).await;

    assert!(b.invocations().is_empty(), "a denied call does not run");
    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: nope"
    );
    assert!(
        events.iter().any(|e| matches!(e,
            AgentEvent::PermissionDenied { tool, reason } if tool == "b" && reason == "nope")),
        "{events:?}"
    );
    let ks = kinds(&events);
    assert_eq!(
        ks[..4],
        [
            "RunStarted",
            "PermissionDenied(b)",
            "ToolOutputItem(call_b)",
            "TurnStarted(0)"
        ],
        "{ks:?}"
    );
    assert_eq!(
        ks.last().map(String::as_str),
        Some("RunCompleted"),
        "{ks:?}"
    );
}

/// Replaces the output of every `b` call with a value that holds a secret.
struct LeakSecret;

#[async_trait]
impl Hook<()> for LeakSecret {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        match event {
            HookEvent::PostToolUse { tool, .. } if tool == "b" => HookDecision::ReplaceOutput {
                value: serde_json::json!({ "token": SECRET }),
            },
            _ => HookDecision::Allow,
        }
    }
}

#[tokio::test]
async fn approved_call_with_post_tool_use_hook_and_redaction() {
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let session = deferred_session(Arc::clone(&b)).await;
    let (input, _) = resume_input(&session, "root", ToolResolution::Approved).await;

    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let agent = resume_agent("root", model, Arc::clone(&b))
        .hook(LeakSecret)
        .build();
    let ctx = session_ctx(&session).with_extra_secrets(vec![SECRET.to_owned()]);
    let events = run_events(&agent, ctx, input).await;

    assert_eq!(b.invocations().len(), 1);
    let text = tool_output_text(&events, "call_b");
    assert!(text.contains("***"), "{text}");
    assert!(!text.contains(SECRET), "{text}");
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

#[tokio::test]
async fn input_guardrail_tripwire_closes_the_deferral() {
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let session = deferred_session(Arc::clone(&b)).await;
    let (input, _) = resume_input(&session, "root", ToolResolution::Approved).await;

    let model = RequestLog::with_scripts(vec![text_turn("never")]);
    let agent = resume_agent("root", Arc::clone(&model), Arc::clone(&b))
        .input_guardrail(TripAlways)
        .build();
    let events = run_events(&agent, session_ctx(&session), input).await;

    assert!(b.invocations().is_empty(), "the tool must not run");
    assert!(model.requests().is_empty(), "the model must not run");
    assert_eq!(
        kinds(&events),
        [
            "RunStarted",
            "ToolOutputItem(call_b)",
            "GuardrailTriggered",
            "RunFailed(input guardrail tripwire)"
        ]
    );
    assert_eq!(tool_output_text(&events, "call_b"), NOT_RUN);
}

#[tokio::test]
async fn renamed_root_denies_and_continues() {
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let session = deferred_session(Arc::clone(&b)).await;
    let (input, _) = resume_input(&session, "other", ToolResolution::Approved).await;

    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let agent = resume_agent("other", Arc::clone(&model), Arc::clone(&b)).build();
    let events = run_events(&agent, session_ctx(&session), input).await;

    assert!(b.invocations().is_empty(), "a denied call does not run");
    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: agent no longer available: root"
    );
    assert_eq!(model.requests().len(), 1);
    let ks = kinds(&events);
    assert_eq!(
        ks.last().map(String::as_str),
        Some("RunCompleted"),
        "{ks:?}"
    );
}

#[tokio::test]
async fn resolutions_without_a_plan_fail() {
    let model = RequestLog::with_scripts(vec![text_turn("never")]);
    let agent = resume_agent(
        "root",
        Arc::clone(&model),
        MockTool::new("b", serde_json::json!({})),
    )
    .build();
    let input = AgentInput::new().with_resolution("x", ToolResolution::Approved);
    let events = run_events(&agent, RunContext::ephemeral(()), input).await;

    assert_eq!(
        kinds(&events),
        [
            "RunStarted",
            "RunFailed(resolutions require a deferral-aware runner)"
        ]
    );
    assert!(model.requests().is_empty());
}

#[tokio::test]
async fn approved_call_to_a_removed_tool_is_denied() {
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let session = deferred_session(Arc::clone(&b)).await;
    let (input, _) = resume_input(&session, "root", ToolResolution::Approved).await;

    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(model)
        .build();
    let events = run_events(&agent, session_ctx(&session), input).await;

    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: tool no longer available: b"
    );
    assert!(
        events.iter().any(|e| matches!(e,
            AgentEvent::PermissionDenied { tool, reason }
                if tool == "b" && reason == "tool no longer available: b")),
        "{events:?}"
    );
    assert!(b.invocations().is_empty());
}

/// Replaces the args of every call with `{"amount": 500}`.
struct RaiseAmount;

#[async_trait]
impl PermissionPolicy<()> for RaiseAmount {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        _tool: &str,
        _args: &serde_json::Value,
    ) -> PermissionDecision {
        PermissionDecision::Replace {
            args: serde_json::json!({ "amount": 500 }),
        }
    }
}

/// Resume an approved `b` call with the context `ctx_of(session)`.
async fn resume_approved_with(
    b: &Arc<MockTool>,
    ctx_of: impl FnOnce(&Arc<MemorySession>) -> RunContext<()>,
) -> Vec<AgentEvent> {
    let session = deferred_session(Arc::clone(b)).await;
    let (input, _) = resume_input(&session, "root", ToolResolution::Approved).await;
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let agent = resume_agent("root", model, Arc::clone(b)).build();
    run_events(&agent, ctx_of(&session), input).await
}

#[tokio::test]
async fn approved_call_that_a_deny_rule_now_denies_does_not_run() {
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let events = resume_approved_with(&b, |s| {
        session_ctx(s).with_deny_rules(vec![DenyRule::tool("b")])
    })
    .await;

    assert!(
        b.invocations().is_empty(),
        "the re-check must deny the call"
    );
    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: denied by deny rule: b"
    );
    assert!(
        events.iter().any(|e| matches!(e,
            AgentEvent::PermissionDenied { tool, reason }
                if tool == "b" && reason == "denied by deny rule: b")),
        "{events:?}"
    );
}

#[tokio::test]
async fn approved_call_whose_policy_now_changes_the_args_does_not_run() {
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let events = resume_approved_with(&b, |s| {
        session_ctx(s).with_permission_policy(Arc::new(RaiseAmount))
    })
    .await;

    assert!(
        b.invocations().is_empty(),
        "the re-check must deny the call"
    );
    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: arguments changed on re-check"
    );
}

// ── Handoff paths (§4.7) ─────────────────────────────────────────────────────

const TRANSFER_NOTE: &str =
    "You are now handling a transferred conversation. Continue assisting the user.";

/// Each `ToolCall` has a `ToolResult` with the same call id later, with no
/// `UserMessage` or `AssistantMessage` between them. Each `ToolResult` closes
/// one earlier open `ToolCall` with its call id. A call id can repeat only
/// after its earlier call has a result.
fn assert_tool_calls_paired(messages: &[Item]) {
    // The calls that wait for a result, in call order.
    let mut outstanding: Vec<&str> = Vec::new();
    for item in messages {
        match item {
            Item::ToolCall { call_id, .. } => {
                assert!(
                    !outstanding.contains(&call_id.as_str()),
                    "call {call_id} repeats while it waits for a result: {messages:?}"
                );
                outstanding.push(call_id);
            }
            Item::ToolResult { call_id, .. } => {
                let Some(p) = outstanding.iter().position(|c| *c == call_id.as_str()) else {
                    panic!("result {call_id} has no open earlier call: {messages:?}");
                };
                // Each result closes one call.
                outstanding.remove(p);
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

/// The call id of the transfer call from `path[i]` to `path[i + 1]`.
fn transfer_id(i: usize) -> String {
    format!("t{}", i + 1)
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

/// Run a handoff chain along `path` whose last agent defers its call to `b`,
/// and store the run in a new session. Returns the session.
async fn handoff_session(path: &[&str]) -> Arc<MemorySession> {
    handoff_session_with(path, call_b_turn()).await
}

/// The leaf turn of [`text_then_call_b_session`]: text, then the call to `b`.
fn text_then_call_b_turn() -> Vec<ModelEvent> {
    let mut turn = vec![ModelEvent::TokenDelta {
        text: "checking".to_owned(),
    }];
    turn.extend(call_b_turn());
    turn
}

/// As [`handoff_session`], but the last agent writes text before its call,
/// so a child `AssistantMessage` sits between each transfer call and the
/// recorder's result for it.
async fn text_then_call_b_session(path: &[&str]) -> Arc<MemorySession> {
    handoff_session_with(path, text_then_call_b_turn()).await
}

async fn handoff_session_with(path: &[&str], leaf_turn: Vec<ModelEvent>) -> Arc<MemorySession> {
    let transfer_turns = (0..path.len() - 1)
        .map(|i| transfer_turn(&transfer_id(i), path[i + 1]))
        .collect();
    scripted_handoff_session(path, transfer_turns, leaf_turn).await
}

/// As [`handoff_session_with`], but `transfer_turns[i]` is the handoff
/// response of `path[i]`.
async fn scripted_handoff_session(
    path: &[&str],
    mut transfer_turns: Vec<Vec<ModelEvent>>,
    leaf_turn: Vec<ModelEvent>,
) -> Arc<MemorySession> {
    let leaf = path.len() - 1;
    assert_eq!(transfer_turns.len(), leaf);
    let mut agent: Arc<dyn Agent<()>> = Arc::new(
        LlmAgent::builder::<()>()
            .name(path[leaf])
            .shared_model(MockModel::with_scripts(vec![leaf_turn]))
            .shared_tool(MockTool::new("b", serde_json::json!({})) as Arc<dyn Tool<()>>)
            .build(),
    );
    for i in (0..leaf).rev() {
        agent = Arc::new(
            LlmAgent::builder::<()>()
                .name(path[i])
                .shared_model(MockModel::with_scripts(vec![transfer_turns.remove(i)]))
                .shared_handoff(agent)
                .build(),
        );
    }
    let session = Arc::new(MemorySession::new());
    let ctx = session_ctx(&session).with_deferred_tool_calls(true);
    let mut recorder = SessionRecorder::new(path[0]).with_deferral_slot(ctx.deferral_handle());
    let input = AgentInput::from_user_text("hi");
    recorder.record_input(&input.messages);
    let events = agent
        .run(ctx, input)
        .await
        .expect("run starts")
        .collect::<Vec<_>>()
        .await;
    assert!(
        matches!(events.last(), Some(AgentEvent::RunDeferred { .. })),
        "{events:?}"
    );
    for ev in &events {
        recorder.observe(ev);
    }
    session.append(&recorder.drain()).await.unwrap();
    session
}

/// As [`handoff_session`], but the log has no result for the transfer calls
/// (`t1`, `t2`, ...). The session recorder closes them at the end of the
/// run; a log from another writer can leave them open.
async fn open_transfer_session(path: &[&str]) -> Arc<MemorySession> {
    without_transfer_results(&handoff_session(path).await).await
}

/// A copy of `session` without the results of the transfer calls (each call
/// id that starts with `t`).
async fn without_transfer_results(session: &Arc<MemorySession>) -> Arc<MemorySession> {
    let recorded = session.events(None).await.unwrap();
    let events: Vec<SessionEvent> = recorded
        .into_iter()
        .filter(|e| {
            !matches!(e, SessionEvent::ToolReturned { call_id, .. } if call_id.starts_with('t'))
        })
        .collect();
    let session = Arc::new(MemorySession::new());
    session.append(&events).await.unwrap();
    session
}

/// Counts `OnHandoff` and records each `OnSubagentStop` agent.
#[derive(Default)]
struct HandoffCounts {
    on_handoff: Mutex<usize>,
    subagent_stops: Mutex<Vec<String>>,
}

struct CountHandoffs(Arc<HandoffCounts>);

#[async_trait]
impl Hook<()> for CountHandoffs {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        match event {
            HookEvent::OnHandoff { .. } => *self.0.on_handoff.lock().unwrap() += 1,
            HookEvent::OnSubagentStop { agent } => {
                self.0.subagent_stops.lock().unwrap().push(agent.clone());
            }
            _ => {}
        }
        HookDecision::Allow
    }
}

/// The index of the `ToolOutputItem` for `call_id`.
fn output_index(events: &[AgentEvent], call_id: &str) -> usize {
    events
        .iter()
        .position(|e| {
            matches!(e, AgentEvent::ToolOutputItem { item: Item::ToolResult { call_id: id, .. } }
                if id == call_id)
        })
        .unwrap_or_else(|| panic!("no ToolOutputItem for {call_id}: {events:?}"))
}

/// The transfer call `call_id` in `messages` is directly followed by the
/// result `agent no longer available: {name}`.
fn assert_transfer_closed(messages: &[Item], call_id: &str, name: &str) {
    let at = messages
        .iter()
        .position(|i| matches!(i, Item::ToolCall { call_id: id, .. } if id == call_id))
        .unwrap_or_else(|| panic!("no ToolCall {call_id}: {messages:?}"));
    match messages.get(at + 1) {
        Some(Item::ToolResult {
            call_id: id,
            content,
        }) if id == call_id => assert_eq!(
            text_of(content),
            format!("agent no longer available: {name}")
        ),
        other => panic!("{call_id} is not closed directly after its call: {other:?}"),
    }
}

/// The result for `call_id` in `messages` is at the call index + 1.
fn assert_result_adjacent(messages: &[Item], call_id: &str) {
    let call = messages
        .iter()
        .position(|i| matches!(i, Item::ToolCall { call_id: id, .. } if id == call_id))
        .unwrap_or_else(|| panic!("no ToolCall {call_id}: {messages:?}"));
    let result = messages
        .iter()
        .position(|i| matches!(i, Item::ToolResult { call_id: id, .. } if id == call_id))
        .unwrap_or_else(|| panic!("no ToolResult {call_id}: {messages:?}"));
    assert_eq!(result, call + 1, "{call_id}: {messages:?}");
}

/// The number of stored `ToolReturned` events for `call_id`.
async fn stored_results(session: &MemorySession, call_id: &str) -> usize {
    session
        .events(None)
        .await
        .unwrap()
        .iter()
        .filter(|e| matches!(e, SessionEvent::ToolReturned { call_id: id, .. } if id == call_id))
        .count()
}

/// No `ToolOutputItem` in `events` is for `call_id`.
fn assert_no_output(events: &[AgentEvent], call_id: &str) {
    assert!(
        !events.iter().any(|e| matches!(e,
            AgentEvent::ToolOutputItem { item: Item::ToolResult { call_id: id, .. } }
                if id == call_id)),
        "{events:?}"
    );
}

/// Resume `session` with an approved `b` and the root `root`, with `agent`.
/// Returns the events and checks that the recorder closes the deferral.
async fn resume_handoff(
    session: &Arc<MemorySession>,
    root: &str,
    agent: &impl Agent<()>,
) -> Vec<AgentEvent> {
    resume_handoff_with(session, root, agent, session_ctx(session)).await
}

/// As [`resume_handoff`], with the run context `ctx`.
async fn resume_handoff_with(
    session: &Arc<MemorySession>,
    root: &str,
    agent: &impl Agent<()>,
    ctx: RunContext<()>,
) -> Vec<AgentEvent> {
    let (input, plan) = resume_input(session, root, ToolResolution::Approved).await;
    let mut recorder = SessionRecorder::new(root).with_resume_plan(&plan);
    recorder.record_input(&input.messages);
    let events = run_events(agent, ctx, input).await;
    for ev in &events {
        recorder.observe(ev);
    }
    let resumed = recorder.drain();
    session.append(&resumed).await.unwrap();
    assert!(
        open_deferrals(&session.events(None).await.unwrap()).is_empty(),
        "{resumed:?}"
    );
    events
}

#[tokio::test]
async fn handoff_resume_runs_the_call_on_the_child_with_the_child_conversation() {
    let session = handoff_session(&["triage", "budgeting"]).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let child_model = RequestLog::with_scripts(vec![text_turn("done")]);
    let budgeting = resume_agent("budgeting", Arc::clone(&child_model), Arc::clone(&b))
        .instructions("you budget")
        .build();
    let root_model = RequestLog::with_scripts(vec![]);
    let counts = Arc::new(HandoffCounts::default());
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(Arc::clone(&root_model))
        .handoff(budgeting)
        .hook(CountHandoffs(Arc::clone(&counts)))
        .build();

    let events = resume_handoff(&session, "triage", &triage).await;

    assert!(
        root_model.requests().is_empty(),
        "triage's model must not run"
    );
    assert_eq!(b.invocations().len(), 1, "b runs once on the child");
    assert_eq!(b.invocations()[0].0, recorded_args());
    assert_eq!(tool_output_text(&events, "call_b"), r#"{"paid":true}"#);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::AgentUpdated { agent } if agent == "budgeting")),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::HandoffItem { .. })),
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::RunStarted { .. }))
            .count(),
        1,
        "the child RunStarted is dropped: {events:?}"
    );
    assert_eq!(*counts.on_handoff.lock().unwrap(), 0);
    assert_eq!(*counts.subagent_stops.lock().unwrap(), ["budgeting"]);
    assert_eq!(
        kinds(&events).last().map(String::as_str),
        Some("RunCompleted")
    );

    let requests = child_model.requests();
    assert_eq!(requests.len(), 1);
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
    let m = &requests[0].messages;
    assert!(
        matches!(&m[0], Item::System { content } if text_of(content) == "you budget"),
        "{m:?}"
    );
    assert!(
        matches!(&m[1], Item::UserMessage { content } if text_of(content) == "hi"),
        "{m:?}"
    );
    assert!(
        matches!(&m[2], Item::UserMessage { content } if text_of(content) == TRANSFER_NOTE),
        "{m:?}"
    );
    let n = m.len();
    assert!(
        matches!(&m[n - 3], Item::ToolCall { call_id, .. } if call_id == "call_b"),
        "{m:?}"
    );
    assert!(
        matches!(&m[n - 2], Item::ToolResult { call_id, .. } if call_id == "call_b"),
        "{m:?}"
    );
    assert!(
        matches!(&m[n - 1], Item::UserMessage { content } if text_of(content) == "next"),
        "{m:?}"
    );
    assert!(
        !m.iter()
            .any(|i| matches!(i, Item::ToolCall { call_id, .. } if call_id == "t1")),
        "{m:?}"
    );
}

#[tokio::test]
async fn missing_handoff_target_denies_the_calls_at_the_deepest_reachable_agent() {
    let session = open_transfer_session(&["triage", "budgeting"]).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let triage = resume_agent("triage", Arc::clone(&model), Arc::clone(&b)).build();

    let events = resume_handoff(&session, "triage", &triage).await;

    assert!(b.invocations().is_empty(), "a denied call does not run");
    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: agent no longer available: budgeting"
    );
    assert_eq!(
        tool_output_text(&events, "t1"),
        "agent no longer available: budgeting"
    );
    assert!(output_index(&events, "t1") < output_index(&events, "call_b"));
    let requests = model.requests();
    assert_eq!(requests.len(), 1, "triage's model runs next");
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
    assert_transfer_closed(&requests[0].messages, "t1", "budgeting");
}

#[tokio::test]
async fn missing_handoff_target_adds_no_second_result_to_a_closed_transfer_call() {
    // The session recorder already closed t1 at the end of the deferring run.
    let session = handoff_session(&["triage", "budgeting"]).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let triage = resume_agent("triage", Arc::clone(&model), Arc::clone(&b)).build();

    let events = resume_handoff(&session, "triage", &triage).await;

    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: agent no longer available: budgeting"
    );
    assert!(
        !events.iter().any(|e| matches!(e,
            AgentEvent::ToolOutputItem { item: Item::ToolResult { call_id, .. } } if call_id == "t1")),
        "{events:?}"
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
}

#[tokio::test]
async fn renamed_root_on_a_handoff_path_closes_every_transfer_call() {
    let session = open_transfer_session(&["triage", "budgeting", "savings"]).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let other = resume_agent("other", Arc::clone(&model), Arc::clone(&b)).build();

    let events = resume_handoff(&session, "other", &other).await;

    assert!(b.invocations().is_empty(), "a denied call does not run");
    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: agent no longer available: triage"
    );
    assert!(output_index(&events, "t1") < output_index(&events, "call_b"));
    assert!(output_index(&events, "t2") < output_index(&events, "call_b"));
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
    assert_transfer_closed(&requests[0].messages, "t1", "budgeting");
    assert_transfer_closed(&requests[0].messages, "t2", "savings");
}

#[tokio::test]
async fn missing_middle_agent_on_a_path_of_three_closes_every_transfer_call() {
    let session = open_transfer_session(&["triage", "budgeting", "savings"]).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let savings = resume_agent("savings", RequestLog::with_scripts(vec![]), Arc::clone(&b)).build();
    // triage hands off to savings now, not to budgeting.
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(Arc::clone(&model))
        .handoff(savings)
        .build();

    let events = resume_handoff(&session, "triage", &triage).await;

    assert!(b.invocations().is_empty(), "a denied call does not run");
    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: agent no longer available: budgeting"
    );
    assert!(output_index(&events, "t1") < output_index(&events, "call_b"));
    assert!(output_index(&events, "t2") < output_index(&events, "call_b"));
    let requests = model.requests();
    assert_eq!(requests.len(), 1, "triage's model runs next");
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
    assert_transfer_closed(&requests[0].messages, "t1", "budgeting");
    assert_transfer_closed(&requests[0].messages, "t2", "savings");
}

#[tokio::test]
async fn missing_deeper_target_denies_at_the_middle_agent() {
    let session = open_transfer_session(&["triage", "budgeting", "savings"]).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let middle_model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let budgeting = resume_agent("budgeting", Arc::clone(&middle_model), Arc::clone(&b)).build();
    let root_model = RequestLog::with_scripts(vec![]);
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(Arc::clone(&root_model))
        .handoff(budgeting)
        .build();

    let events = resume_handoff(&session, "triage", &triage).await;

    assert!(
        root_model.requests().is_empty(),
        "triage's model must not run"
    );
    assert!(b.invocations().is_empty(), "a denied call does not run");
    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: agent no longer available: savings"
    );
    assert!(output_index(&events, "t2") < output_index(&events, "call_b"));
    let requests = middle_model.requests();
    assert_eq!(requests.len(), 1, "budgeting's model runs next");
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
    let m = &requests[0].messages;
    assert_transfer_closed(m, "t2", "savings");
    assert!(
        !m.iter()
            .any(|i| matches!(i, Item::ToolCall { call_id, .. } if call_id == "t1")),
        "{m:?}"
    );
}

/// Check a denial resume on a live log where the child wrote text: the
/// recorder's results for `t1` and `t2` sit directly after their calls in
/// every request, and the session gets no second result for them.
async fn assert_recorded_transfers_moved(
    session: &MemorySession,
    events: &[AgentEvent],
    requests: &[ModelRequest],
) {
    assert_eq!(requests.len(), 1);
    for r in requests {
        assert_tool_calls_paired(&r.messages);
        assert_result_adjacent(&r.messages, "t1");
        assert_result_adjacent(&r.messages, "t2");
    }
    let m = &requests[0].messages;
    assert!(
        m.iter()
            .any(|i| matches!(i, Item::AssistantMessage { content, .. }
            if text_of(content) == "checking")),
        "the child text must be in the request: {m:?}"
    );
    for id in ["t1", "t2"] {
        assert_no_output(events, id);
        assert_eq!(stored_results(session, id).await, 1, "{id}");
    }
}

#[tokio::test]
async fn renamed_root_on_a_recorded_log_moves_the_transfer_results_next_to_their_calls() {
    let session = text_then_call_b_session(&["triage", "budgeting", "savings"]).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let other = resume_agent("other", Arc::clone(&model), Arc::clone(&b)).build();

    let events = resume_handoff(&session, "other", &other).await;

    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: agent no longer available: triage"
    );
    assert_recorded_transfers_moved(&session, &events, &model.requests()).await;
}

#[tokio::test]
async fn missing_middle_agent_on_a_recorded_log_moves_the_transfer_results_next_to_their_calls() {
    let session = text_then_call_b_session(&["triage", "budgeting", "savings"]).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let triage = resume_agent("triage", Arc::clone(&model), Arc::clone(&b)).build();

    let events = resume_handoff(&session, "triage", &triage).await;

    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: agent no longer available: budgeting"
    );
    assert_recorded_transfers_moved(&session, &events, &model.requests()).await;
}

/// One handoff response with a transfer call for each `(call_id, target)`.
fn transfer_turn_with(calls: &[(&str, &str)]) -> Vec<ModelEvent> {
    let mut turn: Vec<ModelEvent> = calls
        .iter()
        .map(|(call_id, target)| ModelEvent::ToolCallDelta {
            call_id: (*call_id).to_owned(),
            name: Some(format!("transfer_to_{target}")),
            args_delta: "{}".to_owned(),
        })
        .collect();
    turn.push(ModelEvent::Finish {
        reason: FinishReason::ToolCalls,
    });
    turn
}

/// `messages` has `count` calls `call_id`, and a result `call_id` follows
/// each call directly. Returns the result texts in call order.
fn results_after_each_call(messages: &[Item], call_id: &str, count: usize) -> Vec<String> {
    let mut texts = Vec::new();
    for (at, item) in messages.iter().enumerate() {
        if !matches!(item, Item::ToolCall { call_id: id, .. } if id == call_id) {
            continue;
        }
        match messages.get(at + 1) {
            Some(Item::ToolResult {
                call_id: id,
                content,
            }) if id == call_id => texts.push(text_of(content)),
            other => panic!("{call_id} at {at} is not closed directly after: {other:?}"),
        }
    }
    assert_eq!(texts.len(), count, "{messages:?}");
    texts
}

/// The number of `ToolOutputItem` events for `call_id`.
fn output_count(events: &[AgentEvent], call_id: &str) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(e, AgentEvent::ToolOutputItem { item: Item::ToolResult { call_id: id, .. } }
                if id == call_id)
        })
        .count()
}

/// triage's handoff response holds two transfer calls to budgeting.
async fn two_transfer_calls_session(leaf_turn: Vec<ModelEvent>) -> Arc<MemorySession> {
    scripted_handoff_session(
        &["triage", "budgeting"],
        vec![transfer_turn_with(&[
            ("t1", "budgeting"),
            ("t1b", "budgeting"),
        ])],
        leaf_turn,
    )
    .await
}

#[tokio::test]
async fn missing_target_closes_every_open_transfer_call_of_one_handoff_response() {
    let session = without_transfer_results(&two_transfer_calls_session(call_b_turn()).await).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let triage = resume_agent("triage", Arc::clone(&model), Arc::clone(&b)).build();

    let events = resume_handoff(&session, "triage", &triage).await;

    assert!(b.invocations().is_empty(), "a denied call does not run");
    for id in ["t1", "t1b"] {
        assert_eq!(
            tool_output_text(&events, id),
            "agent no longer available: budgeting"
        );
        assert!(output_index(&events, id) < output_index(&events, "call_b"));
        assert_eq!(stored_results(&session, id).await, 1, "{id}");
    }
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
    assert_transfer_closed(&requests[0].messages, "t1", "budgeting");
    assert_transfer_closed(&requests[0].messages, "t1b", "budgeting");
}

#[tokio::test]
async fn missing_target_moves_every_recorded_transfer_result_of_one_handoff_response() {
    let session = two_transfer_calls_session(text_then_call_b_turn()).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let triage = resume_agent("triage", Arc::clone(&model), Arc::clone(&b)).build();

    let events = resume_handoff(&session, "triage", &triage).await;

    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
        assert_result_adjacent(&r.messages, "t1");
        assert_result_adjacent(&r.messages, "t1b");
    }
    for id in ["t1", "t1b"] {
        assert_no_output(&events, id);
        assert_eq!(stored_results(&session, id).await, 1, "{id}");
    }
}

/// A path of three whose two transfer calls share the call id `t`.
async fn shared_transfer_id_session(leaf_turn: Vec<ModelEvent>) -> Arc<MemorySession> {
    scripted_handoff_session(
        &["triage", "budgeting", "savings"],
        vec![
            transfer_turn("t", "budgeting"),
            transfer_turn("t", "savings"),
        ],
        leaf_turn,
    )
    .await
}

#[tokio::test]
async fn renamed_root_closes_two_open_transfer_calls_that_share_one_call_id() {
    let session = without_transfer_results(&shared_transfer_id_session(call_b_turn()).await).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let other = resume_agent("other", Arc::clone(&model), Arc::clone(&b)).build();

    let events = resume_handoff(&session, "other", &other).await;

    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: agent no longer available: triage"
    );
    assert_eq!(output_count(&events, "t"), 2, "{events:?}");
    assert_eq!(stored_results(&session, "t").await, 2);
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
    assert_eq!(
        results_after_each_call(&requests[0].messages, "t", 2),
        [
            "agent no longer available: budgeting",
            "agent no longer available: savings"
        ]
    );
}

#[tokio::test]
async fn missing_middle_agent_moves_two_recorded_transfer_results_that_share_one_call_id() {
    let session = shared_transfer_id_session(text_then_call_b_turn()).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let triage = resume_agent("triage", Arc::clone(&model), Arc::clone(&b)).build();

    let events = resume_handoff(&session, "triage", &triage).await;

    assert_eq!(
        tool_output_text(&events, "call_b"),
        "permission denied: agent no longer available: budgeting"
    );
    assert_no_output(&events, "t");
    assert_eq!(stored_results(&session, "t").await, 2);
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
    results_after_each_call(&requests[0].messages, "t", 2);
}

/// The events of a routing exit that fails before the target runs: the
/// open transfer call is closed, then the call gets the not-run result.
/// `updated` is true when the exit comes after `AgentUpdated` ("other").
fn assert_failed_route_closes_the_transfer(events: &[AgentEvent], error: &str, updated: bool) {
    let mut expected = vec!["RunStarted".to_owned()];
    if updated {
        assert!(
            matches!(&events[1], AgentEvent::AgentUpdated { agent } if agent == "budgeting"),
            "{events:?}"
        );
        expected.push("other".to_owned());
    }
    expected.extend([
        "ToolOutputItem(t1)".to_owned(),
        "ToolOutputItem(call_b)".to_owned(),
        format!("RunFailed({error})"),
    ]);
    assert_eq!(kinds(events), expected);
    assert_eq!(
        tool_output_text(events, "t1"),
        "agent no longer available: budgeting"
    );
    assert_eq!(tool_output_text(events, "call_b"), NOT_RUN);
}

#[tokio::test]
async fn route_over_the_depth_limit_closes_the_open_transfer_call() {
    let session = open_transfer_session(&["triage", "budgeting"]).await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let child_model = RequestLog::with_scripts(vec![]);
    let budgeting = resume_agent("budgeting", Arc::clone(&child_model), Arc::clone(&b)).build();
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(RequestLog::with_scripts(vec![]))
        .handoff(budgeting)
        .build();
    let ctx = session_ctx(&session)
        .with_run_config(paigasus_helikon_core::RunConfig::new().with_max_agent_depth(0));
    let failure = ctx.failure_handle();

    let events = resume_handoff_with(&session, "triage", &triage, ctx).await;

    assert!(b.invocations().is_empty());
    assert!(child_model.requests().is_empty());
    // The run fails on the depth limit, not on some other routing error.
    let expected = paigasus_helikon_core::AgentError::MaxAgentDepthExceeded { depth: 1, max: 0 };
    let error = expected.to_string();
    match failure.take() {
        Some(paigasus_helikon_core::AgentError::MaxAgentDepthExceeded { depth: 1, max: 0 }) => {}
        other => panic!("expected MaxAgentDepthExceeded {{ depth: 1, max: 0 }}, got {other:?}"),
    }
    assert!(
        matches!(events.last(), Some(AgentEvent::RunFailed { error: e }) if *e == error),
        "{:?}",
        events.last()
    );
    assert_failed_route_closes_the_transfer(&events, &error, false);
    assert_eq!(stored_results(&session, "t1").await, 1);
}

/// A handoff target named `budgeting` whose run fails to start.
struct FailingBudgeting;

#[async_trait]
impl Agent<()> for FailingBudgeting {
    fn name(&self) -> &str {
        "budgeting"
    }
    fn description(&self) -> &str {
        "fails"
    }
    async fn run(
        &self,
        _ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, paigasus_helikon_core::AgentError> {
        Err(paigasus_helikon_core::AgentError::NotImplemented { feature: "boom" })
    }
}

#[tokio::test]
async fn route_whose_target_fails_to_start_closes_the_open_transfer_call() {
    let session = open_transfer_session(&["triage", "budgeting"]).await;
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(RequestLog::with_scripts(vec![]))
        .handoff(FailingBudgeting)
        .build();

    let events = resume_handoff(&session, "triage", &triage).await;

    let error = paigasus_helikon_core::AgentError::NotImplemented { feature: "boom" }.to_string();
    assert_failed_route_closes_the_transfer(&events, &error, true);
    assert_eq!(stored_results(&session, "t1").await, 1);
}
