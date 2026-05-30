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
    RunResultStreaming, Runner, Session,
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

/// Post-run finalization seam. **SMA-321: placeholder** — flushes zero events
/// so the session handle is wired end-to-end and the "finalize runs on every
/// exit" guarantee is testable now. Session persistence + compaction land in a
/// follow-up, which replaces the empty append with real event writing.
async fn finalize(session: &Arc<dyn Session>) {
    // Placeholder seam: the follow-up that adds session persistence replaces
    // this empty append with real event writing and surfaces/logs the error
    // instead of discarding it.
    let _ = session.append(&[]).await;
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
        // Clone the failure handle before moving ctx into agent.run, mirroring
        // cancel/session above. collect() reads it after the stream drains.
        let failure = ctx.failure_handle();

        let stream = agent.run(ctx, input).await?;
        let (controlled_stream, outcome) = controlled(stream, cancel, timeout);
        // Do NOT `?`-short-circuit before finalize: agent failures surface as
        // collect()=Err, and finalize must still run.
        let collected = RunResultStreaming::with_failure(controlled_stream, failure)
            .collect()
            .await;
        finalize(&session).await;

        // A cancel/timeout outcome wins even if `collected` is Ok (the run may
        // have finished in the same poll the signal fired); `biased` keeps that
        // window small.
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

        let stream = agent.run(ctx, input).await?;
        let (mut controlled_stream, outcome) = controlled(stream, cancel, timeout);

        let out = async_stream::stream! {
            while let Some(ev) = controlled_stream.next().await {
                // Finalize BEFORE exposing a terminal event: a consumer may stop
                // polling (and drop the stream) the moment it sees the terminal,
                // so anything after the `yield` could never run.
                if matches!(
                    ev,
                    AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. }
                ) {
                    finalize(&session).await;
                }
                yield ev;
            }
            // Cancel/timeout: the inner stream ended without a terminal event, so
            // synthesize one — again after finalize, for the same reason.
            match outcome.get() {
                Outcome::Cancelled => {
                    finalize(&session).await;
                    yield AgentEvent::RunFailed {
                        error: "run cancelled".to_owned(),
                    };
                }
                Outcome::TimedOut => {
                    finalize(&session).await;
                    yield AgentEvent::RunFailed {
                        error: "run timed out".to_owned(),
                    };
                }
                Outcome::Completed => {}
            }
        };
        // A later `.collect()` on this streamed handle surfaces structured
        // *agent* failures via the slot (e.g. `RunError::Agent(MaxTurnsExceeded)`).
        // Cancel/timeout are NOT in the slot: they are runner-level outcomes with
        // no `AgentError` equivalent, so they surface only as the synthesized
        // terminal `RunFailed` string events above and a `.collect()` maps them
        // to `RunError::Other`. For structured `Cancelled`/`Timeout`, use `run()`.
        Ok(RunResultStreaming::with_failure(Box::pin(out), failure))
    }
}
