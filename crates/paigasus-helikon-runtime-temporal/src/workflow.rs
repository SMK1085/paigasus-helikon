//! The durable agent-loop Temporal workflow.
//!
//! [`DurableAgentWorkflow`] is a **mechanical executor** of
//! [`crate::driver::DurableDriver`]: it owns no agent logic of its own. It
//! loops `driver.next_effect()` and satisfies each requested
//! [`crate::driver::DriverEffect`] with a Temporal activity
//! ([`crate::activities`]'s `render_instructions` / `call_model` /
//! `invoke_tool`), feeding the activity result back through the matching
//! `apply_*` method. The whole loop is raced against a durable run-deadline
//! timer and cooperative workflow cancellation; both interruptions call
//! [`crate::driver::DurableDriver::interrupt`] and return a normal
//! [`crate::payloads::DurableRunOutcome`] (finalize-on-every-exit).
//!
//! # Determinism
//!
//! Everything in this module runs inside the Temporal workflow sandbox, so it
//! must be deterministic across replay: no wall-clock time, no `uuid`, no
//! randomness, and no `HashMap` **iteration** (a single keyed
//! [`std::collections::HashMap::get`] by `WorkflowInput::agent_name` is
//! deterministic and is the only map access performed here). The workflow's
//! own decision-making is entirely delegated to `core::transition` (via the
//! driver); concurrency and timers use the SDK's deterministic
//! [`temporalio_sdk::workflows::join_all`] / durable-timer primitives.
//!
//! # Registry closure (how the plan reaches the workflow)
//!
//! An [`crate::driver::AgentPlan`] must **never** be serialized (its
//! `OutputType` validator fails closed after a round-trip — see
//! [`crate::activities::DurableAgentDef`]). The workflow therefore resolves
//! its plan from a **process-local**, `Ctx`-free
//! `HashMap<String, AgentPlan>` closed over by the workflow factory:
//! [`crate::worker::TemporalAgentWorkerBuilder::build`] derives that map from
//! its `Ctx`-generic agent registry and passes it (plus the per-activity
//! [`temporalio_sdk::ActivityOptions`]) into [`build_activity_config`], whose
//! [`WorkflowActivityConfig`] the `register_workflow_with_factory` closure
//! clones into each [`DurableAgentWorkflow`] instance. `AgentPlan` is `Ctx`-
//! free, so the workflow struct needs no `Ctx` type parameter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt as _;
use paigasus_helikon_core::{RunInterrupt, TokenUsage, ToolCallOutcome, ToolCallRequest};
use temporalio_common::protos::temporal::api::common::v1::RetryPolicy;
use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{ActivityExecutionError, ActivityOptions, WorkflowContext, WorkflowResult};

use crate::activities::AgentActivities;
use crate::activity_input::{CallModelArgs, InvokeToolArgs, RenderInstructionsArgs};
use crate::driver::{AgentPlan, DriverEffect, DurableDriver};
use crate::error::ErrorKindPayload;
use crate::payloads::{DurableRunOutcome, RunStatusPayload, WorkflowInput};
use crate::worker::RetryPolicyConfig;

/// Default per-attempt activity timeout (start-to-close) for every durable
/// activity this workflow schedules.
///
/// v0 uses one generous default so a slow model or tool call does not fail its
/// attempt spuriously; a crashed worker's in-flight attempt still times out
/// after this bound and is re-dispatched per the activity's retry policy.
/// Override per activity via [`crate::worker::TemporalAgentWorkerBuilder::model_start_to_close`]
/// / [`crate::worker::TemporalAgentWorkerBuilder::tool_start_to_close`].
const DEFAULT_ACTIVITY_START_TO_CLOSE: Duration = Duration::from_secs(300);

/// Per-activity start-to-close timeout overrides.
///
/// Each field bounds a **single execution attempt** of the corresponding
/// activity (`render_instructions` / `call_model` / `invoke_tool`). Temporal
/// detects a dead worker only by an activity attempt overrunning its
/// start-to-close bound, so a shorter `tool`/`model` timeout is what makes a
/// crashed worker's in-flight attempt re-dispatch promptly — the knob a
/// crash-resume-sensitive deployment (or the crash-resume test) tunes down.
/// Defaults to [`DEFAULT_ACTIVITY_START_TO_CLOSE`] for all three.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ActivityTimeouts {
    /// Start-to-close for the `render_instructions` activity.
    pub instructions: Duration,
    /// Start-to-close for the `call_model` activity.
    pub model: Duration,
    /// Start-to-close for the `invoke_tool` activity.
    pub tool: Duration,
}

impl Default for ActivityTimeouts {
    fn default() -> Self {
        Self {
            instructions: DEFAULT_ACTIVITY_START_TO_CLOSE,
            model: DEFAULT_ACTIVITY_START_TO_CLOSE,
            tool: DEFAULT_ACTIVITY_START_TO_CLOSE,
        }
    }
}

/// Process-local, `Ctx`-free durable-run configuration the workflow factory
/// closes over.
///
/// Built once per worker by [`build_activity_config`] and cloned (via `Arc`)
/// into every [`DurableAgentWorkflow`] instance the factory produces. Never
/// serialized — an [`AgentPlan`]'s `OutputType` validator does not survive a
/// deserialize round-trip.
pub(crate) struct WorkflowActivityConfig {
    /// Agent plans keyed by name, resolved by [`WorkflowInput::agent_name`]
    /// with a single [`HashMap::get`] (never iterated inside the workflow).
    plans: HashMap<String, AgentPlan>,
    /// [`ActivityOptions`] for the `render_instructions` activity (no
    /// caller-configurable retry policy in v0 — uses the Temporal server
    /// default).
    instructions_activity_opts: ActivityOptions,
    /// [`ActivityOptions`] for the `call_model` activity, carrying the
    /// worker's configured model retry policy.
    model_activity_opts: ActivityOptions,
    /// [`ActivityOptions`] for the `invoke_tool` activity, carrying the
    /// worker's configured tool retry policy.
    tool_activity_opts: ActivityOptions,
}

/// Assemble the [`WorkflowActivityConfig`] the worker's
/// `register_workflow_with_factory` closure clones into each workflow
/// instance.
///
/// `plans` is the `Ctx`-free projection of the worker's agent registry
/// (`name → AgentPlan`); `model_retry`/`tool_retry` are the builder's
/// [`RetryPolicyConfig`]s, converted here into the proto retry policy attached
/// to the corresponding activity options (un-inerting the fields Task 7
/// stored); `timeouts` supplies the per-activity start-to-close bound.
/// `heartbeat_timeout` is applied to the model/tool activities only —
/// `render_instructions` never gets one.
pub(crate) fn build_activity_config(
    plans: HashMap<String, AgentPlan>,
    model_retry: &RetryPolicyConfig,
    tool_retry: &RetryPolicyConfig,
    timeouts: &ActivityTimeouts,
    heartbeat_timeout: Option<Duration>,
) -> WorkflowActivityConfig {
    WorkflowActivityConfig {
        plans,
        instructions_activity_opts: activity_opts(timeouts.instructions, None, None),
        model_activity_opts: activity_opts(
            timeouts.model,
            to_proto_retry_policy(model_retry),
            heartbeat_timeout,
        ),
        tool_activity_opts: activity_opts(
            timeouts.tool,
            to_proto_retry_policy(tool_retry),
            heartbeat_timeout,
        ),
    }
}

/// Build [`ActivityOptions`] with the given start-to-close timeout, an
/// optional retry policy, and an optional heartbeat timeout.
fn activity_opts(
    start_to_close: Duration,
    retry_policy: Option<RetryPolicy>,
    heartbeat_timeout: Option<Duration>,
) -> ActivityOptions {
    ActivityOptions::with_start_to_close_timeout(start_to_close)
        .maybe_retry_policy(retry_policy)
        .maybe_heartbeat_timeout(heartbeat_timeout)
        .build()
}

/// Convert a [`RetryPolicyConfig`] into the proto retry policy Temporal's
/// [`ActivityOptions`] expects.
///
/// Returns `None` when every field is unset/empty, so the activity uses the
/// Temporal server's own default retry policy rather than an all-zero proto.
fn to_proto_retry_policy(cfg: &RetryPolicyConfig) -> Option<RetryPolicy> {
    let is_default = cfg.initial_interval.is_none()
        && cfg.backoff_coefficient.is_none()
        && cfg.maximum_interval.is_none()
        && cfg.maximum_attempts.is_none()
        && cfg.non_retryable_error_types.is_empty();
    if is_default {
        return None;
    }

    // A prost-generated message. `backoff_coefficient = 0.0` and
    // `maximum_attempts = 0` are the proto's "server default" sentinels, so
    // leaving an unset field at its `Default` here is correct (unlimited
    // attempts / server-default backoff).
    Some(RetryPolicy {
        initial_interval: cfg.initial_interval.and_then(|d| d.try_into().ok()),
        backoff_coefficient: cfg.backoff_coefficient.unwrap_or_default(),
        maximum_interval: cfg.maximum_interval.and_then(|d| d.try_into().ok()),
        maximum_attempts: cfg
            .maximum_attempts
            .map(|max| i32::try_from(max).unwrap_or(i32::MAX))
            .unwrap_or_default(),
        non_retryable_error_types: cfg.non_retryable_error_types.clone(),
    })
}

/// The durable agent-loop workflow.
///
/// Registered on the worker's task queue via
/// [`crate::worker::TemporalAgentWorkerBuilder::build`]'s
/// `register_workflow_with_factory` call. Started per run by
/// [`crate::runner::TemporalRunner`], which passes the [`Self::run`] marker to
/// `temporalio_client::Client::start_workflow`.
#[workflow]
pub(crate) struct DurableAgentWorkflow {
    /// Process-local run configuration (agent plans + activity options),
    /// cloned in by the workflow factory.
    config: Arc<WorkflowActivityConfig>,
}

impl DurableAgentWorkflow {
    /// Construct a workflow instance for the factory closure. Cheap: clones a
    /// single `Arc`.
    pub(crate) fn new(config: Arc<WorkflowActivityConfig>) -> Self {
        Self { config }
    }
}

#[workflow_methods(factory_only)]
impl DurableAgentWorkflow {
    /// Drive one durable agent run to a total [`DurableRunOutcome`].
    ///
    /// `factory_only`: the instance (and its `config`) is produced by the
    /// worker's factory closure, so no `#[init]`/`Default` is needed; `run`
    /// reads the closed-over config from workflow state via
    /// [`WorkflowContext::state`].
    #[run]
    pub(crate) async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: WorkflowInput,
    ) -> WorkflowResult<DurableRunOutcome> {
        let config = ctx.state(|w| Arc::clone(&w.config));
        Ok(drive(&*ctx, config, input).await)
    }
}

/// Run the driver loop, racing it against the run-deadline timer and workflow
/// cancellation. Always returns a total outcome.
async fn drive(
    ctx: &WorkflowContext<DurableAgentWorkflow>,
    config: Arc<WorkflowActivityConfig>,
    input: WorkflowInput,
) -> DurableRunOutcome {
    let agent_name = input.agent_name.clone();
    let timeout_ms = input.timeout_ms;
    let parallel_limit = input.config.parallel_tool_call_limit;
    let ctx_seed = input.ctx_seed.clone();

    // Single keyed lookup — deterministic, not a `HashMap` iteration.
    let plan = match config.plans.get(&agent_name) {
        Some(plan) => plan.clone(),
        None => return unknown_agent_outcome(&agent_name),
    };

    let mut driver = DurableDriver::new(input, plan);

    // Race the whole effect loop against (a) the durable run-deadline timer
    // and (b) cooperative cancellation. The `select!` block owns the borrow of
    // `driver` (via `effects`); it is released at the block's end, before
    // `driver.interrupt(..)` consumes the driver on an interruption. A natural
    // finish `return`s the outcome directly from inside the loop.
    let interrupt = {
        let effects = run_effects(
            ctx,
            &config,
            &agent_name,
            &ctx_seed,
            parallel_limit,
            &mut driver,
        )
        .fuse();
        let deadline = run_deadline(ctx, timeout_ms).fuse();
        let cancelled = ctx.cancelled();
        futures_util::pin_mut!(effects, deadline, cancelled);

        temporalio_sdk::workflows::select! {
            outcome = effects => return outcome,
            _ = deadline => RunInterrupt::TimedOut,
            _ = cancelled => RunInterrupt::Cancelled,
        }
    };

    driver.interrupt(interrupt)
}

/// The run-deadline future: a durable timer of `timeout_ms`, or a never-
/// resolving future when no deadline is configured.
async fn run_deadline(ctx: &WorkflowContext<DurableAgentWorkflow>, timeout_ms: Option<u64>) {
    match timeout_ms {
        Some(ms) => {
            let _ = ctx.timer(Duration::from_millis(ms)).await;
        }
        None => futures_util::future::pending::<()>().await,
    }
}

/// Execute driver effects one activity at a time until the driver reports a
/// terminal outcome.
async fn run_effects(
    ctx: &WorkflowContext<DurableAgentWorkflow>,
    config: &WorkflowActivityConfig,
    agent_name: &str,
    ctx_seed: &Option<serde_json::Value>,
    parallel_limit: Option<usize>,
    driver: &mut DurableDriver,
) -> DurableRunOutcome {
    loop {
        match driver.next_effect() {
            DriverEffect::RenderInstructions => {
                match ctx
                    .start_activity(
                        AgentActivities::render_instructions,
                        RenderInstructionsArgs {
                            agent_name: agent_name.to_owned(),
                            ctx_seed: ctx_seed.clone(),
                        },
                        config.instructions_activity_opts.clone(),
                    )
                    .await
                {
                    Ok(system_text) => driver.apply_instructions(system_text),
                    // An infra-level failure of the (otherwise deterministic)
                    // render step is terminal — surface it like a model
                    // failure so the run finalizes with events-so-far.
                    Err(err) => driver.apply_model_failure(extract_error_kind(&err)),
                }
            }
            DriverEffect::CallModel(request) => {
                match ctx
                    .start_activity(
                        AgentActivities::call_model,
                        CallModelArgs {
                            agent_name: agent_name.to_owned(),
                            request,
                        },
                        config.model_activity_opts.clone(),
                    )
                    .await
                {
                    Ok(turn) => driver.apply_model(turn),
                    // ADR-10: `call_model` marks model errors non-retryable and
                    // carries an `ErrorKindPayload` JSON in the failure — parse
                    // it back and feed the terminal failure to the driver.
                    Err(err) => driver.apply_model_failure(extract_error_kind(&err)),
                }
            }
            DriverEffect::ExecuteTools(calls) => {
                let outcomes =
                    execute_tools(ctx, config, agent_name, ctx_seed, parallel_limit, calls).await;
                driver.apply_tools(outcomes);
            }
            DriverEffect::Finished(outcome) => return outcome,
        }
    }
}

/// Execute one turn's tool calls, started concurrently and chunked by
/// `parallel_limit`, returning the outcomes **in original call order** (the
/// driver reassembles tool results by position, not by completion order).
async fn execute_tools(
    ctx: &WorkflowContext<DurableAgentWorkflow>,
    config: &WorkflowActivityConfig,
    agent_name: &str,
    ctx_seed: &Option<serde_json::Value>,
    parallel_limit: Option<usize>,
    calls: Vec<ToolCallRequest>,
) -> Vec<ToolCallOutcome> {
    // `None`/`0` ⇒ no concurrency cap: one chunk of every call.
    let chunk_size = parallel_limit
        .filter(|&n| n > 0)
        .unwrap_or_else(|| calls.len().max(1));

    let mut outcomes = Vec::with_capacity(calls.len());
    for chunk in calls.chunks(chunk_size) {
        let started = chunk.iter().map(|call| {
            let call = call.clone();
            let call_id = call.call_id.clone();
            let opts = config.tool_activity_opts.clone();
            let agent_name = agent_name.to_owned();
            let ctx_seed_cloned = ctx_seed.clone();
            async move {
                match ctx
                    .start_activity(
                        AgentActivities::invoke_tool,
                        InvokeToolArgs {
                            agent_name,
                            call,
                            ctx_seed: ctx_seed_cloned,
                        },
                        opts,
                    )
                    .await
                {
                    Ok(outcome) => outcome,
                    // Tool-level errors are already `Ok(ToolCallOutcome{ result:
                    // Err(..) })`. Reaching an `Err` here means the activity
                    // itself failed at the infra level (e.g. exhausted retries,
                    // unregistered agent). Fold it into a tool-error outcome so
                    // the loop stays total and the model sees a tool failure —
                    // Task 9 validates the live crash-resume path.
                    Err(err) => ToolCallOutcome {
                        call_id,
                        result: Err(format!(
                            "tool activity failed: {}",
                            activity_failure_message(&err)
                        )),
                    },
                }
            }
        });
        // `join_all` preserves input order in its result `Vec` (deterministic).
        let chunk_outcomes = temporalio_sdk::workflows::join_all(started).await;
        outcomes.extend(chunk_outcomes);
    }
    outcomes
}

/// Reconstruct the [`ErrorKindPayload`] a `call_model` activity failure carries
/// (the non-retryable [`ErrorKindPayload`] JSON from
/// [`crate::activities`]'s `error_kind_to_activity_error`), degrading to
/// [`ErrorKindPayload::Model`] with the raw failure message when the payload
/// cannot be parsed (e.g. an infra failure that never went through that path).
fn extract_error_kind(err: &ActivityExecutionError) -> ErrorKindPayload {
    let message = activity_failure_message(err);
    serde_json::from_str::<ErrorKindPayload>(&message)
        .unwrap_or(ErrorKindPayload::Model { message })
}

/// Best-effort extraction of the human-readable message from an activity
/// failure: prefer the application-failure cause's message (which carries the
/// `ErrorKindPayload` JSON), then the top-level failure message, then the
/// error's `Display`.
fn activity_failure_message(err: &ActivityExecutionError) -> String {
    if let Some(cause) = err.cause() {
        return cause.failure().message.clone();
    }
    if let Some(failure) = err.failure() {
        return failure.message.clone();
    }
    err.to_string()
}

/// Terminal outcome for a run whose agent name is not registered on this
/// worker. Deterministic (no activity, no time) — the client cannot
/// pre-validate the worker's registry, so this surfaces at run start.
fn unknown_agent_outcome(agent_name: &str) -> DurableRunOutcome {
    DurableRunOutcome {
        status: RunStatusPayload::AgentFailed(ErrorKindPayload::Other {
            message: format!("no agent named '{agent_name}' is registered on this worker"),
        }),
        events: Vec::new(),
        usage: TokenUsage::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cfg(
        initial_interval: Option<Duration>,
        backoff_coefficient: Option<f64>,
        maximum_interval: Option<Duration>,
        maximum_attempts: Option<u32>,
        non_retryable_error_types: Vec<String>,
    ) -> RetryPolicyConfig {
        RetryPolicyConfig {
            initial_interval,
            backoff_coefficient,
            maximum_interval,
            maximum_attempts,
            non_retryable_error_types,
        }
    }

    #[test]
    fn empty_retry_config_maps_to_none() {
        assert!(to_proto_retry_policy(&RetryPolicyConfig::default()).is_none());
    }

    #[test]
    fn populated_retry_config_maps_to_proto_fields() {
        let policy = to_proto_retry_policy(&cfg(
            Some(Duration::from_millis(250)),
            Some(2.0),
            Some(Duration::from_secs(30)),
            Some(5),
            vec!["MyError".to_owned()],
        ))
        .expect("a populated config yields a proto policy");

        assert_eq!(policy.backoff_coefficient, 2.0);
        assert_eq!(policy.maximum_attempts, 5);
        assert_eq!(policy.non_retryable_error_types, vec!["MyError".to_owned()]);
        let initial = policy.initial_interval.expect("initial_interval set");
        assert_eq!(initial.seconds, 0);
        assert_eq!(initial.nanos, 250_000_000);
        let maximum = policy.maximum_interval.expect("maximum_interval set");
        assert_eq!(maximum.seconds, 30);
    }

    #[test]
    fn unknown_agent_outcome_is_agent_failed() {
        let outcome = unknown_agent_outcome("missing");
        match outcome.status {
            RunStatusPayload::AgentFailed(ErrorKindPayload::Other { message }) => {
                assert!(message.contains("missing"));
            }
            other => panic!("expected AgentFailed(Other), got {other:?}"),
        }
        assert!(outcome.events.is_empty());
    }

    #[test]
    fn build_activity_config_attaches_retry_policies() {
        let model_retry = cfg(None, None, None, Some(3), Vec::new());
        let tool_retry = cfg(None, None, None, Some(1), Vec::new());
        let config = build_activity_config(
            HashMap::new(),
            &model_retry,
            &tool_retry,
            &ActivityTimeouts::default(),
            None,
        );

        assert_eq!(
            config
                .model_activity_opts
                .retry_policy
                .as_ref()
                .expect("model retry policy present")
                .maximum_attempts,
            3
        );
        assert_eq!(
            config
                .tool_activity_opts
                .retry_policy
                .as_ref()
                .expect("tool retry policy present")
                .maximum_attempts,
            1
        );
        // The instructions activity gets no explicit retry policy (server default).
        assert!(config.instructions_activity_opts.retry_policy.is_none());
    }

    #[test]
    fn build_activity_config_applies_timeout_overrides() {
        use temporalio_sdk::ActivityCloseTimeouts;

        let timeouts = ActivityTimeouts {
            instructions: Duration::from_secs(30),
            model: Duration::from_secs(10),
            tool: Duration::from_secs(5),
        };
        let config = build_activity_config(
            HashMap::new(),
            &RetryPolicyConfig::default(),
            &RetryPolicyConfig::default(),
            &timeouts,
            None,
        );

        assert_eq!(
            config.tool_activity_opts.close_timeouts,
            ActivityCloseTimeouts::StartToClose(Duration::from_secs(5))
        );
        assert_eq!(
            config.model_activity_opts.close_timeouts,
            ActivityCloseTimeouts::StartToClose(Duration::from_secs(10))
        );
        assert_eq!(
            config.instructions_activity_opts.close_timeouts,
            ActivityCloseTimeouts::StartToClose(Duration::from_secs(30))
        );
    }

    #[test]
    fn build_activity_config_sets_heartbeat_timeout_on_model_and_tool_only() {
        let config = build_activity_config(
            HashMap::new(),
            &RetryPolicyConfig::default(),
            &RetryPolicyConfig::default(),
            &ActivityTimeouts::default(),
            Some(Duration::from_secs(4)),
        );
        assert_eq!(
            config.model_activity_opts.heartbeat_timeout,
            Some(Duration::from_secs(4))
        );
        assert_eq!(
            config.tool_activity_opts.heartbeat_timeout,
            Some(Duration::from_secs(4))
        );
        assert_eq!(config.instructions_activity_opts.heartbeat_timeout, None);
    }

    #[test]
    fn build_activity_config_no_heartbeat_when_none() {
        let config = build_activity_config(
            HashMap::new(),
            &RetryPolicyConfig::default(),
            &RetryPolicyConfig::default(),
            &ActivityTimeouts::default(),
            None,
        );
        assert_eq!(config.model_activity_opts.heartbeat_timeout, None);
        assert_eq!(config.tool_activity_opts.heartbeat_timeout, None);
    }
}
