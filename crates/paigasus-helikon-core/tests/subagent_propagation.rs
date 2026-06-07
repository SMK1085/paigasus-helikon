//! SMA-326 (Task 12a): permission config propagates into agent-as-tool
//! sub-runs, and workflow agents fire `OnSubagentStop` per sub-agent.

#[path = "common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{MockAgent, MockModel, MockTool};
use paigasus_helikon_core::{
    Agent, AgentAsTool, AgentEvent, AgentInput, CancellationToken, FinishReason, Hook,
    HookDecision, HookEvent, HookRegistry, LlmAgent, MemorySession, ModelEvent, PermissionMode,
    RunContext, Session, Tool, TracerHandle,
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

    let run_ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
    .with_permission_mode(PermissionMode::Plan);

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
    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()) as Arc<dyn Session>,
        reg,
        TracerHandle::default(),
        CancellationToken::new(),
    );

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
