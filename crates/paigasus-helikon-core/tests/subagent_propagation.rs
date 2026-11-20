//! SMA-326 (Task 12a): permission config propagates into agent-as-tool
//! sub-runs, and workflow agents fire `OnSubagentStop` per sub-agent.

#[path = "common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{DeferB, MockAgent, MockModel, MockTool, RequestLog};
use paigasus_helikon_core::{
    Agent, AgentAsTool, AgentEvent, AgentInput, AllowRule, ContentPart, FinishReason, GuardRule,
    Hook, HookDecision, HookEvent, HookRegistry, Item, LlmAgent, ModelEvent, PermissionMode,
    RunContext, Tool,
};

/// Turn 1: call `tool_name` with `{}`. Turn 2: stop with `"done"`. Two scripts so
/// the inner loop resumes after the tool result (or the permission denial) lands.
fn call_then_stop(tool_name: &str) -> Vec<Vec<ModelEvent>> {
    vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "c1".to_owned(),
                name: Some(tool_name.to_owned()),
                args_delta: "{}".to_owned(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            ModelEvent::TokenDelta {
                text: "done".to_owned(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]
}

/// Test A — `Plan` propagates from the `ToolContext` into the agent-as-tool
/// sub-run. The inner agent scripts a call to a default-`SideEffect` `MockTool`;
/// invoking the wrapper through a `ToolContext` carrying `Plan` must deny that
/// inner tool, proving the parent's permission mode crossed the boundary.
///
/// We drive the wrapper directly via `RunContext::to_tool_context()` (the idiom
/// the existing `agent_as_tool` tests use) rather than nesting it under an outer
/// LlmAgent loop: an outer loop would gate the wrapper's own (`SideEffect`) call
/// under `Plan` and the inner agent would never run, making the assertion a
/// false positive. Driving the tool directly removes the outer gate so the
/// inner-tool denial is unambiguously the propagated `Plan` at work.
#[tokio::test]
async fn plan_mode_propagates_into_agent_as_tool_sub_run() {
    let inner_tool = MockTool::new("secret_writer", serde_json::json!({"ok": true}));
    let inner = LlmAgent::builder::<()>()
        .name("inner")
        .description("calls a side-effecting tool")
        .shared_model(MockModel::with_scripts(call_then_stop("secret_writer")))
        .shared_tool(Arc::clone(&inner_tool) as Arc<dyn Tool<()>>)
        .build();

    let wrapper = AgentAsTool::new(inner).with_name("inner");

    let run_ctx: RunContext<()> =
        RunContext::ephemeral(()).with_permission_mode(PermissionMode::Plan);

    let tc = run_ctx.to_tool_context();
    assert_eq!(
        tc.permission_mode(),
        PermissionMode::Plan,
        "tool context must carry Plan from the parent run context"
    );

    let _ = wrapper
        .invoke(&tc, serde_json::json!({ "input": "go" }))
        .await
        .expect("sub-run completes (inner loop resumes after the denial)");

    assert_eq!(
        inner_tool.invocations().len(),
        0,
        "Plan propagated into the sub-run and denied the side-effecting inner tool"
    );
}

/// Records every `OnSubagentStop` agent name into a shared vec.
struct StopRecorder(Arc<Mutex<Vec<String>>>);

#[async_trait]
impl Hook<()> for StopRecorder {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        if let HookEvent::OnSubagentStop { agent } = event {
            self.0.lock().unwrap().push(agent.clone());
        }
        HookDecision::Allow
    }
}

/// Test B — a `SequentialAgent` fires `OnSubagentStop` for each child after its
/// stream drains. Two trivial `MockAgent` children "a" and "b"; the run-level
/// `HookRegistry` carries a recorder that must observe both names.
#[tokio::test]
async fn sequential_agent_fires_on_subagent_stop_per_child() {
    use paigasus_helikon_core::SequentialAgent;

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let a = MockAgent::<()>::new("a", |_| Vec::new());
    let b = MockAgent::<()>::new("b", |_| Vec::new());
    let workflow = SequentialAgent::<()>::new("seq", "two steps")
        .then(a)
        .then(b);

    let mut reg = HookRegistry::<()>::new();
    reg.push(Arc::new(StopRecorder(seen.clone())));
    let ctx: RunContext<()> = RunContext::ephemeral(()).with_hooks(reg);

    let mut stream = workflow
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .expect("workflow run starts");
    // Drain the outer stream so every sub-agent runs to completion.
    use futures_util::StreamExt as _;
    while let Some(ev) = stream.next().await {
        let _ = ev as AgentEvent;
    }

    let names = seen.lock().unwrap().clone();
    assert!(
        names.contains(&"a".to_owned()),
        "OnSubagentStop must fire for child `a`; saw {names:?}"
    );
    assert!(
        names.contains(&"b".to_owned()),
        "OnSubagentStop must fire for child `b`; saw {names:?}"
    );
}

/// Test C — `AgentAsTool::invoke` fires `OnSubagentStop` against the *parent*
/// run's hook registry after the sub-run completes. The sub-run is isolated
/// (empty hooks), but the carrier projected via `to_tool_context` must deliver
/// the stop event to the parent's `StopRecorder` with the wrapped agent's name.
#[tokio::test]
async fn agent_as_tool_fires_on_subagent_stop() {
    use paigasus_helikon_core::{AgentAsTool, HookRegistry, RunContext, Tool};
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let mut reg = HookRegistry::<()>::new();
    reg.push(std::sync::Arc::new(StopRecorder(seen.clone())));
    let parent = RunContext::ephemeral(()).with_hooks(reg);

    let inner = common::MockAgent::new("inner", |_| Vec::new());
    let wrapper = AgentAsTool::new(inner).with_name("inner_tool");
    let tc = parent.to_tool_context();
    let _ = wrapper
        .invoke(&tc, serde_json::json!({"input": "go"}))
        .await
        .unwrap();

    assert!(
        seen.lock().unwrap().iter().any(|n| n == "inner"),
        "agent-as-tool sub-run fires OnSubagentStop with the inner agent's name"
    );
}

/// Test D — SMA-414: guard rules, `default_guards`, `redact_output`, and
/// `extra_secrets` all propagate from the parent `ToolContext` into the
/// agent-as-tool sub-run's `RunContext`.
///
/// Strategy (lighter-probe): the wrapped agent is a `MockAgent` whose behavior
/// closure captures a shared `Arc<Mutex<...>>` and records the four values it
/// observes on the `RunContext` it receives. `AgentAsTool::invoke` is driven
/// directly via `RunContext::to_tool_context()` (the same idiom used in
/// `plan_mode_propagates_into_agent_as_tool_sub_run`). After the invoke
/// completes the recorded values are asserted against the parent's config.
#[tokio::test]
async fn guard_and_redaction_config_propagates_into_agent_as_tool_sub_run() {
    /// Snapshot of the four SMA-414 fields as seen by the inner agent.
    #[derive(Default)]
    struct Observed {
        guard_rules_len: usize,
        default_guards: bool,
        redact_output: bool,
        extra_secrets: Vec<String>,
    }

    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Observed::default()));
    let obs_clone = Arc::clone(&observed);

    let inner = MockAgent::<()>::new("probe", move |ctx| {
        let mut obs = obs_clone.lock().unwrap();
        obs.guard_rules_len = ctx.guard_rules().len();
        obs.default_guards = ctx.default_guards();
        obs.redact_output = ctx.redact_output();
        obs.extra_secrets = ctx.extra_secrets().to_vec();
        Vec::new()
    });

    let wrapper = AgentAsTool::new(inner).with_name("probe");

    // Parent context: two custom guard rules, extra secret, default_guards off,
    // redact_output off.
    let parent_guard_rules = GuardRule::destructive_defaults(); // non-empty slice
    let parent_guard_rules_len = parent_guard_rules.len();
    let run_ctx: RunContext<()> = RunContext::ephemeral(())
        .with_guard_rules(parent_guard_rules)
        .with_extra_secrets(vec!["zzsecretvalue".to_owned()])
        .without_default_guards()
        .without_output_redaction();

    let tc = run_ctx.to_tool_context();
    let _ = wrapper
        .invoke(&tc, serde_json::json!({ "input": "probe" }))
        .await
        .expect("invoke ok");

    let obs = observed.lock().unwrap();
    assert_eq!(
        obs.guard_rules_len, parent_guard_rules_len,
        "guard_rules must propagate: expected {parent_guard_rules_len} rules, got {}",
        obs.guard_rules_len
    );
    assert!(
        !obs.default_guards,
        "default_guards=false must propagate into the sub-run"
    );
    assert!(
        !obs.redact_output,
        "redact_output=false must propagate into the sub-run"
    );
    assert!(
        obs.extra_secrets.contains(&"zzsecretvalue".to_owned()),
        "extra_secrets must propagate; got {:?}",
        obs.extra_secrets
    );
}

/// Test E — SMA-415: `allow_rules` *and* `PermissionMode::DontAsk` propagate
/// from the parent `ToolContext` into the agent-as-tool sub-run's `RunContext`.
///
/// This is the fourth and final permission-field copy site (`handoff_child`,
/// `subagent_child`, and `to_tool_context` are the other three). Missing it is a
/// *fail-closed* propagation bug: under `DontAsk` every call is denied unless an allow rule
/// matches, so an agent-as-tool sub-run that lost the parent's `allow_rules`
/// would silently deny everything — and a unit-level inheritance test can't catch
/// the agent-as-tool rebuild path, only this integration test can.
///
/// Strategy mirrors Test D's lighter-probe: the wrapped `MockAgent` records
/// `allow_rules().len()` and `permission_mode()` off the `RunContext` it receives.
/// The parent carries one `AllowRule::tool("probe")` and `DontAsk`.
#[tokio::test]
async fn allow_rules_and_dont_ask_propagate_into_agent_as_tool_sub_run() {
    /// Snapshot of the two SMA-415 fields as seen by the inner agent.
    #[derive(Default)]
    struct Observed {
        allow_rules_len: usize,
        permission_mode: Option<PermissionMode>,
    }

    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Observed::default()));
    let obs_clone = Arc::clone(&observed);

    let inner = MockAgent::<()>::new("probe", move |ctx| {
        let mut obs = obs_clone.lock().unwrap();
        obs.allow_rules_len = ctx.allow_rules().len();
        obs.permission_mode = Some(ctx.permission_mode());
        Vec::new()
    });

    let wrapper = AgentAsTool::new(inner).with_name("probe");

    // Parent context: one allow rule scoped to the inner agent's tool name, and
    // `DontAsk` (terminal/tighten-only — set once, sticks).
    let run_ctx: RunContext<()> = RunContext::ephemeral(())
        .with_allow_rules(vec![AllowRule::tool("probe")])
        .with_permission_mode(PermissionMode::DontAsk);

    let tc = run_ctx.to_tool_context();
    assert_eq!(
        tc.permission_mode(),
        PermissionMode::DontAsk,
        "tool context must carry DontAsk from the parent run context"
    );

    let _ = wrapper
        .invoke(&tc, serde_json::json!({ "input": "probe" }))
        .await
        .expect("invoke ok");

    let obs = observed.lock().unwrap();
    assert_eq!(
        obs.allow_rules_len, 1,
        "allow_rules must propagate into the sub-run: expected 1, got {}",
        obs.allow_rules_len
    );
    assert_eq!(
        obs.permission_mode,
        Some(PermissionMode::DontAsk),
        "PermissionMode::DontAsk must propagate into the sub-run; got {:?}",
        obs.permission_mode
    );
}

/// A *failed* agent-as-tool sub-run still fires `OnSubagentStop`, matching the
/// handoff and workflow paths (which report failed sub-runs as stopped).
#[tokio::test]
async fn agent_as_tool_fires_on_subagent_stop_on_failure() {
    use paigasus_helikon_core::{AgentAsTool, HookRegistry, RunContext, Tool};
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let mut reg = HookRegistry::<()>::new();
    reg.push(std::sync::Arc::new(StopRecorder(seen.clone())));
    let parent = RunContext::ephemeral(()).with_hooks(reg);

    // The inner agent fails: its stream yields RunFailed, so collect() errors.
    let inner = common::MockAgent::new("inner", |_| {
        vec![AgentEvent::RunFailed {
            error: "boom".to_owned(),
        }]
    });
    let wrapper = AgentAsTool::new(inner).with_name("inner_tool");
    let tc = parent.to_tool_context();
    let res = wrapper
        .invoke(&tc, serde_json::json!({"input": "go"}))
        .await;

    assert!(res.is_err(), "a failed sub-run surfaces as a tool error");
    assert!(
        seen.lock().unwrap().iter().any(|n| n == "inner"),
        "OnSubagentStop fires even when the agent-as-tool sub-run fails"
    );
}

// ── SMA-803: a sub-run cannot defer a tool call ─────────────────────────────

/// The text of each `ToolResult` item in `items`.
fn tool_result_texts(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::ToolResult { content, .. } => Some(
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
        .collect()
}

async fn drain(agent: &impl Agent<()>, ctx: RunContext<()>) -> Vec<AgentEvent> {
    use futures_util::StreamExt as _;
    agent
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .expect("run starts")
        .collect()
        .await
}

const CANNOT_DEFER: &str = "approval deferred, but this run cannot defer: ";

/// The fourth `RunContext` copy site: `AgentAsTool` builds its sub-context with
/// `RunContext::new`, so the sub-run does not inherit the deferral flag. A
/// deferral there is a denial, and the parent run completes.
#[tokio::test]
async fn deferral_is_denied_inside_agent_as_tool_sub_run() {
    let inner_model = RequestLog::with_scripts(call_then_stop("b"));
    let inner_b = MockTool::new("b", serde_json::json!({"ok": true}));
    let inner = LlmAgent::builder::<()>()
        .name("inner")
        .shared_model(Arc::clone(&inner_model))
        .shared_tool(Arc::clone(&inner_b) as Arc<dyn Tool<()>>)
        .build();
    let wrapper = AgentAsTool::new(inner).with_name("inner");

    let mut parent_script = vec![vec![
        ModelEvent::ToolCallDelta {
            call_id: "p1".to_owned(),
            name: Some("inner".to_owned()),
            args_delta: r#"{"input":"go"}"#.to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]];
    parent_script.push(call_then_stop("unused").remove(1));
    let parent = LlmAgent::builder::<()>()
        .name("parent")
        .shared_model(MockModel::with_scripts(parent_script))
        .tool(wrapper)
        .build();
    let ctx = RunContext::ephemeral(())
        .with_deferred_tool_calls(true)
        .with_permission_policy(Arc::new(DeferB));

    let events = drain(&parent, ctx).await;

    let requests = inner_model.requests();
    assert_eq!(
        requests.len(),
        2,
        "the inner loop continues after the denial"
    );
    let texts = tool_result_texts(&requests[1].messages);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(
        texts[0].starts_with(&format!("permission denied: {CANNOT_DEFER}")),
        "{texts:?}"
    );
    assert_eq!(inner_b.invocations().len(), 0);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::RunDeferred { .. })),
        "{events:?}"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::RunCompleted { .. })),
        "{events:?}"
    );
}

/// A workflow child (`subagent_child`) does not inherit the deferral flag.
#[tokio::test]
async fn deferral_is_denied_inside_a_workflow_child() {
    use paigasus_helikon_core::SequentialAgent;

    let b = MockTool::new("b", serde_json::json!({"ok": true}));
    let child = LlmAgent::builder::<()>()
        .name("child")
        .shared_model(MockModel::with_scripts(call_then_stop("b")))
        .shared_tool(Arc::clone(&b) as Arc<dyn Tool<()>>)
        .build();
    let workflow = SequentialAgent::<()>::new("seq", "one step").then(child);
    let ctx = RunContext::ephemeral(())
        .with_deferred_tool_calls(true)
        .with_permission_policy(Arc::new(DeferB));

    let events = drain(&workflow, ctx).await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::PermissionDenied { tool, reason }
                if tool == "b" && reason.starts_with(CANNOT_DEFER)
        )),
        "{events:?}"
    );
    assert_eq!(b.invocations().len(), 0);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::RunDeferred { .. })),
        "{events:?}"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::RunCompleted { .. })),
        "{events:?}"
    );
}

/// Test F — SMA-804: the turn checkpoint is **never** carried into an
/// agent-as-tool sub-run. `ToolContext` has no checkpoint carrier and the
/// `agent_as_tool::invoke` rebuild must not add one, so a host checkpoint sees
/// only its own run's boundaries. Probe: the inner `MockAgent` records whether
/// the `RunContext` it receives has no checkpoint.
#[tokio::test]
async fn agent_as_tool_sub_context_has_no_turn_checkpoint() {
    use paigasus_helikon_core::{CheckpointDecision, TurnBoundary, TurnCheckpoint};

    struct Noop;

    #[async_trait]
    impl TurnCheckpoint<()> for Noop {
        async fn at_boundary(&self, _: &RunContext<()>, _: &TurnBoundary) -> CheckpointDecision {
            CheckpointDecision::Continue
        }
    }

    let observed: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let obs = Arc::clone(&observed);
    let inner = MockAgent::<()>::new("probe", move |ctx| {
        *obs.lock().unwrap() = Some(ctx.turn_checkpoint().is_none());
        Vec::new()
    });
    let wrapper = AgentAsTool::new(inner).with_name("probe");

    let run_ctx: RunContext<()> =
        RunContext::ephemeral(()).with_turn_checkpoint(Arc::new(Noop) as Arc<_>);
    assert!(run_ctx.turn_checkpoint().is_some(), "parent carries one");

    let tc = run_ctx.to_tool_context();
    let _ = wrapper
        .invoke(&tc, serde_json::json!({ "input": "probe" }))
        .await
        .expect("invoke ok");

    assert_eq!(
        *observed.lock().unwrap(),
        Some(true),
        "the sub-run context must not carry the parent's turn checkpoint"
    );
}
