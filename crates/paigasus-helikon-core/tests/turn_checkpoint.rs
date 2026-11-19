//! SMA-804: the turn-boundary checkpoint (`Continue` and `Inject`).

#[path = "common/mod.rs"]
mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken, CheckpointDecision, ContentPart,
    FinishReason, Guardrail, GuardrailError, GuardrailInput, GuardrailVerdict, Hook, HookDecision,
    HookEvent, Instructions, Item, LlmAgent, MediaSource, Model, ModelCapabilities, ModelError,
    ModelEvent, ModelRequest, ModelSettings, RunConfig, RunContext, SessionEvent, SessionRecorder,
    TurnBoundary, TurnCheckpoint,
};

use common::{noop_run_context, MockTool};

/// Pops one decision per call; `Continue` when empty. Records every boundary.
pub struct ScriptedCheckpoint {
    decisions: Mutex<VecDeque<CheckpointDecision>>,
    seen: Mutex<Vec<TurnBoundary>>,
}

impl ScriptedCheckpoint {
    /// Script the decisions, one per boundary.
    pub fn new(decisions: Vec<CheckpointDecision>) -> Arc<Self> {
        Arc::new(Self {
            decisions: Mutex::new(VecDeque::from(decisions)),
            seen: Mutex::new(Vec::new()),
        })
    }
    /// The boundaries the driver reported, in order.
    pub fn boundaries(&self) -> Vec<TurnBoundary> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl TurnCheckpoint<()> for ScriptedCheckpoint {
    async fn at_boundary(
        &self,
        _ctx: &RunContext<()>,
        boundary: &TurnBoundary,
    ) -> CheckpointDecision {
        self.seen.lock().unwrap().push(boundary.clone());
        self.decisions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(CheckpointDecision::Continue)
    }
}

/// Scripted model that records the messages of every request.
struct RecordingModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
    requests: Arc<Mutex<Vec<Vec<Item>>>>,
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
            .ok_or_else(|| ModelError::Other(anyhow::anyhow!("no more scripts")))?;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

fn tool_turn(n: usize) -> Vec<ModelEvent> {
    let mut v: Vec<ModelEvent> = (0..n)
        .map(|i| ModelEvent::ToolCallDelta {
            call_id: format!("c{i}"),
            name: Some("noop".into()),
            args_delta: "{}".into(),
        })
        .collect();
    v.push(ModelEvent::Finish {
        reason: FinishReason::ToolCalls,
    });
    v
}

fn text_turn(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta { text: text.into() },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

type Requests = Arc<Mutex<Vec<Vec<Item>>>>;

fn model(scripts: Vec<Vec<ModelEvent>>) -> (Arc<RecordingModel>, Requests) {
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    (
        Arc::new(RecordingModel {
            scripts: Mutex::new(VecDeque::from(scripts)),
            requests: Arc::clone(&requests),
        }),
        requests,
    )
}

fn agent(
    model: Arc<RecordingModel>,
    config: RunConfig,
    input_guardrails: Vec<Arc<dyn Guardrail<()>>>,
    hooks: Vec<Arc<dyn Hook<()>>>,
) -> LlmAgent<(), RecordingModel> {
    LlmAgent::<(), _> {
        name: "agent".into(),
        description: "checkpoint test".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools: vec![MockTool::new("noop", serde_json::json!({"ok": true})) as _],
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails,
        output_guardrails: Vec::new(),
        hooks,
        model_settings: ModelSettings::new(),
        config,
        _output: std::marker::PhantomData,
    }
}

struct Outcome {
    events: Vec<AgentEvent>,
    failure: Option<AgentError>,
}

async fn run_with(agent: &impl Agent<()>, cp: Option<Arc<ScriptedCheckpoint>>) -> Outcome {
    use futures_util::StreamExt as _;
    let mut ctx = noop_run_context::<()>();
    if let Some(cp) = cp {
        ctx = ctx.with_turn_checkpoint(cp);
    }
    let failure = ctx.failure_handle();
    let mut s = agent
        .run(ctx, AgentInput::from_user_text("hi"))
        .await
        .expect("run starts");
    let mut events = Vec::new();
    while let Some(e) = s.next().await {
        events.push(e);
    }
    Outcome {
        events,
        failure: failure.take(),
    }
}

fn kinds(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            let d = format!("{e:?}");
            d.split(|c: char| !c.is_alphanumeric())
                .next()
                .unwrap_or("")
                .to_owned()
        })
        .collect()
}

fn text(s: &str) -> ContentPart {
    ContentPart::Text { text: s.into() }
}

#[tokio::test]
async fn continue_matches_no_checkpoint_stream() {
    let scripts = || vec![tool_turn(1), text_turn("done")];
    let (m1, _) = model(scripts());
    let base = run_with(&agent(m1, RunConfig::default(), vec![], vec![]), None).await;
    let (m2, _) = model(scripts());
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Continue]);
    let with = run_with(
        &agent(m2, RunConfig::default(), vec![], vec![]),
        Some(cp.clone()),
    )
    .await;
    assert_eq!(kinds(&base.events), kinds(&with.events));
    assert_eq!(cp.boundaries(), vec![TurnBoundary::new(0, 1, "agent", 0)]);
}

#[tokio::test]
async fn inject_reaches_next_request_after_tool_result() {
    let (m, requests) = model(vec![tool_turn(1), text_turn("done")]);
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Inject(vec![text("news")])]);
    let out = run_with(&agent(m, RunConfig::default(), vec![], vec![]), Some(cp)).await;
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    let n = reqs[1].len();
    assert!(
        matches!(reqs[1][n - 2], Item::ToolResult { .. }),
        "{:?}",
        reqs[1]
    );
    assert_eq!(
        format!("{:?}", reqs[1][n - 1]),
        format!(
            "{:?}",
            Item::UserMessage {
                content: vec![text("news")]
            }
        )
    );
    let k = kinds(&out.events);
    let i = k.iter().position(|x| x == "ToolOutputItem").unwrap();
    assert_eq!(
        &k[i..i + 3],
        ["ToolOutputItem", "UserMessageInjected", "TurnStarted"]
    );
    assert!(out.failure.is_none());
}

#[tokio::test]
async fn inject_empty_is_continue() {
    let (m, requests) = model(vec![tool_turn(1), text_turn("done")]);
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Inject(vec![])]);
    let out = run_with(&agent(m, RunConfig::default(), vec![], vec![]), Some(cp)).await;
    assert!(!kinds(&out.events).contains(&"UserMessageInjected".to_owned()));
    let reqs = requests.lock().unwrap();
    assert!(matches!(reqs[1].last(), Some(Item::ToolResult { .. })));
}

#[tokio::test]
async fn inject_tool_part_fails_before_any_record() {
    let (m, requests) = model(vec![tool_turn(1), text_turn("done")]);
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Inject(vec![
        ContentPart::ToolResult {
            call_id: "x".into(),
            content: vec![],
        },
    ])]);
    let out = run_with(&agent(m, RunConfig::default(), vec![], vec![]), Some(cp)).await;
    assert!(matches!(
        out.events.last(),
        Some(AgentEvent::RunFailed { .. })
    ));
    assert!(!kinds(&out.events).contains(&"UserMessageInjected".to_owned()));
    match out.failure {
        Some(AgentError::InvalidInjection { reason }) => assert!(reason.contains("ToolResult")),
        other => panic!("expected InvalidInjection, got {other:?}"),
    }
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn checkpoint_not_called_at_max_turns() {
    let (m, _) = model(vec![tool_turn(1), text_turn("done")]);
    let cp = ScriptedCheckpoint::new(vec![]);
    let mut cfg = RunConfig::default();
    cfg.max_turns = 1;
    let out = run_with(&agent(m, cfg, vec![], vec![]), Some(cp.clone())).await;
    assert!(cp.boundaries().is_empty());
    assert!(matches!(out.failure, Some(AgentError::MaxTurnsExceeded(1))));
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct Answer {
    a: String,
}

#[tokio::test]
async fn checkpoint_not_called_for_finalizing_turn() {
    let (m, _) = model(vec![
        tool_turn(1),
        text_turn("free text"),
        text_turn("{\"a\":\"x\"}"),
    ]);
    let a = LlmAgent::builder::<()>()
        .name("agent")
        .shared_model(m)
        .instructions("")
        .shared_tool(MockTool::new("noop", serde_json::json!({"ok": true})))
        .output_type::<Answer>()
        .build();
    let cp = ScriptedCheckpoint::new(vec![]);
    let out = run_with(&a, Some(cp.clone())).await;
    assert!(out.failure.is_none(), "{:?}", out.failure);
    assert_eq!(cp.boundaries().len(), 1);
}

struct DenyTurn1;
#[async_trait]
impl Hook<()> for DenyTurn1 {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        match event {
            HookEvent::OnTurnStart { turn: 1 } => HookDecision::Deny {
                reason: "no".into(),
            },
            _ => HookDecision::Allow,
        }
    }
}

#[tokio::test]
async fn inject_then_on_turn_start_deny_keeps_commit() {
    let (m, _) = model(vec![tool_turn(1), text_turn("done")]);
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Inject(vec![text("news")])]);
    let a = agent(
        m,
        RunConfig::default(),
        vec![],
        vec![Arc::new(DenyTurn1) as Arc<dyn Hook<()>>],
    );
    let out = run_with(&a, Some(cp)).await;
    let k = kinds(&out.events);
    let inj = k.iter().position(|x| x == "UserMessageInjected").unwrap();
    let fail = k.iter().position(|x| x == "RunFailed").unwrap();
    assert!(inj < fail);
    assert!(matches!(out.failure, Some(AgentError::HookDenied { .. })));
}

#[tokio::test]
async fn checkpoint_runs_once_after_parallel_results() {
    let (m, _) = model(vec![tool_turn(2), text_turn("done")]);
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Inject(vec![text("news")])]);
    let out = run_with(
        &agent(m, RunConfig::default(), vec![], vec![]),
        Some(cp.clone()),
    )
    .await;
    assert_eq!(cp.boundaries().len(), 1);
    let k = kinds(&out.events);
    let inj = k.iter().position(|x| x == "UserMessageInjected").unwrap();
    let outs: Vec<usize> = k
        .iter()
        .enumerate()
        .filter(|(_, x)| *x == "ToolOutputItem")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(outs.len(), 2);
    assert!(outs.iter().all(|i| *i < inj));
}

/// Records the text each input guardrail check receives.
struct RecordingGuardrail {
    seen: Arc<Mutex<Vec<String>>>,
}
#[async_trait]
impl Guardrail<()> for RecordingGuardrail {
    async fn check(
        &self,
        _: &RunContext<()>,
        input: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError> {
        if let GuardrailInput::UserText(t) = input {
            self.seen.lock().unwrap().push(t.to_owned());
        }
        Ok(GuardrailVerdict::Pass)
    }
}

#[tokio::test]
async fn inject_image_only_passes_guardrails_and_is_sent() {
    let (m, requests) = model(vec![tool_turn(1), text_turn("done")]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::new(RecordingGuardrail {
        seen: Arc::clone(&seen),
    }) as Arc<dyn Guardrail<()>>;
    let img = ContentPart::Image {
        source: MediaSource::Url {
            url: "https://example.com/a.png".into(),
        },
    };
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Inject(vec![img.clone()])]);
    let out = run_with(&agent(m, RunConfig::default(), vec![g], vec![]), Some(cp)).await;
    assert!(out.failure.is_none());
    assert_eq!(*seen.lock().unwrap(), vec!["hi".to_owned(), String::new()]);
    let reqs = requests.lock().unwrap();
    assert_eq!(
        format!("{:?}", reqs[1].last().unwrap()),
        format!("{:?}", Item::UserMessage { content: vec![img] })
    );
}

#[tokio::test]
async fn checkpoint_not_called_without_tool_calls() {
    let (m, _) = model(vec![text_turn("done")]);
    let cp = ScriptedCheckpoint::new(vec![]);
    let _ = run_with(
        &agent(m, RunConfig::default(), vec![], vec![]),
        Some(cp.clone()),
    )
    .await;
    assert!(cp.boundaries().is_empty());
}

/// Insert a `Usage` snapshot before the `Finish` event of a script.
fn with_usage(mut events: Vec<ModelEvent>, input: u32, output: u32) -> Vec<ModelEvent> {
    let at = events.len() - 1;
    events.insert(
        at,
        ModelEvent::Usage {
            input_tokens: input,
            output_tokens: output,
            cached_input_tokens: None,
            reasoning_tokens: None,
        },
    );
    events
}

/// Counts `OnRunComplete` hook events.
struct CountRunComplete(Arc<Mutex<u32>>);
#[async_trait]
impl Hook<()> for CountRunComplete {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        if matches!(event, HookEvent::OnRunComplete) {
            *self.0.lock().unwrap() += 1;
        }
        HookDecision::Allow
    }
}

/// Trips on user text containing "bad", or errors on it.
struct BadTextGuardrail {
    error: bool,
}
#[async_trait]
impl Guardrail<()> for BadTextGuardrail {
    async fn check(
        &self,
        _: &RunContext<()>,
        input: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError> {
        if let GuardrailInput::UserText(t) = input {
            if t.contains("bad") {
                if self.error {
                    return Err(GuardrailError::Other(anyhow::anyhow!("boom")));
                }
                return Ok(GuardrailVerdict::Tripwire {
                    kind: paigasus_helikon_core::GuardrailKind::InputPolicy,
                    info: serde_json::json!({}),
                });
            }
        }
        Ok(GuardrailVerdict::Pass)
    }
}

fn recorded(events: &[AgentEvent]) -> Vec<SessionEvent> {
    let mut r = SessionRecorder::new("agent");
    for e in events {
        r.observe(e);
    }
    r.drain()
}

#[tokio::test]
async fn stop_ends_cleanly_at_boundary() {
    let runs = Arc::new(Mutex::new(0u32));
    let build = || {
        let (m, requests) = model(vec![with_usage(tool_turn(2), 10, 4), text_turn("never")]);
        let a = agent(
            m,
            RunConfig::default(),
            vec![],
            vec![Arc::new(CountRunComplete(Arc::clone(&runs))) as Arc<dyn Hook<()>>],
        );
        (a, requests)
    };
    let (a, requests) = build();
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Stop {
        reason: "operator".into(),
    }]);
    let out = run_with(&a, Some(cp)).await;
    match out.events.last().unwrap() {
        AgentEvent::RunStopped { reason, usage } => {
            assert_eq!(reason, "operator");
            assert_eq!(usage.input_tokens, 10);
            assert_eq!(usage.output_tokens, 4);
        }
        other => panic!("last event: {other:?}"),
    }
    let k = kinds(&out.events);
    assert!(!k.contains(&"RunFailed".to_owned()), "{k:?}");
    assert_eq!(
        k.iter().filter(|x| *x == "TurnStarted").count(),
        1,
        "no TurnStarted for turn 1: {k:?}"
    );
    assert_eq!(requests.lock().unwrap().len(), 1, "model called once");
    assert!(out.failure.is_none());
    assert_eq!(*runs.lock().unwrap(), 1, "OnRunComplete exactly once");
    let rec = recorded(&out.events);
    let called = rec
        .iter()
        .filter(|e| matches!(e, SessionEvent::ToolCalled { .. }))
        .count();
    let returned = rec
        .iter()
        .filter(|e| matches!(e, SessionEvent::ToolReturned { .. }))
        .count();
    assert_eq!(called, 2);
    assert_eq!(called, returned);

    // collect() over the same stream shape.
    let (a2, _) = build();
    let ctx = noop_run_context::<()>().with_turn_checkpoint(ScriptedCheckpoint::new(vec![
        CheckpointDecision::Stop {
            reason: "operator".into(),
        },
    ]));
    let slot = ctx.failure_handle();
    let stream = a2
        .run(ctx, AgentInput::from_user_text("hi"))
        .await
        .expect("run starts");
    let collected = paigasus_helikon_core::RunResultStreaming::with_failure(stream, slot.clone())
        .collect()
        .await;
    // Today `collect()` reads usage only from `RunCompleted`, so it returns
    // `Ok` with a default usage; SMA-803 connects `RunStopped` to `RunResult`.
    let collected = collected.expect("a stopped run is not an error");
    assert!(matches!(
        collected.events.last(),
        Some(AgentEvent::RunStopped { .. })
    ));
    assert!(slot.take().is_none());
}

async fn tripwire_case(error: bool) {
    let (m, requests) = model(vec![tool_turn(1), text_turn("never")]);
    let g = Arc::new(BadTextGuardrail { error }) as Arc<dyn Guardrail<()>>;
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Inject(vec![text("bad")])]);
    let out = run_with(&agent(m, RunConfig::default(), vec![g], vec![]), Some(cp)).await;
    let k = kinds(&out.events);
    let n = k.len();
    assert_eq!(&k[n - 2..], ["GuardrailTriggered", "RunStopped"], "{k:?}");
    assert!(matches!(
        out.events.last().unwrap(),
        AgentEvent::RunStopped { reason, .. } if reason == "input guardrail tripwire"
    ));
    assert!(!k.contains(&"UserMessageInjected".to_owned()));
    assert!(!k.contains(&"RunFailed".to_owned()));
    assert_eq!(requests.lock().unwrap().len(), 1, "model called once");
    assert!(out.failure.is_none());
    let rec = recorded(&out.events);
    assert!(!rec.iter().any(|e| matches!(
        e,
        SessionEvent::UserMessage { content, .. } if format!("{content:?}").contains("bad")
    )));
}

#[tokio::test]
async fn guardrail_tripwire_on_injection_stops() {
    tripwire_case(false).await;
}

#[tokio::test]
async fn guardrail_error_on_injection_stops() {
    tripwire_case(true).await;
}

fn handoff_parent(
    child: LlmAgent<(), common::MockModel>,
    parent_script: Vec<ModelEvent>,
) -> impl Agent<()> {
    LlmAgent::builder::<()>()
        .name("parent")
        .shared_model(common::MockModel::with_scripts(vec![parent_script]))
        .handoff(child)
        .build()
}

fn handoff_child(scripts: Vec<Vec<ModelEvent>>) -> LlmAgent<(), common::MockModel> {
    LlmAgent::builder::<()>()
        .name("child")
        .description("child agent")
        .shared_model(common::MockModel::with_scripts(scripts))
        .shared_tool(MockTool::new("noop", serde_json::json!({"ok": true})) as _)
        .build()
}

fn transfer(usage: (u32, u32)) -> Vec<ModelEvent> {
    with_usage(
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "t1".into(),
                name: Some("transfer_to_child".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        usage.0,
        usage.1,
    )
}

#[tokio::test]
async fn handoff_child_stop_ends_whole_run() {
    let child = handoff_child(vec![with_usage(tool_turn(1), 20, 7), text_turn("never")]);
    let parent = handoff_parent(child, transfer((10, 5)));
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Stop {
        reason: "operator".into(),
    }]);
    let out = run_with(&parent, Some(cp.clone())).await;
    assert_eq!(cp.boundaries(), vec![TurnBoundary::new(0, 1, "child", 1)]);
    match out.events.last().unwrap() {
        AgentEvent::RunStopped { reason, usage } => {
            assert_eq!(reason, "operator");
            assert_eq!(usage.input_tokens, 30);
            assert_eq!(usage.output_tokens, 12);
        }
        other => panic!("last event: {other:?}"),
    }
    assert!(out.failure.is_none());
}

#[tokio::test]
async fn inject_then_stop_across_two_boundaries() {
    let (m, requests) = model(vec![
        with_usage(tool_turn(1), 10, 1),
        with_usage(tool_turn(1), 10, 1),
        text_turn("never"),
    ]);
    let cp = ScriptedCheckpoint::new(vec![
        CheckpointDecision::Inject(vec![text("a")]),
        CheckpointDecision::Stop { reason: "b".into() },
    ]);
    let out = run_with(
        &agent(m, RunConfig::default(), vec![], vec![]),
        Some(cp.clone()),
    )
    .await;
    let reqs = requests.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        format!("{:?}", reqs[1].last().unwrap()),
        format!(
            "{:?}",
            Item::UserMessage {
                content: vec![text("a")]
            }
        )
    );
    assert_eq!(cp.boundaries()[1].completed_turn, 1);
    match out.events.last().unwrap() {
        AgentEvent::RunStopped { reason, usage } => {
            assert_eq!(reason, "b");
            assert_eq!(usage.input_tokens, 20);
            assert_eq!(usage.output_tokens, 2);
        }
        other => panic!("last event: {other:?}"),
    }
}

#[tokio::test]
async fn handoff_child_inject_is_recorded_in_order() {
    let child = handoff_child(vec![tool_turn(1), text_turn("done")]);
    let parent = handoff_parent(child, transfer((1, 1)));
    let cp = ScriptedCheckpoint::new(vec![CheckpointDecision::Inject(vec![text("x")])]);
    let out = run_with(&parent, Some(cp)).await;
    assert!(out.failure.is_none());
    let rec = recorded(&out.events);
    let i = rec
        .iter()
        .position(|e| matches!(e, SessionEvent::UserMessage { .. }))
        .expect("injected user message recorded");
    assert!(matches!(
        rec[i - 1],
        SessionEvent::ToolReturned { ref call_id, .. } if call_id == "c0"
    ));
    assert_eq!(
        format!(
            "{:?}",
            match &rec[i] {
                SessionEvent::UserMessage { content, .. } => content.clone(),
                _ => unreachable!(),
            }
        ),
        format!("{:?}", vec![text("x")])
    );
}
