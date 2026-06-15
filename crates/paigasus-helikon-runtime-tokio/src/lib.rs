//! `paigasus-helikon-runtime-tokio` — the default ephemeral Tokio runner.
//!
//! [`TokioRunner`] implements [`paigasus_helikon_core::Runner`] by consuming
//! the agent's [`paigasus_helikon_core::AgentEvent`] stream and adding
//! run-level control (cancellation, timeout, aggregation) at the boundary. It
//! does not own the loop driver — the agent does (see ADR-13).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream::StreamExt as _;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, RunConfig, RunContext, RunError, RunResult,
    RunResultStreaming, Runner, Session, SessionRecorder,
};

/// How a controlled run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Completed,
    Cancelled,
    TimedOut,
}

/// Read handle for the outcome committed by [`controlled`].
struct OutcomeHandle(Arc<Mutex<Outcome>>);

impl OutcomeHandle {
    fn get(&self) -> Outcome {
        *self.0.lock().unwrap()
    }
}

/// Wrap an agent event stream with cancel/deadline control.
///
/// Passes agent events through. On cancellation or deadline it commits the
/// reason into the returned handle and ends the stream (dropping the inner
/// stream cancels nested in-flight awaits within one poll). The outcome is
/// committed *before* the terminating `None`, so a caller reading the handle
/// after draining never sees a stale value.
fn controlled(
    mut stream: BoxStream<'static, AgentEvent>,
    cancel: CancellationToken,
    timeout: Option<Duration>,
) -> (BoxStream<'static, AgentEvent>, OutcomeHandle) {
    let cell = Arc::new(Mutex::new(Outcome::Completed));
    let handle = OutcomeHandle(Arc::clone(&cell));
    let out = async_stream::stream! {
        let sleep = async move {
            match timeout {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                biased;
                maybe_ev = stream.next() => {
                    match maybe_ev {
                        Some(ev) => yield ev,
                        None => break, // inner stream done => Completed (default)
                    }
                }
                () = cancel.cancelled() => {
                    *cell.lock().unwrap() = Outcome::Cancelled;
                    break;
                }
                () = &mut sleep => {
                    *cell.lock().unwrap() = Outcome::TimedOut;
                    break;
                }
            }
        }
    };
    (Box::pin(out), handle)
}

/// Snapshot the session into the merged input and seed a recorder with the
/// run's new-turn messages. A read failure is a hard error: the run cannot
/// faithfully resume from an unreadable session, so it fails before the agent
/// starts. (The write side, by contrast, is best-effort — see `finalize`.)
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
/// Persistence is best-effort — an append error is logged, never propagated, so
/// the run's outcome (Ok / Cancelled / Timeout / Agent error) is unchanged.
async fn finalize(session: &Arc<dyn Session>, recorder: &Arc<Mutex<SessionRecorder>>) {
    let events = recorder
        .lock()
        .expect("session recorder mutex poisoned")
        .drain();
    if let Err(e) = session.append(&events).await {
        tracing::warn!(
            error = %e,
            "session persistence failed during finalize; run outcome unaffected"
        );
    }
}

/// The default ephemeral execution backend. Stateless.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioRunner;

#[async_trait]
impl<Ctx> Runner<Ctx> for TokioRunner
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
        let timeout = config.timeout;
        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();
        let failure = ctx.failure_handle();

        // Load persisted history and seed the recorder with the new turn.
        let (merged, recorder) = load_and_record(&session, agent.name(), input).await?;

        let stream = agent.run(ctx, merged).await?;
        let (controlled_stream, outcome) = controlled(stream, cancel, timeout);
        let rec_inspect = Arc::clone(&recorder);
        let recorded = controlled_stream
            .inspect(move |ev| {
                rec_inspect
                    .lock()
                    .expect("session recorder mutex poisoned")
                    .observe(ev)
            })
            .boxed();
        // Do NOT `?`-short-circuit before finalize: agent failures surface as
        // collect()=Err, and finalize must still run.
        let collected = RunResultStreaming::with_failure(recorded, failure)
            .collect()
            .await;
        finalize(&session, &recorder).await;

        // A cancel/timeout outcome wins even if `collected` is Ok (the run may
        // have finished in the same poll the signal fired); `biased` keeps that
        // window small. This precedence is deliberate (SMA-321) — see
        // `prefired_cancel_still_completes_ready_run`.
        match outcome.get() {
            Outcome::Cancelled => Err(RunError::Cancelled),
            Outcome::TimedOut => Err(RunError::Timeout),
            Outcome::Completed => collected,
        }
    }

    async fn run_streamed(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        let timeout = config.timeout;
        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();
        let failure = ctx.failure_handle();

        let (merged, recorder) = load_and_record(&session, agent.name(), input).await?;

        let stream = agent.run(ctx, merged).await?;
        let (controlled_stream, outcome) = controlled(stream, cancel, timeout);
        let rec_inspect = Arc::clone(&recorder);
        let mut recorded = controlled_stream
            .inspect(move |ev| {
                rec_inspect
                    .lock()
                    .expect("session recorder mutex poisoned")
                    .observe(ev)
            })
            .boxed();

        let out = async_stream::stream! {
            while let Some(ev) = recorded.next().await {
                // Finalize BEFORE exposing a terminal event: a consumer may stop
                // polling (and drop the stream) the moment it sees the terminal,
                // so anything after the `yield` could never run.
                if matches!(
                    ev,
                    AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. }
                ) {
                    finalize(&session, &recorder).await;
                }
                yield ev;
            }
            // Cancel/timeout: the inner stream ended without a terminal event, so
            // synthesize one — again after finalize, for the same reason.
            match outcome.get() {
                Outcome::Cancelled => {
                    finalize(&session, &recorder).await;
                    yield AgentEvent::RunFailed { error: "run cancelled".to_owned() };
                }
                Outcome::TimedOut => {
                    finalize(&session, &recorder).await;
                    yield AgentEvent::RunFailed { error: "run timed out".to_owned() };
                }
                Outcome::Completed => {}
            }
        };
        Ok(RunResultStreaming::with_failure(Box::pin(out), failure))
    }
}
