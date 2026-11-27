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
    effective_interrupt, open_deferrals, Agent, AgentEvent, AgentInput, CancellationToken,
    DeferralSlot, ResumePlan, RunConfig, RunContext, RunError, RunInterrupt, RunResult,
    RunResultStreaming, Runner, Session, SessionError, SessionRecorder, SessionWriteSlot,
    StopReason,
};

pub mod retry;
pub use retry::{RetryPolicy, RetryingModel};

/// Read handle for the interrupt committed by [`controlled`], if any.
struct InterruptHandle(Arc<Mutex<Option<RunInterrupt>>>);

impl InterruptHandle {
    fn get(&self) -> Option<RunInterrupt> {
        *self.0.lock().unwrap()
    }
}

/// Wrap an agent event stream with cancel/deadline control.
///
/// Passes agent events through. On cancellation or deadline it commits the
/// reason into the returned handle and ends the stream (dropping the inner
/// stream cancels nested in-flight awaits within one poll). The interrupt, if
/// any, is committed *before* the terminating `None`, so a caller reading the
/// handle after draining never sees a stale value. A run that ends naturally
/// commits nothing and the handle stays `None`. A stream that ends
/// after the token fired commits `Cancelled`.
fn controlled(
    mut stream: BoxStream<'static, AgentEvent>,
    cancel: CancellationToken,
    timeout: Option<Duration>,
) -> (BoxStream<'static, AgentEvent>, InterruptHandle) {
    let cell = Arc::new(Mutex::new(None));
    let handle = InterruptHandle(Arc::clone(&cell));
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
                        None => {
                            // The agent stream ended. If the token fired, the
                            // agent ended early because of the cancel (a model
                            // must not emit `Finish` on cancel), so record it.
                            // A real terminal event still wins in `run`.
                            if cancel.is_cancelled() {
                                *cell.lock().unwrap() = Some(RunInterrupt::Cancelled);
                            }
                            break;
                        }
                    }
                }
                () = cancel.cancelled() => {
                    *cell.lock().unwrap() = Some(RunInterrupt::Cancelled);
                    break;
                }
                () = &mut sleep => {
                    *cell.lock().unwrap() = Some(RunInterrupt::TimedOut);
                    break;
                }
            }
        }
    };
    (Box::pin(out), handle)
}

/// What [`load_and_record`] gives the run.
struct Loaded {
    /// The input for the agent.
    input: AgentInput,
    /// The recorder, seeded with the run's new input messages.
    recorder: Arc<Mutex<SessionRecorder>>,
    /// `true` when the run resumes deferred tool calls.
    resuming: bool,
}

/// Read the session, make the agent input, and seed a recorder with the run's
/// new input messages.
///
/// A read failure is a hard error: the run cannot faithfully resume from an
/// unreadable session, so it fails before the agent starts.
///
/// The run resumes when the input has resolutions or the log has open
/// deferrals. A resume gets a validated [`ResumePlan`]: the plan carries the
/// history, and the agent input carries only the new messages. An invalid
/// resume fails with `RunError::InvalidResume` before the agent starts, and
/// nothing is recorded. Other runs get `history ++ input.messages`, as before.
///
/// The recorder always gets the run's deferral slot, so a stored
/// `ToolDeferred` event holds the raw arguments, not the redacted event copy.
async fn load_and_record(
    session: &Arc<dyn Session>,
    agent_name: &str,
    input: AgentInput,
    deferrals: DeferralSlot,
    max_agent_depth: u32,
) -> Result<Loaded, RunError> {
    let snapshot = session
        .snapshot()
        .await
        .map_err(|e| RunError::Other(anyhow::Error::new(e)))?;
    let events = session
        .events(None)
        .await
        .map_err(|e| RunError::Other(anyhow::Error::new(e)))?;
    let resuming = !input.resolutions().is_empty() || !open_deferrals(&events).is_empty();

    let mut recorder = SessionRecorder::new(agent_name).with_deferral_slot(deferrals);
    let mut agent_input = AgentInput::new();
    if resuming {
        let plan =
            ResumePlan::from_events(&events, &snapshot, agent_name, &input, max_agent_depth)?;
        recorder = recorder.with_resume_plan(&plan);
        recorder.record_input(&input.messages);
        agent_input.messages = input.messages;
        agent_input = agent_input.with_resume_plan(plan);
    } else {
        recorder.record_input(&input.messages);
        agent_input.messages = snapshot.messages;
        agent_input.messages.extend(input.messages);
    }
    Ok(Loaded {
        input: agent_input,
        recorder: Arc::new(Mutex::new(recorder)),
        resuming,
    })
}

/// Post-run finalization: drain the recorder and append the run's events.
///
/// An append error is logged and returned. The caller decides what it means:
/// a run that defers or resumes fails with it (the session lost the deferral
/// or the resumed results), and other runs ignore it (best-effort).
async fn finalize(
    session: &Arc<dyn Session>,
    recorder: &Arc<Mutex<SessionRecorder>>,
) -> Result<(), SessionError> {
    let events = recorder
        .lock()
        .expect("session recorder mutex poisoned")
        .drain();
    let result = session.append(&events).await;
    if let Err(e) = &result {
        tracing::warn!(
            target: "paigasus::runtime_tokio::runner",
            error = %e,
            "session persistence failed during finalize"
        );
    }
    result
}

/// Record a lost strict session write and make the `RunFailed` event that
/// replaces the run's terminal event.
fn lost_write(slot: &SessionWriteSlot, e: SessionError) -> AgentEvent {
    let error = format!("session write failed: {e}");
    slot.set(e);
    AgentEvent::RunFailed { error }
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
        let max_agent_depth = config.max_agent_depth;
        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();
        let failure = ctx.failure_handle();

        // Load persisted history (or the resume plan) and seed the recorder
        // with the new turn.
        let Loaded {
            input,
            recorder,
            resuming,
        } = load_and_record(
            &session,
            agent.name(),
            input,
            ctx.deferral_handle(),
            max_agent_depth,
        )
        .await?;

        let stream = agent.run(ctx, input).await?;
        let (controlled_stream, interrupt) = controlled(stream, cancel, timeout);
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
        let written = finalize(&session, &recorder).await;

        // A run that defers or resumes must not lose its write: the session
        // would lose the deferral, or the results of calls that may have run.
        // This error wins over the agent result and the interrupt.
        let deferred = matches!(
            &collected,
            Ok(r) if matches!(r.stop_reason, StopReason::Deferred { .. })
        );
        if let Err(e) = written {
            if resuming || deferred {
                return Err(RunError::SessionWrite(e));
            }
        }

        // A genuine terminal event (RunCompleted/RunFailed) is the run's true
        // outcome; a cancel/timeout overrides ONLY when no terminal was
        // observed — i.e. it actually aborted the run in-flight. The rule
        // itself lives in core as `effective_interrupt` so every runner applies
        // one definition (SMA-422); see `Runner::run`'s docs for the contract
        // and SMA-421 for the bug it closes.
        let saw_terminal = collected
            .as_ref()
            .map(|r| r.events.iter().any(AgentEvent::is_terminal))
            .unwrap_or(true); // Err(_) from collect() ⇔ a RunFailed was observed

        match effective_interrupt(interrupt.get(), saw_terminal).map(RunInterrupt::run_error) {
            Some(err) => Err(err),
            None => collected,
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
        let max_agent_depth = config.max_agent_depth;
        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();
        let failure = ctx.failure_handle();

        let Loaded {
            input,
            recorder,
            resuming,
        } = load_and_record(
            &session,
            agent.name(),
            input,
            ctx.deferral_handle(),
            max_agent_depth,
        )
        .await?;

        let stream = agent.run(ctx, input).await?;
        let (controlled_stream, interrupt) = controlled(stream, cancel, timeout);
        let rec_inspect = Arc::clone(&recorder);
        let mut recorded = controlled_stream
            .inspect(move |ev| {
                rec_inspect
                    .lock()
                    .expect("session recorder mutex poisoned")
                    .observe(ev)
            })
            .boxed();

        let slot = SessionWriteSlot::new();
        let write_slot = slot.clone();
        let out = async_stream::stream! {
            let mut saw_terminal = false;
            let mut finalized = false;
            while let Some(ev) = recorded.next().await {
                // Finalize BEFORE exposing a terminal event: a consumer may stop
                // polling (and drop the stream) the moment it sees the terminal,
                // so anything after the `yield` could never run.
                if AgentEvent::is_terminal(&ev) {
                    saw_terminal = true;
                    if !finalized {
                        finalized = true;
                        let written = finalize(&session, &recorder).await;
                        // A run that defers or resumes must not lose its
                        // write; it ends with RunFailed instead, and collect()
                        // returns RunError::SessionWrite from the slot.
                        let strict = resuming || matches!(ev, AgentEvent::RunDeferred { .. });
                        if let (Err(e), true) = (written, strict) {
                            yield lost_write(&write_slot, e);
                            continue;
                        }
                    }
                }
                yield ev;
            }
            // Synthesize a terminal ONLY when the run aborted in-flight (no real
            // terminal was ever yielded). Reaching here with `!saw_terminal`
            // means the loop never finalized, so finalize directly. A late
            // cancel/timeout that fired AFTER a real terminal — e.g. during a
            // suspending OnRunComplete hook — must NOT emit a second, synthetic
            // terminal; `effective_interrupt` is what decides that (SMA-422,
            // closing SMA-421). Note the natural-completion case (no terminal,
            // no interrupt) deliberately does not finalize here — pre-existing
            // behaviour, preserved.
            if let Some(i) = effective_interrupt(interrupt.get(), saw_terminal) {
                // A resuming run is strict here too: the approved tool may
                // have started. Other runs stay best-effort.
                match finalize(&session, &recorder).await {
                    Err(e) if resuming => yield lost_write(&write_slot, e),
                    _ => yield i.terminal_event(),
                }
            }
        };
        Ok(RunResultStreaming::with_failure(Box::pin(out), failure).with_session_write(slot))
    }
}
