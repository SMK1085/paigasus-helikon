//! The client-side durable [`Runner`](paigasus_helikon_core::Runner)
//! implementation.
//!
//! [`TemporalRunner`](crate::runner::TemporalRunner) implements
//! [`paigasus_helikon_core::Runner`] by starting the durable agent-loop
//! workflow (the crate-internal `workflow` module) on a Temporal task queue,
//! awaiting its total [`crate::payloads::DurableRunOutcome`], and mapping that
//! onto the runner boundary via this crate's `error::outcome_to_run_result`.
//! The agent itself executes on the **worker**
//! ([`crate::worker::TemporalAgentWorker`]); this runner never runs the agent
//! locally — it only needs [`paigasus_helikon_core::Agent::name`] to address
//! the registered agent and to seed the session recorder.
//!
//! # Session semantics (mirrors `TokioRunner`)
//!
//! `run`/`run_streamed` load persisted history and seed the conversation as
//! `history ++ input.messages` (the session owns history, `input` is the new
//! turn), and finalize the run's events into the session on **every** exit
//! path — success, agent failure, cancellation, timeout, or infrastructure
//! error — matching `TokioRunner`'s finalize-on-every-exit guarantee. A
//! session read failure is a hard error (the run cannot faithfully resume from
//! an unreadable session); session writes are best-effort.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, FailureSlot, RunConfig, RunContext, RunError,
    RunInterrupt, RunResult, RunResultStreaming, Runner, Session, SessionRecorder,
};
use temporalio_client::{
    Client, WorkflowCancelOptions, WorkflowGetResultOptions, WorkflowStartOptions,
};

use crate::error::outcome_to_run_result;
use crate::payloads::{DriverConfig, DurableRunOutcome, WorkflowInput};

/// Configuration for a [`TemporalRunner`].
///
/// `#[non_exhaustive]`: construct via [`TemporalRunnerConfig::new`] and the
/// `with_*` builder methods, not a struct literal — this keeps adding a field
/// (e.g. the private `ctx_seed`) a non-breaking change.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TemporalRunnerConfig {
    /// Task queue the durable workflow (and its worker) are served on. Must
    /// match the task queue the [`crate::worker::TemporalAgentWorker`] polls.
    pub task_queue: String,
    /// Workflow id assigned per run.
    ///
    /// `None` (the default) mints a fresh `helikon-run-{uuid-v4}` per run,
    /// client-side. Set this only when the caller needs a deterministic
    /// workflow id (e.g. idempotent start / dedup); reusing the same id across
    /// concurrent runs is a Temporal id-reuse conflict.
    pub workflow_id: Option<String>,
    /// Backstop margin added to `RunConfig::timeout` when setting the hard
    /// Temporal **workflow-execution** timeout.
    ///
    /// The run deadline itself is a durable timer *inside* the workflow (so an
    /// expiry returns `TimedOut` with events-so-far); this execution timeout is
    /// only a safety backstop above it and would discard the outcome, so it is
    /// set generously above the durable timer. Default 60s.
    pub execution_timeout_margin: Duration,
    /// Optional request-scoped seed forwarded to the worker's seeded ctx
    /// factory. Private: set via [`Self::with_ctx_seed`]. Default `None`.
    ctx_seed: Option<serde_json::Value>,
}

impl TemporalRunnerConfig {
    /// Construct a config for `task_queue` with the default workflow-id policy
    /// (`helikon-run-{uuid}`) and a 60s execution-timeout margin.
    pub fn new(task_queue: impl Into<String>) -> Self {
        Self {
            task_queue: task_queue.into(),
            workflow_id: None,
            execution_timeout_margin: Duration::from_secs(60),
            ctx_seed: None,
        }
    }

    /// Attach a request-scoped seed forwarded (explicitly) to the worker's
    /// seeded ctx factory for every run this config drives. Recorded in
    /// Temporal history — keep it small and secret-free.
    pub fn with_ctx_seed(mut self, seed: serde_json::Value) -> Self {
        self.ctx_seed = Some(seed);
        self
    }
}

/// A durable, Temporal-backed [`Runner`].
///
/// Holds a connected [`temporalio_client::Client`] and a
/// [`TemporalRunnerConfig`]; each `run` starts one durable workflow execution
/// on the configured task queue. Construct via [`TemporalRunner::new`].
pub struct TemporalRunner {
    client: Client,
    config: TemporalRunnerConfig,
}

impl TemporalRunner {
    /// Build a runner from a connected client and its configuration.
    pub fn new(client: Client, config: TemporalRunnerConfig) -> Self {
        Self { client, config }
    }

    /// Run the durable workflow to completion, racing the client's cancel
    /// token against the awaited result: a fired token requests cooperative
    /// workflow cancellation, after which the workflow still returns a total
    /// (`Cancelled`) outcome.
    async fn run_workflow(
        &self,
        input: WorkflowInput,
        timeout: Option<Duration>,
        cancel: CancellationToken,
    ) -> Result<DurableRunOutcome, RunError> {
        // Workflow id is minted client-side (never inside the workflow, which
        // must stay deterministic).
        let workflow_id = self
            .config
            .workflow_id
            .clone()
            .unwrap_or_else(|| format!("helikon-run-{}", uuid::Uuid::new_v4()));

        let execution_timeout =
            timeout.map(|d| d.saturating_add(self.config.execution_timeout_margin));
        let start_opts = WorkflowStartOptions::new(self.config.task_queue.clone(), workflow_id)
            .maybe_execution_timeout(execution_timeout)
            .build();

        let handle = self
            .client
            .start_workflow(
                crate::workflow::DurableAgentWorkflow::run,
                input,
                start_opts,
            )
            .await
            .map_err(|e| {
                RunError::Other(anyhow::anyhow!("failed to start temporal workflow: {e}"))
            })?;

        let get_result = handle.get_result(WorkflowGetResultOptions::default());
        tokio::pin!(get_result);
        let mut requested_cancel = false;
        let outcome = loop {
            tokio::select! {
                result = &mut get_result => break result,
                // Once the run's cancel token fires, request cooperative
                // workflow cancellation once, then keep awaiting the (now
                // `Cancelled`) total outcome.
                () = cancel.cancelled(), if !requested_cancel => {
                    requested_cancel = true;
                    let _ = handle.cancel(WorkflowCancelOptions::default()).await;
                }
            }
        };

        outcome.map_err(|e| RunError::Other(anyhow::anyhow!("temporal workflow failed: {e}")))
    }

    /// Shared run path for both [`Runner::run`] and [`Runner::run_streamed`].
    ///
    /// Returns the run's events plus its mapped terminal result. The outer
    /// `Err` is reserved for a session **read** failure (load fails before the
    /// run starts); every other exit — including workflow start/await
    /// infrastructure failures — finalizes the session recorder and reports its
    /// outcome as the inner `Result`.
    async fn run_inner<Ctx>(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<(Vec<AgentEvent>, Result<RunResult, RunError>), RunError>
    where
        Ctx: Send + Sync + 'static,
    {
        let timeout = config.timeout;
        let max_turns = config.max_turns;
        let parallel_tool_call_limit = config.parallel_tool_call_limit;

        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();

        // Session read failure is a hard error (mirrors `TokioRunner`).
        let (merged, recorder) = load_and_record(&session, agent.name(), input).await?;

        let workflow_input = WorkflowInput {
            agent_name: agent.name().to_owned(),
            conversation: merged.messages,
            config: DriverConfig {
                max_turns,
                parallel_tool_call_limit: parallel_tool_call_limit.map(|n| n.get()),
            },
            timeout_ms: timeout.map(|d| d.as_millis() as u64),
            ctx_seed: self.config.ctx_seed.clone(),
        };

        match self.run_workflow(workflow_input, timeout, cancel).await {
            Ok(outcome) => {
                {
                    let mut rec = recorder.lock().expect("session recorder mutex poisoned");
                    for event in &outcome.events {
                        rec.observe(event);
                    }
                }
                finalize(&session, &recorder).await;
                let events = outcome.events.clone();
                Ok((events, outcome_to_run_result(outcome)))
            }
            Err(infra) => {
                // Infra failure: still finalize (the recorder holds this turn's
                // new-turn input) before surfacing the error.
                finalize(&session, &recorder).await;
                Ok((Vec::new(), Err(infra)))
            }
        }
    }
}

#[async_trait]
impl<Ctx> Runner<Ctx> for TemporalRunner
where
    Ctx: Send + Sync + 'static,
{
    async fn run(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResult, RunError> {
        let (_events, result) = self.run_inner(agent, ctx, input, config).await?;
        result
    }

    /// **Buffered, not live.** The durable workflow runs to completion first;
    /// the returned stream then replays the already-recorded
    /// [`AgentEvent`]s as an immediate, finite stream. Because persistence
    /// happened *before* the stream exists (a strictly stronger guarantee than
    /// the trait's warning), dropping the stream early loses nothing. Live
    /// token streaming across the workflow boundary is future work.
    ///
    /// On a failed run a terminal `RunFailed` event is present whenever the
    /// durable log did not already end in a terminal. An agent failure is also
    /// wired into the returned handle's [`FailureSlot`], so
    /// [`RunResultStreaming::collect`] surfaces the typed [`RunError::Agent`] —
    /// matching `TokioRunner`. Cancellation, timeout and infrastructure
    /// failures leave the slot empty, so `collect` returns [`RunError::Other`]
    /// carrying the terminal frame's text, as `TokioRunner` does for an
    /// interrupt. This applies only when the stream ends in a failed state; if
    /// the durable log already ended in a successful terminal state, that
    /// outcome is returned as `Ok` instead.
    async fn run_streamed(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        let (events, result) = self.run_inner(agent, ctx, input, config).await?;
        Ok(into_streaming(events, result))
    }
}

/// The terminal frame [`Runner::run_streamed`] synthesizes when a run ends
/// without one of its own, or `None` when the run succeeded.
///
/// Cancellation and timeout render through [`RunInterrupt::terminal_event`],
/// **not** through [`RunError`]'s `Display`. The two disagree — `RunError::Cancelled`
/// displays as `"cancelled"` while the canonical synthesized frame says
/// `"run cancelled"` — so going through `Display` here would make this runner emit
/// different text than `TokioRunner` for the same event. One rendering, one place
/// (SMA-422, SMA-515).
fn synthetic_terminal_event(result: &Result<RunResult, RunError>) -> Option<AgentEvent> {
    match result {
        Ok(_) => None,
        Err(RunError::Agent(err)) => Some(AgentEvent::RunFailed {
            error: err.to_string(),
        }),
        Err(RunError::Cancelled) => Some(RunInterrupt::Cancelled.terminal_event()),
        Err(RunError::Timeout) => Some(RunInterrupt::TimedOut.terminal_event()),
        // `RunError` is `#[non_exhaustive]` and foreign, so this arm is required.
        // Infrastructure failures have no canonical interrupt text; their own
        // message is the most informative thing available. Not a `RunInterrupt`,
        // so the frame is built here rather than in core.
        Err(other) => Some(AgentEvent::RunFailed {
            error: other.to_string(),
        }),
    }
}

/// Append `event` as the run's terminal frame, unless the durable event log
/// already ends in one.
///
/// The guard is load-bearing (SMA-422, closing SMA-421): a status that mapped to
/// `Err` while the event log ended in `RunCompleted` must not append a second
/// terminal. It tests for *any* terminal via [`AgentEvent::is_terminal`], not
/// just `RunFailed`.
fn append_synthetic_terminal(events: &mut Vec<AgentEvent>, event: Option<AgentEvent>) {
    if let Some(event) = event {
        if !events.last().is_some_and(AgentEvent::is_terminal) {
            events.push(event);
        }
    }
}

/// Assemble the durable run's `(events, result)` into the handle
/// [`Runner::run_streamed`] returns. Pure, so the whole assembly is unit-tested
/// without a Temporal server (SMA-516).
fn into_streaming(
    mut events: Vec<AgentEvent>,
    result: Result<RunResult, RunError>,
) -> RunResultStreaming {
    let failure = FailureSlot::new();
    let terminal = synthetic_terminal_event(&result);
    // Move the structured error into the slot only for an agent failure;
    // the event itself was already rendered above.
    if let Err(RunError::Agent(err)) = result {
        failure.set(err);
    }

    // `collect()` only reads the failure slot once it observes a terminal
    // `RunFailed` in the stream. The durable event log already carries one
    // for `AgentFailed` runs; synthesize one for the terminal states that do
    // not (cancellation/timeout/infra), so a failed run never collects as
    // `Ok`. `append_synthetic_terminal` owns the guard that stops this from
    // appending a *second* terminal (SMA-422).
    append_synthetic_terminal(&mut events, terminal);

    RunResultStreaming::with_failure(stream::iter(events).boxed(), failure)
}

/// Snapshot the session into the merged input and seed a recorder with the
/// run's new-turn messages. A read failure is a hard error: the run cannot
/// faithfully resume from an unreadable session. (Mirrors
/// `TokioRunner::load_and_record`.)
async fn load_and_record(
    session: &Arc<dyn Session>,
    agent_name: &str,
    input: AgentInput,
) -> Result<(AgentInput, Arc<Mutex<SessionRecorder>>), RunError> {
    let snapshot = session
        .snapshot()
        .await
        .map_err(|e| RunError::Other(anyhow::Error::new(e)))?;
    let mut recorder = SessionRecorder::new(agent_name);
    recorder.record_input(&input.messages);

    let mut merged = AgentInput::new();
    merged.messages = snapshot.messages;
    merged.messages.extend(input.messages);
    Ok((merged, Arc::new(Mutex::new(recorder))))
}

/// Post-run finalization: drain the recorder and append the run's events.
/// Persistence is best-effort — an append error is logged, never propagated.
/// (Mirrors `TokioRunner::finalize`.)
async fn finalize(session: &Arc<dyn Session>, recorder: &Arc<Mutex<SessionRecorder>>) {
    let events = recorder
        .lock()
        .expect("session recorder mutex poisoned")
        .drain();
    if let Err(e) = session.append(&events).await {
        tracing::warn!(
            target: "paigasus::runtime_temporal::runner",
            error = %e,
            "session persistence failed during finalize; run outcome unaffected"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::{AgentError, ContentPart, Item, TokenUsage};

    /// Unwrap a synthesized terminal to its message, asserting the variant on
    /// the way through — something the old `-> Option<String>` signature made
    /// impossible.
    fn terminal_text(ev: Option<AgentEvent>) -> Option<String> {
        match ev {
            Some(AgentEvent::RunFailed { error }) => Some(error),
            Some(other) => panic!("synthesized terminal must be RunFailed, got {other:?}"),
            None => None,
        }
    }

    #[test]
    fn with_ctx_seed_stores_seed() {
        let cfg =
            TemporalRunnerConfig::new("q").with_ctx_seed(serde_json::json!({"tenant": "acme"}));
        assert_eq!(cfg.ctx_seed, Some(serde_json::json!({"tenant": "acme"})));
    }

    #[test]
    fn ctx_seed_defaults_none() {
        assert_eq!(TemporalRunnerConfig::new("q").ctx_seed, None);
    }

    /// A cancelled run's synthesized terminal must carry the *canonical*
    /// interrupt text, so this runner and `TokioRunner` are indistinguishable
    /// to a stream consumer. Routing through `RunError`'s `Display` instead
    /// would silently emit "cancelled" here and "run cancelled" there.
    #[test]
    fn cancelled_renders_the_canonical_interrupt_message() {
        let message = terminal_text(synthetic_terminal_event(&Err(RunError::Cancelled)));
        assert_eq!(
            message.as_deref(),
            Some(RunInterrupt::Cancelled.terminal_message())
        );
        assert_eq!(message.as_deref(), Some("run cancelled"));
        assert_ne!(
            message,
            Some(RunError::Cancelled.to_string()),
            "must not fall through to RunError's Display, which says \"cancelled\""
        );
    }

    #[test]
    fn timed_out_renders_the_canonical_interrupt_message() {
        assert_eq!(
            terminal_text(synthetic_terminal_event(&Err(RunError::Timeout))).as_deref(),
            Some(RunInterrupt::TimedOut.terminal_message())
        );
    }

    #[test]
    fn success_synthesizes_no_terminal_event() {
        assert_eq!(
            terminal_text(synthetic_terminal_event(&Ok(RunResult::default()))),
            None,
            "a completed run already carries its own terminal"
        );
    }

    /// An agent failure keeps its own structured message rather than being
    /// flattened into an interrupt's canonical text.
    #[test]
    fn agent_failure_keeps_its_own_message() {
        let message = terminal_text(synthetic_terminal_event(&Err(RunError::Agent(
            AgentError::MaxTurnsExceeded(3),
        ))))
        .expect("an agent failure always has a message");
        assert!(
            message.contains('3'),
            "expected the AgentError's own text, got {message:?}"
        );
    }

    /// The SMA-421 regression guard: a durable log that already ends in a
    /// terminal must not gain a second one, even when the run's status mapped
    /// to `Err`. This test fails if the guard is dropped.
    #[test]
    fn guard_suppresses_a_second_terminal() {
        let mut events = vec![AgentEvent::RunCompleted {
            usage: TokenUsage::default(),
        }];
        append_synthetic_terminal(&mut events, Some(RunInterrupt::Cancelled.terminal_event()));
        assert_eq!(
            events.len(),
            1,
            "a log already ending in a terminal must not gain a second: {events:?}"
        );
        assert!(
            matches!(events[0], AgentEvent::RunCompleted { .. }),
            "the original terminal must survive untouched: {events:?}"
        );
    }

    #[test]
    fn guard_appends_when_the_log_has_no_terminal() {
        let mut events = vec![AgentEvent::TurnStarted { turn: 0 }];
        append_synthetic_terminal(&mut events, Some(RunInterrupt::TimedOut.terminal_event()));
        assert_eq!(
            events.len(),
            2,
            "a log with no terminal must receive the synthetic one: {events:?}"
        );
        assert_eq!(
            terminal_text(events.pop()).as_deref(),
            Some(RunInterrupt::TimedOut.terminal_message())
        );
    }

    #[test]
    fn no_event_appends_nothing() {
        let mut events = vec![AgentEvent::TurnStarted { turn: 0 }];
        append_synthetic_terminal(&mut events, None);
        assert_eq!(events.len(), 1, "None must be a no-op: {events:?}");

        let mut empty: Vec<AgentEvent> = Vec::new();
        append_synthetic_terminal(&mut empty, None);
        assert!(empty.is_empty(), "None must be a no-op on an empty log");
    }

    // ---- SMA-516: the assembled `RunResultStreaming` ----------------------
    //
    // `collect()` consumes the handle and drops its events on `Err`, and
    // `RunError` is not `Clone`. So every `Err` case builds its input twice:
    // one handle is drained for the frames, a fresh one is collected. Draining
    // and then collecting the SAME handle would see an empty stream and return
    // `Ok` for any input — a test that can never fail.

    type Input = (Vec<AgentEvent>, Result<RunResult, RunError>);

    /// Drain a handle's raw frames.
    async fn frames(streaming: RunResultStreaming) -> Vec<AgentEvent> {
        futures_util::StreamExt::collect::<Vec<_>>(streaming.events).await
    }

    /// Exactly one terminal, and it is the last frame.
    fn assert_one_terminal_last(frames: &[AgentEvent]) {
        assert_eq!(
            frames.iter().filter(|e| e.is_terminal()).count(),
            1,
            "exactly one terminal frame: {frames:?}"
        );
        assert!(
            frames.last().is_some_and(AgentEvent::is_terminal),
            "the terminal must be the last frame: {frames:?}"
        );
    }

    const UNKNOWN_AGENT: &str = "no agent named 'x' is registered on this worker";
    const INFRA: &str = "temporal workflow failed: boom";

    /// T1: the realistic agent failure — the driver already wrote `RunFailed`.
    fn agent_failure_with_terminal() -> Input {
        (
            vec![
                AgentEvent::TurnStarted { turn: 0 },
                AgentEvent::RunFailed {
                    error: AgentError::MaxTurnsExceeded(3).to_string(),
                },
            ],
            Err(RunError::Agent(AgentError::MaxTurnsExceeded(3))),
        )
    }

    /// T2: reachable today — `workflow.rs`'s `unknown_agent_outcome` returns
    /// `AgentFailed` with NO events (so do the driver's `Handoff` and
    /// unknown-`NextAction` arms). Both the synthetic frame and the slot are
    /// load-bearing here.
    fn agent_failure_without_terminal() -> Input {
        (
            Vec::new(),
            Err(RunError::Agent(AgentError::Other(anyhow::anyhow!(
                UNKNOWN_AGENT
            )))),
        )
    }

    fn interrupted(err: RunError) -> Input {
        (vec![AgentEvent::TurnStarted { turn: 0 }], Err(err))
    }

    /// T5: an infrastructure failure — `run_inner` returns no events.
    fn infra_failure() -> Input {
        (Vec::new(), Err(RunError::Other(anyhow::anyhow!(INFRA))))
    }

    /// T1. Fails if the slot is not set, if `new` replaces `with_failure`, if
    /// the guard is bypassed, or if the guard tests only `RunCompleted` or reads
    /// `events.first()` — the last two survive every SMA-515 guard test.
    #[tokio::test]
    async fn agent_failure_with_its_own_terminal_collects_the_typed_error() {
        let (events, result) = agent_failure_with_terminal();
        let frames = frames(into_streaming(events, result)).await;
        assert_eq!(
            frames.len(),
            2,
            "the log must pass through unchanged: {frames:?}"
        );
        assert!(matches!(frames[0], AgentEvent::TurnStarted { turn: 0 }));
        assert_one_terminal_last(&frames);

        let (events, result) = agent_failure_with_terminal();
        match into_streaming(events, result).collect().await {
            Err(RunError::Agent(AgentError::MaxTurnsExceeded(3))) => {}
            Err(other) => {
                panic!("expected the typed RunError::Agent(MaxTurnsExceeded(3)), got {other:?}")
            }
            Ok(_) => panic!("a failed run must never collect as Ok"),
        }
    }

    /// T2.
    #[tokio::test]
    async fn agent_failure_without_a_terminal_gets_one_and_the_typed_error() {
        let (events, result) = agent_failure_without_terminal();
        let frames = frames(into_streaming(events, result)).await;
        assert_eq!(frames.len(), 1, "one synthetic frame: {frames:?}");
        assert_one_terminal_last(&frames);
        match &frames[0] {
            AgentEvent::RunFailed { error } => assert_eq!(error, UNKNOWN_AGENT),
            other => panic!("the synthetic terminal must be RunFailed, got {other:?}"),
        }

        let (events, result) = agent_failure_without_terminal();
        match into_streaming(events, result).collect().await {
            Err(RunError::Agent(AgentError::Other(e))) => assert_eq!(e.to_string(), UNKNOWN_AGENT),
            Err(other) => panic!("expected the typed RunError::Agent(Other(..)), got {other:?}"),
            Ok(_) => panic!("a failed run must never collect as Ok"),
        }
    }

    /// Shared body for T3–T5: the slot must stay EMPTY, so `collect()` returns
    /// `RunError::Other` carrying the frame text — the same as `TokioRunner`
    /// for an interrupt. The variant is pinned on purpose: a change to it must
    /// update these tests in the same PR.
    async fn assert_untyped_failure(input: fn() -> Input, prefix_len: usize, text: &str) {
        let (events, result) = input();
        let frames = frames(into_streaming(events, result)).await;
        assert_eq!(
            frames.len(),
            prefix_len + 1,
            "prefix plus one synthetic frame: {frames:?}"
        );
        if prefix_len == 1 {
            assert!(
                matches!(frames[0], AgentEvent::TurnStarted { turn: 0 }),
                "the input prefix must stay in place: {frames:?}"
            );
        }
        assert_one_terminal_last(&frames);
        match frames.last() {
            Some(AgentEvent::RunFailed { error }) => assert_eq!(error, text),
            other => panic!("the synthetic terminal must be RunFailed, got {other:?}"),
        }

        let (events, result) = input();
        match into_streaming(events, result).collect().await {
            Err(RunError::Other(e)) => assert_eq!(e.to_string(), text),
            Err(other) => {
                panic!("the slot must stay empty, so expected RunError::Other, got {other:?}")
            }
            Ok(_) => panic!("a failed run must never collect as Ok"),
        }
    }

    /// T3.
    #[tokio::test]
    async fn cancelled_run_collects_as_err_with_the_canonical_text() {
        assert_untyped_failure(|| interrupted(RunError::Cancelled), 1, "run cancelled").await;
    }

    /// T4.
    #[tokio::test]
    async fn timed_out_run_collects_as_err_with_the_canonical_text() {
        assert_untyped_failure(
            || interrupted(RunError::Timeout),
            1,
            RunInterrupt::TimedOut.terminal_message(),
        )
        .await;
    }

    /// T5.
    #[tokio::test]
    async fn infra_failure_collects_as_err_with_its_own_text() {
        assert_untyped_failure(infra_failure, 0, INFRA).await;
    }

    /// T6.
    #[tokio::test]
    async fn successful_run_gains_no_frame_and_collects_ok() {
        let events = vec![
            AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text {
                        text: "hi".to_owned(),
                    }],
                    agent: None,
                },
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        let collected = into_streaming(events, Ok(RunResult::default()))
            .collect()
            .await
            .unwrap_or_else(|e| panic!("a completed run must collect as Ok, got {e:?}"));
        assert_eq!(collected.final_output, "hi");
        assert_eq!(
            collected.events.len(),
            2,
            "no frame appended: {:?}",
            collected.events
        );
        assert_one_terminal_last(&collected.events);
        assert!(matches!(
            collected.events[1],
            AgentEvent::RunCompleted { .. }
        ));
    }

    /// T7 — decided 2026-09-26: the event log wins. A log ending in
    /// `RunCompleted` gets no second terminal even when the result is `Err`, so
    /// `collect()` returns `Ok` while `run()` would return `Err`. `run()` and
    /// `collect()` differ in `Ok` versus `Err` only in this state. It is
    /// unreachable today: `DurableDriver::interrupt` returns a `Done` outcome
    /// unchanged, pinned by `driver.rs`'s `terminal_wins_over_late_interrupt`.
    #[tokio::test]
    async fn log_ending_in_run_completed_wins_over_an_err_result() {
        let events = vec![
            AgentEvent::TurnStarted { turn: 0 },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        let collected = into_streaming(events, Err(RunError::Cancelled))
            .collect()
            .await
            .unwrap_or_else(|e| panic!("the log's RunCompleted must win, got {e:?}"));
        assert_eq!(
            collected.events.len(),
            2,
            "no second terminal: {:?}",
            collected.events
        );
        assert_one_terminal_last(&collected.events);
        assert!(matches!(
            collected.events[1],
            AgentEvent::RunCompleted { .. }
        ));
    }
}
