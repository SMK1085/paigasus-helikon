//! SMA-803 — `LlmAgent` resumes the resolved deferred calls of a session.

#[path = "common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{MockModel, MockTool};
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use paigasus_helikon_core::{
    open_deferrals, Agent, AgentEvent, AgentInput, CancellationToken, ContentPart, FinishReason,
    Guardrail, GuardrailError, GuardrailInput, GuardrailKind, GuardrailVerdict, Hook, HookDecision,
    HookEvent, HookRegistry, Item, LlmAgent, MemorySession, Model, ModelCapabilities, ModelError,
    ModelEvent, ModelRequest, PermissionDecision, PermissionPolicy, ResumePlan, RunContext,
    Session, SessionEvent, SessionRecorder, Tool, ToolResolution, TracerHandle,
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
