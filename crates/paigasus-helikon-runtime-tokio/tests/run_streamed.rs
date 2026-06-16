//! run_streamed: event ordering, concurrency, terminal-on-cancel, finalize.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::StreamExt as _;
use paigasus_helikon_core::{
    AgentEvent, AgentInput, CancellationToken, FinishReason, Hook, ModelEvent, RunConfig, Runner,
    Session, Tool,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{
    noop_run_context, run_context_with_cancel, run_context_with_cancel_and_hooks,
    run_context_with_session, run_context_with_session_and_cancel, text_agent,
    CancelOnRunCompleteHook, CountingSession, MockModel, MockToolBarrier, PendingModel,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_event_order() {
    let agent = text_agent(MockModel::quick_hi(), Vec::new());
    let rs = TokioRunner
        .run_streamed(
            &agent,
            noop_run_context(),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("stream starts");

    let events: Vec<AgentEvent> = rs.events.collect().await;

    assert!(
        matches!(events.first(), Some(AgentEvent::RunStarted { .. })),
        "first must be RunStarted: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::RunCompleted { .. })),
        "last must be RunCompleted: {events:?}"
    );
    let msg = events
        .iter()
        .position(|e| matches!(e, AgentEvent::MessageOutput { .. }))
        .expect("a MessageOutput");
    let done = events
        .iter()
        .position(|e| matches!(e, AgentEvent::RunCompleted { .. }))
        .unwrap();
    assert!(msg < done, "semantic item must precede terminal");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_tools_run_concurrently() {
    let barrier = Arc::new(tokio::sync::Barrier::new(5));
    let tools: Vec<Arc<dyn Tool<()>>> = (1..=5)
        .map(|i| MockToolBarrier::new(&format!("t{i}"), barrier.clone()) as Arc<dyn Tool<()>>)
        .collect();

    let mut first = Vec::new();
    for i in 1..=5 {
        first.push(ModelEvent::ToolCallDelta {
            call_id: i.to_string(),
            name: Some(format!("t{i}")),
            args_delta: "{}".into(),
        });
    }
    first.push(ModelEvent::Finish {
        reason: FinishReason::ToolCalls,
    });
    let model = MockModel::with_scripts(vec![
        first,
        vec![
            ModelEvent::TokenDelta { text: "ok".into() },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);
    let agent = text_agent(model, tools);

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        TokioRunner.run(
            &agent,
            noop_run_context(),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("tools ran serially (barrier deadlock)")
    .expect("run ok");

    assert!(matches!(
        result.events.last(),
        Some(AgentEvent::RunCompleted { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_cancel_emits_terminal_runfailed() {
    let cancel = CancellationToken::new();
    let ctx = run_context_with_cancel(cancel.clone());
    let agent = text_agent(Arc::new(PendingModel), Vec::new());

    let rs = TokioRunner
        .run_streamed(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("stream starts");

    let events = tokio::time::timeout(Duration::from_secs(5), async {
        let mut s = rs.events;
        let drain = async {
            let mut evs = Vec::new();
            while let Some(ev) = s.next().await {
                evs.push(ev);
            }
            evs
        };
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (evs, _) = tokio::join!(drain, canceller);
        evs
    })
    .await
    .expect("stream must end within 5s of cancel");

    assert!(
        matches!(events.last(), Some(AgentEvent::RunFailed { error }) if error == "run cancelled"),
        "last event must be RunFailed(run cancelled): {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_runs_on_streamed_exits() {
    // normal
    let session = CountingSession::new();
    let agent = text_agent(MockModel::quick_hi(), Vec::new());
    let rs = TokioRunner
        .run_streamed(
            &agent,
            run_context_with_session(session.clone() as Arc<dyn Session>),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("starts");
    let _drained: Vec<AgentEvent> = rs.events.collect().await;
    assert_eq!(
        session.append_count(),
        1,
        "finalize on normal streamed exit"
    );

    // agent failure
    let session = CountingSession::new();
    let agent = text_agent(MockModel::with_scripts(vec![]), Vec::new());
    let rs = TokioRunner
        .run_streamed(
            &agent,
            run_context_with_session(session.clone() as Arc<dyn Session>),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("starts");
    let _drained: Vec<AgentEvent> = rs.events.collect().await;
    assert_eq!(
        session.append_count(),
        1,
        "finalize on failed streamed exit"
    );

    // cancel — finalize must still run on the Cancelled path, not only Completed.
    let session = CountingSession::new();
    let cancel = CancellationToken::new();
    let agent = text_agent(Arc::new(PendingModel), Vec::new());
    let rs = TokioRunner
        .run_streamed(
            &agent,
            run_context_with_session_and_cancel(
                session.clone() as Arc<dyn Session>,
                cancel.clone(),
            ),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("starts");
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut s = rs.events;
        let drain = async { while s.next().await.is_some() {} };
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        tokio::join!(drain, canceller);
    })
    .await
    .expect("cancelled stream must end within 5s");
    assert_eq!(
        session.append_count(),
        1,
        "finalize on cancelled streamed exit"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_runs_even_if_consumer_stops_at_terminal() {
    // A consumer that drops the stream the moment it sees the terminal event
    // must still have triggered finalize — it runs before the terminal is
    // exposed, not on a later poll the consumer never makes.
    let session = CountingSession::new();
    let agent = text_agent(MockModel::quick_hi(), Vec::new());
    let rs = TokioRunner
        .run_streamed(
            &agent,
            run_context_with_session(session.clone() as Arc<dyn Session>),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("starts");

    let mut s = rs.events;
    while let Some(ev) = s.next().await {
        if matches!(
            ev,
            AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. }
        ) {
            break; // stop at the terminal and drop the stream below
        }
    }
    drop(s);

    assert_eq!(
        session.append_count(),
        1,
        "finalize must run before the terminal event is exposed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_then_late_cancel_no_synthetic_terminal() {
    // Same window via run_streamed: the real terminal already went out, then a
    // late cancel fires during the OnRunComplete hook. The stream must NOT append
    // a second, synthetic RunFailed. (SMA-421)
    let cancel = CancellationToken::new();
    let ctx = run_context_with_cancel_and_hooks(
        cancel,
        vec![Arc::new(CancelOnRunCompleteHook) as Arc<dyn Hook<()>>],
    );
    let agent = text_agent(MockModel::quick_hi(), Vec::new());

    let rs = TokioRunner
        .run_streamed(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("stream starts");

    let events: Vec<AgentEvent> =
        tokio::time::timeout(Duration::from_secs(5), rs.events.collect::<Vec<_>>())
            .await
            .expect("stream must end within 5s");

    let terminals = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. }
            )
        })
        .count();
    assert_eq!(terminals, 1, "exactly one terminal event: {events:?}");
    assert!(
        matches!(events.last(), Some(AgentEvent::RunCompleted { .. })),
        "last event must be RunCompleted (not a synthetic RunFailed): {events:?}"
    );
}
