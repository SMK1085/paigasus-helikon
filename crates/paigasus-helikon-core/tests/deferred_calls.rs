//! SMA-803 — the agent loop ends a turn that has deferred tool calls.

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use common::{MockModel, MockTool};
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, ContentPart, DenyRule, FinishReason, Hook,
    HookDecision, HookEvent, HookRegistry, Item, LlmAgent, Model, ModelCapabilities, ModelError,
    ModelEvent, ModelRequest, PermissionDecision, PermissionPolicy, RunConfig, RunContext,
    RunResult, RunResultStreaming, StopReason, Tool,
};

const PROMPT: &str = "policy says wait";
const SECRET: &str = "sk-test-SECRET-1234";

/// Defers the calls to the named tools (every tool when `None`); allows the rest.
struct DeferPolicy(Option<Vec<&'static str>>);

impl DeferPolicy {
    fn only_b() -> Arc<Self> {
        Arc::new(Self(Some(vec!["b"])))
    }
    fn everything() -> Arc<Self> {
        Arc::new(Self(None))
    }
}

#[async_trait]
impl PermissionPolicy<()> for DeferPolicy {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        tool: &str,
        _args: &serde_json::Value,
    ) -> PermissionDecision {
        let defers = self.0.as_ref().is_none_or(|names| names.contains(&tool));
        if defers {
            PermissionDecision::Defer {
                prompt: PROMPT.to_owned(),
            }
        } else {
            PermissionDecision::Allow
        }
    }
}

/// A [`MockModel`] that counts its invocations.
struct RecordingModel {
    inner: Arc<MockModel>,
    invocations: AtomicUsize,
}

impl RecordingModel {
    fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            inner: MockModel::with_scripts(scripts),
            invocations: AtomicUsize::new(0),
        })
    }
    fn invocations(&self) -> usize {
        self.invocations.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Model for RecordingModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        self.inner.invoke(request, cancel).await
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// A model turn that calls each `(call_id, tool)` with `{}`, with optional usage.
fn tool_turn(calls: &[(&str, &str)], input_tokens: Option<u32>) -> Vec<ModelEvent> {
    let mut turn: Vec<ModelEvent> = calls
        .iter()
        .map(|(call_id, tool)| ModelEvent::ToolCallDelta {
            call_id: (*call_id).to_owned(),
            name: Some((*tool).to_owned()),
            args_delta: "{}".to_owned(),
        })
        .collect();
    if let Some(input_tokens) = input_tokens {
        turn.push(ModelEvent::Usage {
            input_tokens,
            output_tokens: 0,
            cached_input_tokens: None,
            reasoning_tokens: None,
        });
    }
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

/// A short label for the events these tests check; `"other"` for the rest.
fn kind(ev: &AgentEvent) -> String {
    match ev {
        AgentEvent::ToolOutputItem {
            item: Item::ToolResult { call_id, .. },
        } => format!("ToolOutputItem({call_id})"),
        AgentEvent::ApprovalRequested { call_id, .. } => format!("ApprovalRequested({call_id})"),
        AgentEvent::PermissionDenied { tool, .. } => format!("PermissionDenied({tool})"),
        AgentEvent::RunDeferred { .. } => "RunDeferred".to_owned(),
        AgentEvent::RunCompleted { .. } => "RunCompleted".to_owned(),
        AgentEvent::RunFailed { error } => format!("RunFailed({error})"),
        _ => "other".to_owned(),
    }
}

fn kinds(events: &[AgentEvent]) -> Vec<String> {
    events.iter().map(kind).collect()
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
            } if id == call_id => Some(
                content
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no ToolOutputItem for {call_id}"))
}

/// The `calls` of the last `RunDeferred` event.
fn deferred_calls(events: &[AgentEvent]) -> Vec<paigasus_helikon_core::PendingToolCall> {
    match events.last() {
        Some(AgentEvent::RunDeferred { calls, .. }) => calls.clone(),
        other => panic!("expected RunDeferred as the last event, got {other:?}"),
    }
}

async fn collect(agent: &impl Agent<()>, ctx: RunContext<()>) -> RunResult {
    let stream = agent
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .expect("run starts");
    RunResultStreaming::new(stream)
        .collect()
        .await
        .expect("run collects")
}

fn deferring_ctx() -> RunContext<()> {
    RunContext::ephemeral(())
        .with_deferred_tool_calls(true)
        .with_permission_policy(DeferPolicy::only_b())
}

#[tokio::test]
async fn turn_with_allowed_and_deferred_call_ends_with_run_deferred() {
    let (a, b, c) = (
        MockTool::new("a", serde_json::json!({"ok": "a"})),
        MockTool::new("b", serde_json::json!({"ok": "b"})),
        MockTool::new("c", serde_json::json!({"ok": "c"})),
    );
    let model = RecordingModel::with_scripts(vec![tool_turn(
        &[("call_a", "a"), ("call_b", "b"), ("call_c", "c")],
        None,
    )]);
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(Arc::clone(&model))
        .shared_tool(Arc::clone(&a) as Arc<dyn Tool<()>>)
        .shared_tool(Arc::clone(&b) as Arc<dyn Tool<()>>)
        .shared_tool(Arc::clone(&c) as Arc<dyn Tool<()>>)
        .build();
    let ctx = deferring_ctx();
    let slot = ctx.deferral_handle();

    let result = collect(&agent, ctx).await;
    let ks = kinds(&result.events);
    assert_eq!(
        ks[ks.len() - 4..],
        [
            "ToolOutputItem(call_a)",
            "ToolOutputItem(call_c)",
            "ApprovalRequested(call_b)",
            "RunDeferred",
        ],
        "event kinds: {ks:?}"
    );

    let calls = deferred_calls(&result.events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].call_id(), "call_b");
    assert_eq!(calls[0].tool(), "b");
    assert_eq!(calls[0].prompt(), PROMPT);
    assert_eq!(calls[0].handoff_path(), ["root".to_owned()]);
    uuid::Uuid::parse_str(calls[0].deferral_id()).expect("deferral id is a UUID");

    assert_eq!(model.invocations(), 1, "the model is not called again");
    assert_eq!(b.invocations().len(), 0, "the deferred tool does not run");
    assert_eq!(a.invocations().len(), 1);
    assert_eq!(c.invocations().len(), 1);

    assert_eq!(slot.calls(), calls, "the slot records the deferred calls");
    assert_eq!(result.stop_reason, StopReason::Deferred { calls });
}

#[tokio::test]
async fn permission_denied_events_come_first() {
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(MockModel::with_scripts(vec![tool_turn(
            &[
                ("call_a", "a"),
                ("call_b", "b"),
                ("call_c", "c"),
                ("call_d", "d"),
            ],
            None,
        )]))
        .tool_arc_all(&["a", "b", "c", "d"])
        .build();
    let ctx = deferring_ctx().with_deny_rules(vec![DenyRule::tool("d")]);

    let result = collect(&agent, ctx).await;
    let ks = kinds(&result.events);
    let denied = ks
        .iter()
        .position(|k| k == "PermissionDenied(d)")
        .unwrap_or_else(|| panic!("no PermissionDenied(d) in {ks:?}"));
    let first_output = ks
        .iter()
        .position(|k| k.starts_with("ToolOutputItem("))
        .unwrap_or_else(|| panic!("no ToolOutputItem in {ks:?}"));
    assert!(denied < first_output, "event kinds: {ks:?}");
    assert_eq!(ks.last().map(String::as_str), Some("RunDeferred"));
}

#[tokio::test]
async fn deferral_on_the_last_allowed_turn_ends_with_run_deferred() {
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(MockModel::with_scripts(vec![tool_turn(
            &[("call_b", "b")],
            None,
        )]))
        .tool_arc_all(&["b"])
        .build();
    let mut config = RunConfig::new();
    config.max_turns = 1;
    let ctx = deferring_ctx().with_run_config(config);

    let result = collect(&agent, ctx).await;
    let ks = kinds(&result.events);
    assert_eq!(ks.last().map(String::as_str), Some("RunDeferred"));
    assert!(
        !ks.iter().any(|k| k.starts_with("RunFailed")),
        "event kinds: {ks:?}"
    );
}

/// Replaces the args of every `b` call with a value that holds a secret.
struct InjectSecret;

#[async_trait]
impl Hook<()> for InjectSecret {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        match event {
            HookEvent::PreToolUse { tool, .. } if tool == "b" => HookDecision::ReplaceInput {
                value: serde_json::json!({ "token": SECRET }),
            },
            _ => HookDecision::Allow,
        }
    }
}

#[tokio::test]
async fn deferred_event_args_are_redacted() {
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(MockModel::with_scripts(vec![tool_turn(
            &[("call_b", "b")],
            None,
        )]))
        .tool_arc_all(&["b"])
        .hook(InjectSecret)
        .build();
    let ctx = deferring_ctx().with_extra_secrets(vec![SECRET.to_owned()]);
    let slot = ctx.deferral_handle();

    let result = collect(&agent, ctx).await;
    let approval_args = result
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ApprovalRequested { args, .. } => Some(args.clone()),
            _ => None,
        })
        .expect("an ApprovalRequested event");
    assert!(
        !approval_args.to_string().contains(SECRET),
        "{approval_args}"
    );

    let calls = deferred_calls(&result.events);
    assert!(
        !calls[0].args().to_string().contains(SECRET),
        "{}",
        calls[0].args()
    );
    let stop = serde_json::to_string(&result.stop_reason).unwrap();
    assert!(!stop.contains(SECRET), "{stop}");

    assert_eq!(
        slot.calls()[0].args(),
        &serde_json::json!({ "token": SECRET }),
        "the slot keeps the raw effective args"
    );
}

#[tokio::test]
async fn unknown_tool_is_not_deferred() {
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(MockModel::with_scripts(vec![
            tool_turn(&[("call_g", "ghost")], None),
            text_turn("done"),
        ]))
        .build();
    let ctx = RunContext::ephemeral(())
        .with_deferred_tool_calls(true)
        .with_permission_policy(DeferPolicy::everything());

    let result = collect(&agent, ctx).await;
    assert_eq!(
        tool_output_text(&result.events, "call_g"),
        "unknown tool: ghost"
    );
    let ks = kinds(&result.events);
    assert_eq!(
        ks.last().map(String::as_str),
        Some("RunCompleted"),
        "{ks:?}"
    );
    assert_eq!(result.final_output, "done");
}

#[tokio::test]
async fn flag_off_turns_defer_into_permission_denied() {
    let b = MockTool::new("b", serde_json::json!({"ok": "b"}));
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(MockModel::with_scripts(vec![
            tool_turn(&[("call_a", "a"), ("call_b", "b"), ("call_c", "c")], None),
            text_turn("done"),
        ]))
        .tool_arc_all(&["a", "c"])
        .shared_tool(Arc::clone(&b) as Arc<dyn Tool<()>>)
        .build();
    let ctx = RunContext::ephemeral(()).with_permission_policy(DeferPolicy::only_b());

    let result = collect(&agent, ctx).await;
    let reason = result
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::PermissionDenied { tool, reason } if tool == "b" => Some(reason.clone()),
            _ => None,
        })
        .expect("PermissionDenied for b");
    assert!(
        reason.starts_with("approval deferred, but this run cannot defer: "),
        "{reason}"
    );
    assert_eq!(b.invocations().len(), 0);
    let ks = kinds(&result.events);
    assert!(!ks.iter().any(|k| k == "RunDeferred"), "{ks:?}");
    assert_eq!(
        ks.last().map(String::as_str),
        Some("RunCompleted"),
        "{ks:?}"
    );
}

/// Counts `OnRunComplete` events.
struct CountRunComplete(Arc<AtomicUsize>);

#[async_trait]
impl Hook<()> for CountRunComplete {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        if matches!(event, HookEvent::OnRunComplete) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        HookDecision::Allow
    }
}

#[tokio::test]
async fn on_run_complete_fires_once_on_deferral() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut hooks = HookRegistry::<()>::new();
    hooks.push(Arc::new(CountRunComplete(Arc::clone(&count))));
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(MockModel::with_scripts(vec![tool_turn(
            &[("call_b", "b")],
            None,
        )]))
        .tool_arc_all(&["b"])
        .build();
    let ctx = deferring_ctx().with_hooks(hooks);

    let result = collect(&agent, ctx).await;
    assert_eq!(
        kinds(&result.events).last().map(String::as_str),
        Some("RunDeferred")
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn deferral_in_handoff_child_carries_the_path_and_summed_usage() {
    let budgeting = LlmAgent::builder::<()>()
        .name("budgeting")
        .shared_model(MockModel::with_scripts(vec![tool_turn(
            &[("call_b", "b")],
            Some(4),
        )]))
        .tool_arc_all(&["b"])
        .build();
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .shared_model(MockModel::with_scripts(vec![tool_turn(
            &[("t1", "transfer_to_budgeting")],
            Some(3),
        )]))
        .handoff(budgeting)
        .build();

    let result = collect(&triage, deferring_ctx()).await;
    let calls = deferred_calls(&result.events);
    assert_eq!(
        calls[0].handoff_path(),
        ["triage".to_owned(), "budgeting".to_owned()]
    );
    match result.events.last() {
        Some(AgentEvent::RunDeferred { usage, .. }) => assert_eq!(usage.input_tokens, 7),
        other => panic!("expected RunDeferred, got {other:?}"),
    }
}

/// Builder sugar: add one fresh `MockTool` per name.
trait ToolArcAll {
    fn tool_arc_all(self, names: &[&str]) -> Self;
}

impl<M, T, N, Mo, So> ToolArcAll for paigasus_helikon_core::LlmAgentBuilder<(), M, T, N, Mo, So> {
    fn tool_arc_all(self, names: &[&str]) -> Self {
        names.iter().fold(self, |builder, name| {
            builder.shared_tool(
                MockTool::new(name, serde_json::json!({ "ok": name })) as Arc<dyn Tool<()>>
            )
        })
    }
}
