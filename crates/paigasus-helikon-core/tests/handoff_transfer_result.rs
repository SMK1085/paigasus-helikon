//! SMA-810 — the parent agent gives each handoff transfer call a tool result
//! directly after the handoff.

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use common::{assert_tool_calls_paired, DeferB, MockModel, MockTool, RequestLog};
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use paigasus_helikon_core::{
    open_deferrals, project, Agent, AgentError, AgentEvent, AgentInput, CancellationToken,
    ContentPart, FinishReason, Hook, HookDecision, HookEvent, HookRegistry, Item, LlmAgent,
    MemorySession, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest, ResumePlan,
    RunConfig, RunContext, Session, SessionEvent, SessionRecorder, SwarmAgent, Tool,
    ToolResolution, TracerHandle,
};

const CANCEL: &str = "tool call did not complete (run cancelled/timed out)";

// ── Fixtures ─────────────────────────────────────────────────────────────────

fn call(call_id: &str, tool: &str) -> ModelEvent {
    ModelEvent::ToolCallDelta {
        call_id: call_id.to_owned(),
        name: Some(tool.to_owned()),
        args_delta: "{}".to_owned(),
    }
}

/// One response with a call per `(call_id, tool)` pair.
fn calls_turn(calls: &[(&str, &str)]) -> Vec<ModelEvent> {
    let mut turn: Vec<ModelEvent> = calls.iter().map(|(id, tool)| call(id, tool)).collect();
    turn.push(ModelEvent::Finish {
        reason: FinishReason::ToolCalls,
    });
    turn
}

fn transfer_turn(call_id: &str, target: &str) -> Vec<ModelEvent> {
    calls_turn(&[(call_id, &format!("transfer_to_{target}"))])
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

fn agent(name: &str, scripts: Vec<Vec<ModelEvent>>) -> LlmAgent<(), MockModel> {
    LlmAgent::builder::<()>()
        .name(name)
        .description(format!("agent {name}"))
        .shared_model(MockModel::with_scripts(scripts))
        .build()
}

fn session_ctx(session: &Arc<MemorySession>) -> RunContext<()> {
    RunContext::new(
        Arc::new(()),
        Arc::clone(session) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
}

/// Run `root` with a session recorder. Returns the events and the drained log.
async fn recorded_run(
    root: &impl Agent<()>,
    ctx: RunContext<()>,
) -> (Vec<AgentEvent>, Vec<SessionEvent>) {
    let mut recorder = SessionRecorder::new(root.name());
    let input = AgentInput::from_user_text("hi");
    recorder.record_input(&input.messages);
    let events: Vec<AgentEvent> = root
        .run(ctx, input)
        .await
        .expect("run starts")
        .collect()
        .await;
    for ev in &events {
        recorder.observe(ev);
    }
    (events, recorder.drain())
}

async fn run_with_session(root: &impl Agent<()>) -> (Vec<AgentEvent>, Vec<SessionEvent>) {
    let session = Arc::new(MemorySession::new());
    recorded_run(root, session_ctx(&session)).await
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn text_of(content: &[ContentPart]) -> String {
    content
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => text.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

/// Every `ToolOutputItem` as `(call_id, text)`, in event order.
fn outputs(events: &[AgentEvent]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolOutputItem {
                item: Item::ToolResult { call_id, content },
            } => Some((call_id.clone(), text_of(content))),
            _ => None,
        })
        .collect()
}

/// A short label per event kind, for order checks.
fn kind(e: &AgentEvent) -> String {
    match e {
        AgentEvent::ToolCallItem {
            item: Item::ToolCall { call_id, .. },
        } => format!("call({call_id})"),
        AgentEvent::ToolOutputItem {
            item: Item::ToolResult { call_id, .. },
        } => format!("output({call_id})"),
        AgentEvent::HandoffItem { to, .. } => format!("handoff({to})"),
        AgentEvent::AgentUpdated { agent } => format!("updated({agent})"),
        AgentEvent::RunFailed { .. } => "failed".to_owned(),
        _ => "other".to_owned(),
    }
}

fn kinds(events: &[AgentEvent]) -> Vec<String> {
    events.iter().map(kind).filter(|k| k != "other").collect()
}

/// A short label per session event.
fn label(e: &SessionEvent) -> String {
    match e {
        SessionEvent::UserMessage { .. } => "user".to_owned(),
        SessionEvent::AssistantMessage { agent, .. } => format!("assistant({agent})"),
        SessionEvent::ToolCalled { call_id, .. } => format!("tool_called({call_id})"),
        SessionEvent::ToolReturned {
            call_id, content, ..
        } => format!("tool_returned({call_id}, {})", text_of(content)),
        SessionEvent::HandoffOccurred { to, .. } => format!("handoff({to})"),
        SessionEvent::ToolDeferred { call_id, .. } => format!("tool_deferred({call_id})"),
        other => format!("{other:?}"),
    }
}

fn labels(log: &[SessionEvent]) -> Vec<String> {
    log.iter().map(label).collect()
}

/// No transfer result for `call_id` and no `transferred` text anywhere.
fn assert_no_transfer_result(events: &[AgentEvent], call_id: &str) {
    let outs = outputs(events);
    assert!(
        !outs.iter().any(|(id, _)| id == call_id),
        "no ToolOutputItem for {call_id}: {outs:?}"
    );
    assert!(
        !outs.iter().any(|(_, t)| t.starts_with("transferred")),
        "no transferred text: {outs:?}"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::RunFailed { .. })),
        "{events:?}"
    );
}

// ── T1 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn transfer_result_follows_handoff_item() {
    let budgeting = agent("budgeting", vec![text_turn("Cut dining by $60.")]);
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(MockModel::with_scripts(vec![transfer_turn(
            "t1",
            "budgeting",
        )]))
        .handoff(budgeting)
        .build();

    let (events, log) = run_with_session(&triage).await;

    assert_eq!(
        kinds(&events),
        [
            "call(t1)",
            "handoff(budgeting)",
            "output(t1)",
            "updated(budgeting)"
        ],
        "{events:?}"
    );
    assert_eq!(
        outputs(&events),
        [("t1".to_owned(), "transferred to budgeting".to_owned())]
    );

    let labels = labels(&log);
    let ho = labels
        .iter()
        .position(|l| l == "handoff(budgeting)")
        .unwrap_or_else(|| panic!("no handoff: {labels:?}"));
    assert_eq!(
        labels[ho + 1],
        "tool_returned(t1, transferred to budgeting)",
        "{labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.contains(CANCEL)),
        "no cancel result: {labels:?}"
    );

    let messages = project(&log).messages;
    assert_tool_calls_paired(&messages);
    let at = messages
        .iter()
        .position(|i| matches!(i, Item::ToolCall { call_id, .. } if call_id == "t1"))
        .expect("ToolCall t1");
    assert!(
        matches!(&messages[at + 1], Item::ToolResult { call_id, .. } if call_id == "t1"),
        "{messages:?}"
    );
}

// ── T2 ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn two_transfer_calls_get_two_results_in_call_order() {
    let budgeting = agent("budgeting", vec![text_turn("ok")]);
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(MockModel::with_scripts(vec![calls_turn(&[
            ("t1", "transfer_to_budgeting"),
            ("t2", "transfer_to_budgeting"),
        ])]))
        .handoff(budgeting)
        .build();

    let (events, log) = run_with_session(&triage).await;

    assert_eq!(
        outputs(&events),
        [
            ("t1".to_owned(), "transferred to budgeting".to_owned()),
            (
                "t2".to_owned(),
                "transfer not performed: this response already transferred to budgeting".to_owned()
            ),
        ]
    );
    assert_eq!(
        kinds(&events),
        [
            "call(t1)",
            "call(t2)",
            "handoff(budgeting)",
            "output(t1)",
            "output(t2)",
            "updated(budgeting)"
        ],
        "{events:?}"
    );
    assert_tool_calls_paired(&project(&log).messages);
}

// ── T3 ───────────────────────────────────────────────────────────────────────

struct DenyHandoff;

#[async_trait]
impl Hook<()> for DenyHandoff {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        match event {
            HookEvent::OnHandoff { .. } => HookDecision::Deny {
                reason: "no".to_owned(),
            },
            _ => HookDecision::Allow,
        }
    }
}

/// An agent whose name changes from `budgeting` to `renamed` once `renamed`
/// is set, so the parent's target lookup finds no handoff.
struct RenamingAgent {
    renamed: Arc<AtomicBool>,
}

#[async_trait]
impl Agent<()> for RenamingAgent {
    fn name(&self) -> &str {
        if self.renamed.load(Ordering::SeqCst) {
            "renamed"
        } else {
            "budgeting"
        }
    }
    fn description(&self) -> &str {
        "renaming agent"
    }
    async fn run(
        &self,
        _ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        panic!("the parent must not find this agent");
    }
}

/// A [`MockModel`] that sets `flag` when it is called.
struct FlagOnInvoke {
    inner: Arc<MockModel>,
    flag: Arc<AtomicBool>,
}

#[async_trait]
impl Model for FlagOnInvoke {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.flag.store(true, Ordering::SeqCst);
        self.inner.invoke(request, cancel).await
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

#[tokio::test]
async fn no_result_for_a_handoff_that_did_not_occur() {
    // (a) An `OnHandoff` hook denies.
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(MockModel::with_scripts(vec![transfer_turn(
            "t1",
            "budgeting",
        )]))
        .handoff(agent("budgeting", vec![]))
        .hook(DenyHandoff)
        .build();
    let (events, _) = run_with_session(&triage).await;
    assert_no_transfer_result(&events, "t1");

    // (b) `max_agent_depth` is exceeded.
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(MockModel::with_scripts(vec![transfer_turn(
            "t1",
            "budgeting",
        )]))
        .handoff(agent("budgeting", vec![]))
        .build();
    let session = Arc::new(MemorySession::new());
    let ctx = session_ctx(&session).with_run_config(RunConfig::new().with_max_agent_depth(0));
    let (events, _) = recorded_run(&triage, ctx).await;
    assert!(
        matches!(events.last(), Some(AgentEvent::RunFailed { error }) if error.contains("nesting depth")),
        "{events:?}"
    );
    assert_no_transfer_result(&events, "t1");

    // (c) An unknown target: the target's name changes after the transfer
    // tools are built.
    let renamed = Arc::new(AtomicBool::new(false));
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(Arc::new(FlagOnInvoke {
            inner: MockModel::with_scripts(vec![transfer_turn("t1", "budgeting")]),
            flag: Arc::clone(&renamed),
        }))
        .shared_handoff(Arc::new(RenamingAgent {
            renamed: Arc::clone(&renamed),
        }))
        .build();
    let (events, _) = run_with_session(&triage).await;
    assert!(
        matches!(events.last(), Some(AgentEvent::RunFailed { error }) if error.contains("unknown handoff target")),
        "{events:?}"
    );
    assert_no_transfer_result(&events, "t1");

    // (d) A swarm with `max_handoffs(0)`.
    let swarm = SwarmAgent::builder()
        .name("swarm")
        .member(
            LlmAgent::builder::<()>()
                .name("triage")
                .description("triage")
                .shared_model(MockModel::with_scripts(vec![transfer_turn(
                    "call-1",
                    "budgeting",
                )]))
                .build(),
        )
        .member(agent("budgeting", vec![text_turn("ok")]))
        .entry("triage")
        .max_handoffs(0)
        .build()
        .unwrap();
    let (events, log) = run_with_session(&swarm).await;
    assert_no_transfer_result(&events, "call-1");
    assert_eq!(
        labels(&log),
        [
            "user".to_owned(),
            "tool_called(call-1)".to_owned(),
            format!("tool_returned(call-1, {CANCEL})"),
        ]
    );
}

// ── T4 ───────────────────────────────────────────────────────────────────────

/// An agent whose `run` fails to start.
struct FailsToStart;

#[async_trait]
impl Agent<()> for FailsToStart {
    fn name(&self) -> &str {
        "budgeting"
    }
    fn description(&self) -> &str {
        "fails to start"
    }
    async fn run(
        &self,
        _ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        Err(AgentError::Other(anyhow::anyhow!("internal detail")))
    }
}

#[tokio::test]
async fn target_run_error_gives_transfer_failed() {
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(MockModel::with_scripts(vec![transfer_turn(
            "t1",
            "budgeting",
        )]))
        .shared_handoff(Arc::new(FailsToStart))
        .build();

    let (events, log) = run_with_session(&triage).await;

    assert_eq!(
        kinds(&events),
        ["call(t1)", "handoff(budgeting)", "output(t1)", "failed"],
        "{events:?}"
    );
    assert_eq!(
        outputs(&events),
        [(
            "t1".to_owned(),
            "transfer failed: budgeting did not start".to_owned()
        )]
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::AgentUpdated { agent } if agent == "budgeting")),
        "{events:?}"
    );
    assert_tool_calls_paired(&project(&log).messages);
}

// ── Review Focus 3 and 4 ─────────────────────────────────────────────────────

#[tokio::test]
async fn custom_handoff_tool_name_gets_a_result() {
    // The tool name is the slug `transfer_to_budgeting_specialist`; it does
    // not hold the target name `Budgeting Specialist`.
    let budgeting = agent("Budgeting Specialist", vec![text_turn("ok")]);
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(MockModel::with_scripts(vec![calls_turn(&[(
            "t1",
            "transfer_to_budgeting_specialist",
        )])]))
        .handoff(budgeting)
        .build();

    let (events, log) = run_with_session(&triage).await;

    assert_eq!(
        outputs(&events),
        [(
            "t1".to_owned(),
            "transferred to Budgeting Specialist".to_owned()
        )]
    );
    assert_tool_calls_paired(&project(&log).messages);
}

#[tokio::test]
async fn nested_handoff_gives_each_transfer_call_its_own_result() {
    let leaf = agent("leaf", vec![text_turn("done")]);
    let mid = LlmAgent::builder::<()>()
        .name("mid")
        .description("mid")
        .shared_model(MockModel::with_scripts(vec![transfer_turn("t2", "leaf")]))
        .handoff(leaf)
        .build();
    let root = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(MockModel::with_scripts(vec![transfer_turn("t1", "mid")]))
        .handoff(mid)
        .build();

    let (events, log) = run_with_session(&root).await;

    assert_eq!(
        kinds(&events),
        [
            "call(t1)",
            "handoff(mid)",
            "output(t1)",
            "updated(mid)",
            "call(t2)",
            "handoff(leaf)",
            "output(t2)",
            "updated(leaf)"
        ],
        "{events:?}"
    );
    assert_eq!(
        outputs(&events),
        [
            ("t1".to_owned(), "transferred to mid".to_owned()),
            ("t2".to_owned(), "transferred to leaf".to_owned()),
        ]
    );
    assert_tool_calls_paired(&project(&log).messages);
}

// ── T6: three levels with one call id ────────────────────────────────────────

fn deferring_ctx(session: &Arc<MemorySession>) -> RunContext<()> {
    session_ctx(session).with_permission_policy(Arc::new(DeferB))
}

/// An agent `name` with the model `model` and the tool `b`.
fn b_agent(name: &str, model: Arc<RequestLog>, b: &Arc<MockTool>) -> Arc<dyn Agent<()>> {
    Arc::new(
        LlmAgent::builder::<()>()
            .name(name)
            .shared_model(model)
            .shared_tool(Arc::clone(b) as Arc<dyn Tool<()>>)
            .build(),
    )
}

/// An agent `name` with the model `model` that can hand off to `target`.
fn handoff_agent(
    name: &str,
    model: Arc<RequestLog>,
    target: Arc<dyn Agent<()>>,
) -> Arc<dyn Agent<()>> {
    Arc::new(
        LlmAgent::builder::<()>()
            .name(name)
            .shared_model(model)
            .shared_handoff(target)
            .build(),
    )
}

/// `triage → budgeting → savings`, with a model per agent from `scripts`.
/// Returns the root and the three models.
fn fc_0_chain(
    scripts: [Vec<Vec<ModelEvent>>; 3],
    b: &Arc<MockTool>,
) -> (Arc<dyn Agent<()>>, Vec<Arc<RequestLog>>) {
    let models: Vec<Arc<RequestLog>> = scripts.into_iter().map(RequestLog::with_scripts).collect();
    let savings = b_agent("savings", Arc::clone(&models[2]), b);
    let budgeting = handoff_agent("budgeting", Arc::clone(&models[1]), savings);
    let triage = handoff_agent("triage", Arc::clone(&models[0]), budgeting);
    (triage, models)
}

/// Run `triage(fc_0) → budgeting(fc_0) → savings`, where savings defers its
/// call to `b`, also with the call id `fc_0`. Store the run in a new session.
async fn fc_0_deferred_session() -> (Arc<MemorySession>, Vec<AgentEvent>, Vec<Arc<RequestLog>>) {
    let b = MockTool::new("b", serde_json::json!({}));
    let (triage, models) = fc_0_chain(
        [
            vec![transfer_turn("fc_0", "budgeting")],
            vec![transfer_turn("fc_0", "savings")],
            vec![calls_turn(&[("fc_0", "b")])],
        ],
        &b,
    );
    let session = Arc::new(MemorySession::new());
    let ctx = deferring_ctx(&session).with_deferred_tool_calls(true);
    let mut recorder = SessionRecorder::new("triage").with_deferral_slot(ctx.deferral_handle());
    let input = AgentInput::from_user_text("hi");
    recorder.record_input(&input.messages);
    let events: Vec<AgentEvent> = triage
        .run(ctx, input)
        .await
        .expect("run starts")
        .collect()
        .await;
    for ev in &events {
        recorder.observe(ev);
    }
    session.append(&recorder.drain()).await.unwrap();
    assert!(b.invocations().is_empty(), "a deferred call does not run");
    (session, events, models)
}

/// Resume the one open deferral of `session` as approved, with the root
/// `root` and the agent `agent`. Returns the events. The recorder closes the
/// deferral.
async fn resume_approved(
    session: &Arc<MemorySession>,
    root: &str,
    agent: &Arc<dyn Agent<()>>,
) -> Vec<AgentEvent> {
    let log = session.events(None).await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    let open = open_deferrals(&log);
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0].call_id(), "fc_0");
    let input = AgentInput::from_user_text("next")
        .with_resolution(open[0].deferral_id(), ToolResolution::Approved);
    let plan = ResumePlan::from_events(&log, &snapshot, root, &input, 8).expect("valid resume");
    let input = input.with_resume_plan(plan.clone());
    let mut recorder = SessionRecorder::new(root).with_resume_plan(&plan);
    recorder.record_input(&input.messages);
    let events: Vec<AgentEvent> = agent
        .run(deferring_ctx(session), input)
        .await
        .expect("run starts")
        .collect()
        .await;
    for ev in &events {
        recorder.observe(ev);
    }
    session.append(&recorder.drain()).await.unwrap();
    assert!(
        open_deferrals(&session.events(None).await.unwrap()).is_empty(),
        "{events:?}"
    );
    events
}

/// The stored results whose text starts with `transferred`.
async fn stored_transfer_results(session: &MemorySession) -> usize {
    session
        .events(None)
        .await
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(e, SessionEvent::ToolReturned { content, .. }
                if text_of(content).starts_with("transferred"))
        })
        .count()
}

/// The resume of `fc_0` was denied with `reason`, and it added no transfer
/// result.
async fn assert_denied_with_no_new_transfer_result(
    session: &MemorySession,
    events: &[AgentEvent],
    reason: &str,
) {
    let outs = outputs(events);
    assert_eq!(
        outs,
        [("fc_0".to_owned(), format!("permission denied: {reason}"))],
        "{events:?}"
    );
    assert_eq!(stored_transfer_results(session).await, 2);
    // Two transfer results and the denial of the deferred call.
    let fc_0_results = session
        .events(None)
        .await
        .unwrap()
        .iter()
        .filter(|e| matches!(e, SessionEvent::ToolReturned { call_id, .. } if call_id == "fc_0"))
        .count();
    assert_eq!(fc_0_results, 3);
}

#[tokio::test]
async fn three_levels_with_one_call_id_defer_and_resume() {
    // (a) The run defers, and each transfer call has its result before the
    // next call with the same id.
    let (session, events, models) = fc_0_deferred_session().await;

    assert!(
        matches!(events.last(), Some(AgentEvent::RunDeferred { .. })),
        "{events:?}"
    );
    let log = session.events(None).await.unwrap();
    assert_eq!(
        labels(&log),
        [
            "user",
            "tool_called(fc_0)",
            "handoff(budgeting)",
            "tool_returned(fc_0, transferred to budgeting)",
            "tool_called(fc_0)",
            "handoff(savings)",
            "tool_returned(fc_0, transferred to savings)",
            "tool_called(fc_0)",
            "tool_deferred(fc_0)",
        ]
    );
    assert!(!labels(&log).iter().any(|l| l.contains(CANCEL)));
    for m in &models {
        for r in &m.requests() {
            assert_tool_calls_paired(&r.messages);
        }
    }

    // (b) A resume that approves runs `b` once, on savings.
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let (triage, models) = fc_0_chain([vec![], vec![], vec![text_turn("done")]], &b);

    let events = resume_approved(&session, "triage", &triage).await;

    assert_eq!(b.invocations().len(), 1, "b runs once");
    assert_eq!(
        outputs(&events),
        [("fc_0".to_owned(), r#"{"paid":true}"#.to_owned())]
    );
    assert!(models[0].requests().is_empty());
    assert!(models[1].requests().is_empty());
    let requests = models[2].requests();
    assert_eq!(requests.len(), 1);
    for r in &requests {
        assert_tool_calls_paired(&r.messages);
    }
    assert_eq!(stored_transfer_results(&session).await, 2);
}

#[tokio::test]
async fn three_levels_with_one_call_id_deny_on_a_renamed_root() {
    // (c) The root is now `other`, so the path cannot be followed.
    let (session, _, _) = fc_0_deferred_session().await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let other = b_agent("other", Arc::clone(&model), &b);

    let events = resume_approved(&session, "other", &other).await;

    assert!(b.invocations().is_empty(), "a denied call does not run");
    assert_denied_with_no_new_transfer_result(
        &session,
        &events,
        "agent no longer available: triage",
    )
    .await;
    for r in &model.requests() {
        assert_tool_calls_paired(&r.messages);
    }
}

#[tokio::test]
async fn three_levels_with_one_call_id_deny_on_a_missing_middle_agent() {
    // (c) triage hands off to savings now, not to budgeting.
    let (session, _, _) = fc_0_deferred_session().await;
    let b = MockTool::new("b", serde_json::json!({ "paid": true }));
    let model = RequestLog::with_scripts(vec![text_turn("ok")]);
    let savings = b_agent("savings", RequestLog::with_scripts(vec![]), &b);
    let triage = handoff_agent("triage", Arc::clone(&model), savings);

    let events = resume_approved(&session, "triage", &triage).await;

    assert!(b.invocations().is_empty(), "a denied call does not run");
    assert_denied_with_no_new_transfer_result(
        &session,
        &events,
        "agent no longer available: budgeting",
    )
    .await;
    for r in &model.requests() {
        assert_tool_calls_paired(&r.messages);
    }
}
