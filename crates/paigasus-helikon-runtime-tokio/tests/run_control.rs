//! run-level control: cancellation, timeout, biased completion, finalize.

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use paigasus_helikon_core::{AgentInput, CancellationToken, RunConfig, RunError, Runner, Session};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{
    run_context_with_cancel, run_context_with_session, run_context_with_session_and_cancel,
    text_agent, CountingSession, MockModel, PendingModel,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_aborts_in_flight_run() {
    let cancel = CancellationToken::new();
    let ctx = run_context_with_cancel(cancel.clone());
    let agent = text_agent(std::sync::Arc::new(PendingModel), Vec::new());

    let res = tokio::time::timeout(Duration::from_secs(5), async {
        let run_fut = TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        );
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (r, _) = tokio::join!(run_fut, canceller);
        r
    })
    .await
    .expect("run must abort within 5s of cancel");

    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_returns_timeout() {
    let agent = text_agent(std::sync::Arc::new(PendingModel), Vec::new());
    let res = tokio::time::timeout(Duration::from_secs(5), async {
        TokioRunner
            .run(
                &agent,
                common::run_context_with_cancel(CancellationToken::new()),
                AgentInput::from_user_text("go"),
                RunConfig::new().with_timeout(Duration::from_millis(50)),
            )
            .await
    })
    .await
    .expect("run must self-timeout within 5s");

    assert!(matches!(res, Err(RunError::Timeout)), "got {res:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prefired_cancel_still_completes_ready_run() {
    // Token already fired before the run starts; because every event is
    // immediately ready, biased stream-first still lets the run drain to
    // completion rather than reporting Cancelled.
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = run_context_with_cancel(cancel);
    let agent = text_agent(MockModel::quick_hi(), Vec::new());

    let res = TokioRunner
        .run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(
        res.is_ok(),
        "ready run must complete despite a fired token: {res:?}"
    );
    assert_eq!(res.unwrap().final_output, "hi");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_runs_on_every_run_exit() {
    // 1. normal
    let session = CountingSession::new();
    let agent = text_agent(MockModel::quick_hi(), Vec::new());
    let _ = TokioRunner
        .run(
            &agent,
            run_context_with_session(session.clone() as std::sync::Arc<dyn Session>),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert_eq!(session.append_count(), 1, "finalize on normal exit");

    // 2. agent failure (empty scripts => model invoke errors => RunFailed)
    let session = CountingSession::new();
    let agent = text_agent(MockModel::with_scripts(vec![]), Vec::new());
    let res = TokioRunner
        .run(
            &agent,
            run_context_with_session(session.clone() as std::sync::Arc<dyn Session>),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(res.is_err(), "agent failure must be Err");
    assert_eq!(session.append_count(), 1, "finalize on failure exit");

    // 3. cancel
    let session = CountingSession::new();
    let cancel = CancellationToken::new();
    let ctx = run_context_with_session_and_cancel(
        session.clone() as std::sync::Arc<dyn Session>,
        cancel.clone(),
    );
    let agent = text_agent(std::sync::Arc::new(PendingModel), Vec::new());
    let cancel_res = tokio::time::timeout(Duration::from_secs(5), async {
        let run_fut = TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        );
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (r, _) = tokio::join!(run_fut, canceller);
        r
    })
    .await
    .expect("cancel within 5s");
    assert!(
        matches!(cancel_res, Err(RunError::Cancelled)),
        "expected cancel path, got {cancel_res:?}"
    );
    assert_eq!(session.append_count(), 1, "finalize on cancel exit");

    // 4. timeout
    let session = CountingSession::new();
    let agent = text_agent(std::sync::Arc::new(PendingModel), Vec::new());
    let timeout_res = tokio::time::timeout(Duration::from_secs(5), async {
        TokioRunner
            .run(
                &agent,
                run_context_with_session(session.clone() as std::sync::Arc<dyn Session>),
                AgentInput::from_user_text("go"),
                RunConfig::new().with_timeout(Duration::from_millis(50)),
            )
            .await
    })
    .await
    .expect("timeout within 5s");
    assert!(
        matches!(timeout_res, Err(RunError::Timeout)),
        "expected timeout path, got {timeout_res:?}"
    );
    assert_eq!(session.append_count(), 1, "finalize on timeout exit");
}
