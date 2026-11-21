//! SMA-827 — a provider-data-only message is invisible to text consumers.
//!
//! Two scripts differ only by `ModelEvent::ProviderData` events in a tool-only
//! turn. Every text consumer must give the same result for both.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::{DeferB, MockModel, MockTool};
use futures_util::StreamExt as _;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, ContentPart, FinishReason, HookRegistry,
    Item, LlmAgent, MemorySession, ModelEvent, RunContext, RunResultStreaming, SequentialAgent,
    Session, SessionEvent, SessionRecorder, StopReason, Tool, TracerHandle,
};

#[derive(Debug, PartialEq, serde::Deserialize, schemars::JsonSchema)]
struct Ok2 {
    ok: bool,
}

fn text(t: &str) -> ModelEvent {
    ModelEvent::TokenDelta { text: t.to_owned() }
}

fn call(id: &str, tool: &str) -> ModelEvent {
    ModelEvent::ToolCallDelta {
        call_id: id.to_owned(),
        name: Some(tool.to_owned()),
        args_delta: "{}".to_owned(),
    }
}

fn sig(call_id: &str) -> ModelEvent {
    ModelEvent::ProviderData {
        provider: "gemini".to_owned(),
        call_id: Some(call_id.to_owned()),
        data: serde_json::json!({ "thoughtSignature": "SIG_A" }),
    }
}

fn finish(reason: FinishReason) -> ModelEvent {
    ModelEvent::Finish { reason }
}

/// Turn 1: text plus a call. Turn 2: a call only (plus provider data in
/// variant B). Turn 3: the final text.
fn scripts(with_data: bool, last: &str) -> Vec<Vec<ModelEvent>> {
    let mut turn2 = vec![call("c2", "b")];
    if with_data {
        turn2.push(sig("c2"));
    }
    turn2.push(finish(FinishReason::ToolCalls));
    vec![
        vec![
            text("thinking aloud"),
            call("c1", "a"),
            finish(FinishReason::ToolCalls),
        ],
        turn2,
        vec![text(last), finish(FinishReason::Stop)],
    ]
}

fn tools() -> (Arc<MockTool>, Arc<MockTool>) {
    (
        MockTool::new("a", serde_json::json!({ "ok": true })),
        MockTool::new("b", serde_json::json!({ "ok": true })),
    )
}

#[tokio::test]
async fn collect_output_identical_with_and_without_provider_data() {
    let mut outputs = Vec::new();
    for with_data in [false, true] {
        let (a, b) = tools();
        let agent = LlmAgent::builder::<()>()
            .name("root")
            .shared_model(MockModel::with_scripts(scripts(with_data, "done")))
            .shared_tool(a as Arc<dyn Tool<()>>)
            .shared_tool(b as Arc<dyn Tool<()>>)
            .build();
        let stream = agent
            .run(RunContext::ephemeral(()), AgentInput::from_user_text("go"))
            .await
            .unwrap();
        outputs.push(
            RunResultStreaming::new(stream)
                .collect()
                .await
                .unwrap()
                .final_output,
        );
    }
    assert_eq!(outputs[0], "done");
    assert_eq!(outputs[0], outputs[1]);
}

#[tokio::test]
async fn collect_typed_identical_with_and_without_provider_data() {
    let mut outputs = Vec::new();
    for with_data in [false, true] {
        let (a, b) = tools();
        // A structured run asks for the JSON in one more turn after a text turn.
        let mut turns = scripts(with_data, "free text");
        turns.push(vec![text("{\"ok\":true}"), finish(FinishReason::Stop)]);
        let agent = LlmAgent::builder::<()>()
            .name("root")
            .shared_model(MockModel::with_scripts(turns))
            .shared_tool(a as Arc<dyn Tool<()>>)
            .shared_tool(b as Arc<dyn Tool<()>>)
            .output_type::<Ok2>()
            .build();
        let stream = agent
            .run(RunContext::ephemeral(()), AgentInput::from_user_text("go"))
            .await
            .unwrap();
        let typed = RunResultStreaming::new(stream)
            .collect_typed::<Ok2>()
            .await
            .unwrap();
        outputs.push(typed.final_output);
    }
    assert_eq!(outputs[0], Ok2 { ok: true });
    assert_eq!(outputs[0], outputs[1]);
}

#[tokio::test]
async fn deferred_after_provider_data_only_turn_keeps_text() {
    let mut outputs = Vec::new();
    for with_data in [false, true] {
        let (a, b) = tools();
        let agent = LlmAgent::builder::<()>()
            .name("root")
            .shared_model(MockModel::with_scripts(scripts(with_data, "unused")))
            .shared_tool(a as Arc<dyn Tool<()>>)
            .shared_tool(b as Arc<dyn Tool<()>>)
            .build();
        let ctx = RunContext::ephemeral(())
            .with_permission_policy(Arc::new(DeferB))
            .with_deferred_tool_calls(true);
        let stream = agent
            .run(ctx, AgentInput::from_user_text("go"))
            .await
            .unwrap();
        let result = RunResultStreaming::new(stream).collect().await.unwrap();
        assert!(
            matches!(result.stop_reason, StopReason::Deferred { .. }),
            "{:?}",
            result.stop_reason
        );
        outputs.push(result.final_output);
    }
    assert_eq!(outputs[0], "thinking aloud");
    assert_eq!(outputs[0], outputs[1]);
}

#[tokio::test]
async fn workflow_state_identical_with_and_without_provider_data() {
    let mut states = Vec::new();
    for with_data in [false, true] {
        let (a, b) = tools();
        let step = LlmAgent::builder::<()>()
            .name("step")
            .shared_model(MockModel::with_scripts(scripts(with_data, "done")))
            .shared_tool(a as Arc<dyn Tool<()>>)
            .shared_tool(b as Arc<dyn Tool<()>>)
            .build();
        let seq = SequentialAgent::new("seq", "one step").then(step);
        let ctx = RunContext::ephemeral(());
        let state = ctx.state().clone();
        let stream = seq
            .run(ctx, AgentInput::from_user_text("go"))
            .await
            .unwrap();
        RunResultStreaming::new(stream).collect().await.unwrap();
        states.push(state.get("step"));
    }
    assert_eq!(states[0], Some(serde_json::json!("done")));
    assert_eq!(states[0], states[1]);
}

#[tokio::test]
async fn session_records_provider_data_before_tool_call() {
    let (a, b) = tools();
    let agent = LlmAgent::builder::<()>()
        .name("root")
        .shared_model(MockModel::with_scripts(scripts(true, "done")))
        .shared_tool(a as Arc<dyn Tool<()>>)
        .shared_tool(b as Arc<dyn Tool<()>>)
        .build();
    let session = Arc::new(MemorySession::new());
    let ctx = RunContext::new(
        Arc::new(()),
        Arc::clone(&session) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );
    let mut recorder = SessionRecorder::new("root");
    let input = AgentInput::from_user_text("go");
    recorder.record_input(&input.messages);
    let events: Vec<AgentEvent> = agent.run(ctx, input).await.unwrap().collect().await;
    for ev in &events {
        recorder.observe(ev);
    }
    session.append(&recorder.drain()).await.unwrap();

    let log = session.events(None).await.unwrap();
    let is_data =
        |c: &[ContentPart]| c.len() == 1 && matches!(c[0], ContentPart::ProviderData { .. });
    let data_at = log
        .iter()
        .position(
            |e| matches!(e, SessionEvent::AssistantMessage { content, .. } if is_data(content)),
        )
        .expect("a provider-data message is in the log");
    let call_at = log
        .iter()
        .position(|e| matches!(e, SessionEvent::ToolCalled { call_id, .. } if call_id == "c2"))
        .expect("the c2 call is in the log");
    assert!(data_at < call_at, "{log:?}");

    let snapshot = session.snapshot().await.unwrap();
    let msg_at = snapshot
        .messages
        .iter()
        .position(|i| matches!(i, Item::AssistantMessage { content, .. } if is_data(content)))
        .expect("a provider-data message is in the projection");
    let tool_at = snapshot
        .messages
        .iter()
        .position(|i| matches!(i, Item::ToolCall { call_id, .. } if call_id == "c2"))
        .expect("the c2 call is in the projection");
    assert!(msg_at < tool_at, "{:?}", snapshot.messages);
}
