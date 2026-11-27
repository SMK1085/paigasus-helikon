//! run-level control: cancellation, timeout, biased completion, finalize.

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use paigasus_helikon_core::{
    AgentInput, CancellationToken, Hook, RunConfig, RunError, Runner, Session,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{
    run_context_with_cancel, run_context_with_cancel_and_hooks, run_context_with_session,
    run_context_with_session_and_cancel, text_agent, CancelOnRunCompleteHook, CountingSession,
    MockModel, PendingModel,
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
    // Token already fired before the run starts; the run drains to completion,
    // a RunCompleted terminal is observed, and the cancel cannot override it
    // (SMA-421: a genuine terminal wins over any cancel/timeout).
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_then_late_cancel_reports_completed() {
    // A suspending OnRunComplete hook cancels the run AFTER the terminal event
    // already went out. The completed run must be reported as Ok — the late
    // cancel must not override a genuine terminal. (SMA-421)
    let cancel = CancellationToken::new();
    let ctx = run_context_with_cancel_and_hooks(
        cancel,
        vec![std::sync::Arc::new(CancelOnRunCompleteHook) as std::sync::Arc<dyn Hook<()>>],
    );
    let agent = text_agent(MockModel::quick_hi(), Vec::new());

    let res = tokio::time::timeout(
        Duration::from_secs(5),
        TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("run must settle within 5s");

    assert!(res.is_ok(), "terminal must win over a late cancel: {res:?}");
    assert_eq!(res.unwrap().final_output, "hi");
}

/// A model like a real provider: it streams some events, waits, and ends its
/// stream with no `Finish` when the token fires.
struct EndsOnCancel {
    events: Vec<paigasus_helikon_core::ModelEvent>,
}

#[async_trait::async_trait]
impl paigasus_helikon_core::Model for EndsOnCancel {
    async fn invoke(
        &self,
        _request: paigasus_helikon_core::ModelRequest,
        cancel: CancellationToken,
    ) -> Result<
        futures_core::stream::BoxStream<
            'static,
            Result<paigasus_helikon_core::ModelEvent, paigasus_helikon_core::ModelError>,
        >,
        paigasus_helikon_core::ModelError,
    > {
        let events = self.events.clone();
        Ok(Box::pin(async_stream::stream! {
            for ev in events {
                yield Ok(ev);
            }
            cancel.cancelled().await;
        }))
    }

    fn capabilities(&self) -> paigasus_helikon_core::ModelCapabilities {
        paigasus_helikon_core::ModelCapabilities::default()
    }
}

/// A tool that counts its calls.
struct CountingTool(std::sync::atomic::AtomicUsize);

#[async_trait::async_trait]
impl paigasus_helikon_core::Tool<()> for CountingTool {
    fn name(&self) -> &str {
        "search"
    }
    fn description(&self) -> &str {
        "counting tool"
    }
    fn schema(&self) -> &serde_json::Value {
        static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        SCHEMA.get_or_init(|| serde_json::json!({"type": "object"}))
    }
    async fn invoke(
        &self,
        _ctx: &paigasus_helikon_core::ToolContext<()>,
        _args: serde_json::Value,
    ) -> Result<paigasus_helikon_core::ToolOutput, paigasus_helikon_core::ToolError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(paigasus_helikon_core::ToolOutput::new(
            serde_json::json!({}),
        ))
    }
}

async fn cancel_after_20ms(
    events: Vec<paigasus_helikon_core::ModelEvent>,
) -> (Result<paigasus_helikon_core::RunResult, RunError>, usize) {
    let tool = std::sync::Arc::new(CountingTool(Default::default()));
    let cancel = CancellationToken::new();
    let ctx = run_context_with_cancel(cancel.clone());
    let agent = text_agent(
        std::sync::Arc::new(EndsOnCancel { events }),
        vec![tool.clone() as std::sync::Arc<dyn paigasus_helikon_core::Tool<()>>],
    );
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
    .expect("run must end within 5s of cancel");
    (res, tool.0.load(std::sync::atomic::Ordering::SeqCst))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_a_tool_call_stream_is_cancelled() {
    let (res, calls) = cancel_after_20ms(vec![paigasus_helikon_core::ModelEvent::ToolCallDelta {
        call_id: "c1".into(),
        name: Some("search".into()),
        args_delta: "{}".into(),
    }])
    .await;
    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
    assert_eq!(calls, 0, "no tool may run");
}
