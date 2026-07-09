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
    RunResult, RunResultStreaming, Runner, Session, SessionRecorder,
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
    /// On a failed run the terminal error is wired into the returned handle's
    /// [`FailureSlot`] and a terminal `RunFailed` event is guaranteed present,
    /// so [`RunResultStreaming::collect`] surfaces the typed
    /// [`RunError::Agent`] — matching `TokioRunner`.
    async fn run_streamed(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        let (mut events, result) = self.run_inner(agent, ctx, input, config).await?;

        let failure = FailureSlot::new();
        let terminal_message = match result {
            Ok(_) => None,
            Err(RunError::Agent(err)) => {
                let message = err.to_string();
                failure.set(err);
                Some(message)
            }
            Err(other) => Some(other.to_string()),
        };

        // `collect()` only reads the failure slot once it observes a terminal
        // `RunFailed` in the stream. The durable event log already carries one
        // for `AgentFailed` runs; synthesize one for the terminal states that
        // do not (cancellation/timeout/infra), so a failed run never collects
        // as `Ok`.
        if let Some(message) = terminal_message {
            if !matches!(events.last(), Some(AgentEvent::RunFailed { .. })) {
                events.push(AgentEvent::RunFailed { error: message });
            }
        }

        Ok(RunResultStreaming::with_failure(
            stream::iter(events).boxed(),
            failure,
        ))
    }
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
            error = %e,
            "session persistence failed during finalize; run outcome unaffected"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
