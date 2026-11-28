//! A cancelled model turn ends as `Cancelled`, never as a completed run (SMA-869).

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use futures_util::StreamExt;
use paigasus_helikon_core::{
    project, AgentEvent, AgentInput, CancellationToken, ContentPart, FinishReason, Item,
    MemorySession, ModelError, ModelEvent, RunConfig, RunError, Runner, Session, StopReason,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{
    run_context_with_cancel, run_context_with_session_and_cancel, text_agent, CancelScriptModel,
    Step,
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
