//! Temporal worker construction.
//!
//! [`crate::worker::TemporalAgentWorker`] wraps a `temporalio_sdk::Worker`
//! configured to serve one or more registered
//! [`paigasus_helikon_core::LlmAgent`]s' activities on a task queue. Build
//! one via [`crate::worker::TemporalAgentWorker::builder`].
//!
//! # Workflow registration (SMA-332 Task 8)
//!
//! [`crate::worker::TemporalAgentWorkerBuilder::build`] registers the durable
//! agent-loop workflow (the crate-internal `workflow` module) via
//! `register_workflow_with_factory`, because `build()` is the last point at
//! which `Ctx` is still in scope. The workflow itself is `Ctx`-free (it plans
//! against a `Ctx`-free `HashMap<String, AgentPlan>` projected from the
//! registry), so the factory closes over an
//! `Arc<crate::workflow::WorkflowActivityConfig>` — never a serialized plan
//! and never a `Ctx`-generic value. The constructed `temporalio_sdk::Worker`
//! type-erases every registered activity/workflow, so
//! [`crate::worker::TemporalAgentWorker`] carries no `Ctx` parameter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use paigasus_helikon_core::{
    AllowRule, ApprovalHandler, DenyRule, GuardRule, LlmAgent, Model, PermissionMode,
    PermissionPolicy, RunContext, ToolDef,
};

use crate::activities::{self, DurableAgentDef};
use crate::driver::AgentPlan;

/// Worker-side security posture applied to every `RunContext` the durable
/// activities fabricate. `Default` reproduces the crate's v0 fixed defaults
/// (`PermissionMode::Default`, built-in destructive guards on, output redaction
/// on, no custom rules / policy / approval handler / extra secrets).
pub struct WorkerPosture<Ctx: Send + Sync + 'static> {
    permission_mode: PermissionMode,
    deny_rules: Vec<DenyRule>,
    allow_rules: Vec<AllowRule>,
    guard_rules: Vec<GuardRule>,
    permission_policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,
    approval_handler: Option<Arc<dyn ApprovalHandler>>,
    default_guards: bool,
    redact_output: bool,
    extra_secrets: Vec<String>,
}

impl<Ctx: Send + Sync + 'static> Default for WorkerPosture<Ctx> {
    fn default() -> Self {
        Self {
            permission_mode: PermissionMode::default(),
            deny_rules: Vec::new(),
            allow_rules: Vec::new(),
            guard_rules: Vec::new(),
            permission_policy: None,
            approval_handler: None,
            default_guards: true,
            redact_output: true,
            extra_secrets: Vec::new(),
        }
    }
}

impl<Ctx: Send + Sync + 'static> WorkerPosture<Ctx> {
    /// Set the permission mode the activities enforce (tighten-only from `Default`).
    pub fn with_permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }
    /// Install deny rules (evaluated before mode; override even `Bypass`).
    pub fn with_deny_rules(mut self, rules: Vec<DenyRule>) -> Self {
        self.deny_rules = rules;
        self
    }
    /// Install allow rules (positive short-circuit in any mode).
    pub fn with_allow_rules(mut self, rules: Vec<AllowRule>) -> Self {
        self.allow_rules = rules;
        self
    }
    /// Install user guard rules (evaluated before mode; may ask or deny).
    pub fn with_guard_rules(mut self, rules: Vec<GuardRule>) -> Self {
        self.guard_rules = rules;
        self
    }
    /// Install the `canUseTool` permission policy. It can read the per-run
    /// (seeded) `RunContext::user_ctx` for request-scoped decisions.
    pub fn with_permission_policy(mut self, policy: Arc<dyn PermissionPolicy<Ctx>>) -> Self {
        self.permission_policy = Some(policy);
        self
    }
    /// Install the approval handler that resolves `AskUser` / guard `Ask` decisions.
    pub fn with_approval_handler(mut self, handler: Arc<dyn ApprovalHandler>) -> Self {
        self.approval_handler = Some(handler);
        self
    }
    /// Disable the always-on built-in destructive guard set (power-user opt-out).
    pub fn without_default_guards(mut self) -> Self {
        self.default_guards = false;
        self
    }
    /// Disable automatic secret redaction of tool output. Note: unredacted tool
    /// output then enters permanent Temporal history.
    pub fn without_output_redaction(mut self) -> Self {
        self.redact_output = false;
        self
    }
    /// Add extra secret values to redact from tool output, beyond the env set.
    pub fn with_extra_secrets(mut self, secrets: Vec<String>) -> Self {
        self.extra_secrets = secrets;
        self
    }

    /// Apply this posture onto a freshly fabricated `RunContext`.
    ///
    /// NB: this is the **fifth** hand-copy of core's nine-field permission
    /// bundle (see `RunContext`'s fields and core's `pub(crate) PermissionFields`).
    /// `PermissionFields` cannot be reused across the crate boundary. If core
    /// gains a tenth posture knob, add it here too — the default-equivalence
    /// unit test enumerates every field to catch the omission.
    pub(crate) fn apply(&self, ctx: RunContext<Ctx>) -> RunContext<Ctx> {
        let mut ctx = ctx
            .with_permission_mode(self.permission_mode)
            .with_deny_rules(self.deny_rules.clone())
            .with_allow_rules(self.allow_rules.clone())
            .with_guard_rules(self.guard_rules.clone())
            .with_extra_secrets(self.extra_secrets.clone());
        if let Some(p) = &self.permission_policy {
            ctx = ctx.with_permission_policy(Arc::clone(p));
        }
        if let Some(h) = &self.approval_handler {
            ctx = ctx.with_approval_handler(Arc::clone(h));
        }
        if !self.default_guards {
            ctx = ctx.without_default_guards();
        }
        if !self.redact_output {
            ctx = ctx.without_output_redaction();
        }
        ctx
    }
}

/// Retry-policy knobs applied to a durable agent's `call_model`/`invoke_tool`
/// activity invocations.
///
/// `None`/empty fields mean "use the Temporal server's own defaults" — this
/// type intentionally does not carry Temporal's raw
/// `temporal.api.common.v1::RetryPolicy` proto shape; SMA-332 Task 8, which
/// is the first consumer that actually builds an `ActivityOptions` to attach
/// a retry policy to, converts a [`RetryPolicyConfig`] into that proto type
/// at the point of use.
#[derive(Debug, Clone, Default)]
pub struct RetryPolicyConfig {
    /// Interval before the first retry.
    pub initial_interval: Option<std::time::Duration>,
    /// Multiplier applied to the previous retry interval. Must be `>= 1.0`
    /// when set.
    pub backoff_coefficient: Option<f64>,
    /// Cap on the backed-off retry interval.
    pub maximum_interval: Option<std::time::Duration>,
    /// Maximum retry attempts. `Some(1)` disables retries; `Some(0)` or
    /// `None` means unlimited (bounded only by activity timeouts).
    pub maximum_attempts: Option<u32>,
    /// Error type names that must not be retried.
    pub non_retryable_error_types: Vec<String>,
}

/// Why a seeded `Ctx` factory rejected a run's seed. Surfaced as a
/// **non-retryable** activity failure so a malformed/hostile seed fails the run
/// fast instead of retry-looping.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct CtxSeedError(String);

impl CtxSeedError {
    pub(crate) fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// The fallible seeded `Ctx` factory slot type, shared by
/// [`TemporalAgentWorkerBuilder`] and `crate::activities::TypedRuntime` —
/// factored into an alias so clippy's `type_complexity` lint doesn't fire on
/// every use site.
pub(crate) type CtxFactory<Ctx> =
    Arc<dyn Fn(Option<serde_json::Value>) -> Result<Ctx, CtxSeedError> + Send + Sync>;

/// Why [`TemporalAgentWorkerBuilder::register`] rejected an agent.
///
/// v0 durably executes only the "plain" `LlmAgent` shape: no lifecycle
/// hooks, no handoffs, no input/output guardrails (spec §5.7). All four are
/// stored-but-not-driven by the ephemeral loop today too (SMA-314), so
/// rejecting them here is a temporary, explicit v0 boundary rather than a
/// silent capability gap.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistrationError {
    /// The agent configures a feature the durable Temporal runtime does not
    /// yet drive. The payload names the feature (`"hooks"`, `"handoffs"`,
    /// `"input_guardrails"`, or `"output_guardrails"`).
    #[error("agent uses a feature unsupported by the durable Temporal runtime: {0}")]
    UnsupportedFeature(&'static str),
    /// Another agent with this name is already registered on this worker.
    #[error("an agent named '{0}' is already registered on this worker")]
    DuplicateAgentName(String),
}

/// Why [`TemporalAgentWorkerBuilder::build`] failed.
#[derive(Debug, thiserror::Error)]
pub enum WorkerBuildError {
    /// [`TemporalAgentWorkerBuilder::task_queue`] was never called.
    #[error("task_queue must be set via TemporalAgentWorkerBuilder::task_queue before build()")]
    MissingTaskQueue,
    /// [`TemporalAgentWorkerBuilder::client`] was never called.
    #[error("a client must be set via TemporalAgentWorkerBuilder::client before build()")]
    MissingClient,
    /// [`TemporalAgentWorkerBuilder::with_ctx`] was never called.
    #[error("a ctx factory must be set via TemporalAgentWorkerBuilder::with_ctx before build()")]
    MissingCtxFactory,
    /// [`TemporalAgentWorkerBuilder::register`] was never called successfully.
    #[error(
        "at least one agent must be registered via TemporalAgentWorkerBuilder::register before build()"
    )]
    NoAgentsRegistered,
    /// The Temporal Core runtime (telemetry + async executor glue) failed to
    /// initialize.
    #[error("failed to initialize the Temporal Core runtime: {0}")]
    Runtime(String),
    /// `temporalio_sdk::Worker::new` failed.
    #[error("failed to construct the Temporal worker: {0}")]
    Worker(String),
}

/// Why [`TemporalAgentWorker::run`] returned early.
#[derive(Debug, thiserror::Error)]
#[error("temporal worker run failed: {0}")]
pub struct WorkerRunError(String);

/// Builder for [`TemporalAgentWorker`]. Construct via
/// [`TemporalAgentWorker::builder`].
pub struct TemporalAgentWorkerBuilder<Ctx: Send + Sync + 'static> {
    task_queue: Option<String>,
    client: Option<temporalio_client::Client>,
    ctx_factory: Option<CtxFactory<Ctx>>,
    registry: HashMap<String, Arc<DurableAgentDef<Ctx>>>,
    model_retry_policy: RetryPolicyConfig,
    tool_retry_policy: RetryPolicyConfig,
    model_start_to_close: Option<Duration>,
    tool_start_to_close: Option<Duration>,
    posture: WorkerPosture<Ctx>,
}

/// A Temporal worker configured to serve one or more durable [`LlmAgent`]s'
/// activities on a task queue.
///
/// Carries no `Ctx` type parameter: by the time [`TemporalAgentWorkerBuilder::build`]
/// returns one, every agent's `Ctx`-generic state has already been
/// type-erased into the wrapped `temporalio_sdk::Worker`'s activity
/// registry (this crate's private `activities` module holds the erasure
/// details).
pub struct TemporalAgentWorker {
    inner: temporalio_sdk::Worker,
    // Kept alive for the worker's lifetime; dropping this while `inner` is
    // still running risks silently losing telemetry/metrics the runtime
    // owns. Never read directly (hence the leading underscore), only held.
    _runtime: temporalio_sdk_core::CoreRuntime,
}

impl TemporalAgentWorker {
    /// Start building a worker for agents sharing the per-run context type
    /// `Ctx`.
    pub fn builder<Ctx: Send + Sync + 'static>() -> TemporalAgentWorkerBuilder<Ctx> {
        TemporalAgentWorkerBuilder {
            task_queue: None,
            client: None,
            ctx_factory: None,
            registry: HashMap::new(),
            model_retry_policy: RetryPolicyConfig::default(),
            tool_retry_policy: RetryPolicyConfig::default(),
            model_start_to_close: None,
            tool_start_to_close: None,
            posture: WorkerPosture::default(),
        }
    }

    /// Serve the task queue until shutdown.
    ///
    /// Polls **both** workflow and activity tasks (`build()` registers the
    /// durable agent-loop workflow and sets `WorkerTaskTypes::all()`): it
    /// drives durable runs started by [`crate::runner::TemporalRunner`] and
    /// executes their activities.
    pub async fn run(self) -> Result<(), WorkerRunError> {
        let mut worker = self;
        worker
            .inner
            .run()
            .await
            .map_err(|e| WorkerRunError(e.to_string()))
    }
}

impl<Ctx: Send + Sync + 'static> TemporalAgentWorkerBuilder<Ctx> {
    /// Set the task queue this worker polls.
    pub fn task_queue(mut self, queue: impl Into<String>) -> Self {
        self.task_queue = Some(queue.into());
        self
    }

    /// Set the connected Temporal client this worker polls with.
    pub fn client(mut self, client: temporalio_client::Client) -> Self {
        self.client = Some(client);
        self
    }

    /// Set the per-activity-invocation `Ctx` factory (seed ignored).
    ///
    /// Called once per activity invocation (`render_instructions`,
    /// `call_model`, `invoke_tool`) to build a fresh [`paigasus_helikon_core::RunContext`]
    /// — mirroring how a fresh `Ctx` typically seeds one ephemeral run.
    pub fn with_ctx(mut self, factory: impl Fn() -> Ctx + Send + Sync + 'static) -> Self {
        self.ctx_factory = Some(Arc::new(move |_seed| Ok(factory())));
        self
    }

    /// Set a seeded `Ctx` factory that reconstitutes the per-run context from the
    /// client's `serde_json::Value` seed (`None` when the client set none).
    ///
    /// **Totality contract:** this closure must never panic and should be cheap —
    /// it runs once per `render_instructions` and per `invoke_tool` invocation. For
    /// authorization-bearing seeds prefer [`Self::try_with_seeded_ctx`] so a bad
    /// seed fails the run loudly instead of defaulting to the wrong identity.
    pub fn with_seeded_ctx(
        mut self,
        factory: impl Fn(Option<serde_json::Value>) -> Ctx + Send + Sync + 'static,
    ) -> Self {
        self.ctx_factory = Some(Arc::new(move |seed| Ok(factory(seed))));
        self
    }

    /// Like [`Self::with_seeded_ctx`], but fallible: a seed the factory rejects
    /// fails the run with a **non-retryable** activity error instead of proceeding
    /// under a default identity.
    pub fn try_with_seeded_ctx<E: std::fmt::Display>(
        mut self,
        factory: impl Fn(Option<serde_json::Value>) -> Result<Ctx, E> + Send + Sync + 'static,
    ) -> Self {
        self.ctx_factory = Some(Arc::new(move |seed| {
            factory(seed).map_err(|e| CtxSeedError::new(e.to_string()))
        }));
        self
    }

    /// Snapshot `agent` into this worker's registry.
    ///
    /// Errors when the agent has hooks, guardrails, or handoffs configured
    /// (v0 fail-fast, spec §5.7) or when another agent with the same name
    /// is already registered.
    pub fn register<M, T>(
        mut self,
        agent: Arc<LlmAgent<Ctx, M, T>>,
    ) -> Result<Self, RegistrationError>
    where
        M: Model + 'static,
        T: Send + Sync + 'static,
    {
        if !agent.hooks.is_empty() {
            return Err(RegistrationError::UnsupportedFeature("hooks"));
        }
        if !agent.handoffs.is_empty() {
            return Err(RegistrationError::UnsupportedFeature("handoffs"));
        }
        if !agent.input_guardrails.is_empty() {
            return Err(RegistrationError::UnsupportedFeature("input_guardrails"));
        }
        if !agent.output_guardrails.is_empty() {
            return Err(RegistrationError::UnsupportedFeature("output_guardrails"));
        }
        if self.registry.contains_key(&agent.name) {
            return Err(RegistrationError::DuplicateAgentName(agent.name.clone()));
        }

        // Mirrors the ephemeral driver's per-invocation snapshot
        // (`agent.rs:695-702`).
        let tool_defs = agent
            .tools
            .iter()
            .map(|t| ToolDef {
                name: t.name().to_owned(),
                description: t.description().to_owned(),
                schema: t.schema().clone(),
            })
            .collect();
        let plan = AgentPlan {
            tool_defs,
            model_settings: agent.model_settings.clone(),
            output: agent.output_type.clone(),
        };
        // `M: Model + 'static` is concrete here, so `Arc<M>` unsizes to
        // `Arc<dyn Model>` — the same upcast the brief's Step 3 describes.
        let model: Arc<dyn Model> = agent.model.clone();
        let def = DurableAgentDef {
            name: agent.name.clone(),
            instructions: Arc::clone(&agent.instructions),
            model,
            tools: agent.tools.clone(),
            plan,
        };
        self.registry.insert(agent.name.clone(), Arc::new(def));
        Ok(self)
    }

    /// Set the retry policy applied to `call_model` activity invocations.
    pub fn model_retry_policy(mut self, p: RetryPolicyConfig) -> Self {
        self.model_retry_policy = p;
        self
    }

    /// Set the retry policy applied to `invoke_tool` activity invocations.
    pub fn tool_retry_policy(mut self, p: RetryPolicyConfig) -> Self {
        self.tool_retry_policy = p;
        self
    }

    /// Override the per-attempt start-to-close timeout for the `call_model`
    /// activity (default 300s).
    ///
    /// This bounds a **single** model-call attempt. Temporal detects a dead
    /// worker only when an in-flight attempt overruns this bound, so a shorter
    /// value shortens the window before a crashed run's model call is
    /// re-dispatched to a healthy worker (subject to the model retry policy).
    pub fn model_start_to_close(mut self, d: Duration) -> Self {
        self.model_start_to_close = Some(d);
        self
    }

    /// Override the per-attempt start-to-close timeout for the `invoke_tool`
    /// activity (default 300s).
    ///
    /// This bounds a **single** tool-call attempt. A shorter value shortens the
    /// window before a crashed run's in-flight tool call is re-dispatched to a
    /// healthy worker (subject to the tool retry policy) — the knob a
    /// crash-resume-sensitive deployment tunes down for a fast-failing tool.
    pub fn tool_start_to_close(mut self, d: Duration) -> Self {
        self.tool_start_to_close = Some(d);
        self
    }

    /// Set the worker-side security posture applied to every fabricated
    /// `RunContext`. Defaults to `WorkerPosture::default()` (v0 fixed defaults).
    pub fn posture(mut self, posture: WorkerPosture<Ctx>) -> Self {
        self.posture = posture;
        self
    }

    /// Assemble the Temporal worker.
    ///
    /// Requires [`Self::task_queue`], [`Self::client`], [`Self::with_ctx`],
    /// and at least one [`Self::register`] call. Fallible parts of this
    /// method (Core runtime initialization, `temporalio_sdk::Worker`
    /// construction) do not require network I/O — the caller-supplied
    /// [`temporalio_client::Client`] is assumed already connected.
    pub fn build(self) -> Result<TemporalAgentWorker, WorkerBuildError> {
        let task_queue = self.task_queue.ok_or(WorkerBuildError::MissingTaskQueue)?;
        let client = self.client.ok_or(WorkerBuildError::MissingClient)?;
        let ctx_factory = self
            .ctx_factory
            .ok_or(WorkerBuildError::MissingCtxFactory)?;
        if self.registry.is_empty() {
            return Err(WorkerBuildError::NoAgentsRegistered);
        }

        tracing::debug!(
            model_retry_policy = ?self.model_retry_policy,
            tool_retry_policy = ?self.tool_retry_policy,
            agents = self.registry.len(),
            "assembling durable-agent Temporal worker",
        );

        // SMA-332 Task 8: the durable workflow is registered here, inside this
        // generic `build()`, because it is the last point at which `Ctx` is in
        // scope. The workflow itself is `Ctx`-free — it plans against a
        // `Ctx`-free `HashMap<String, AgentPlan>` derived from the registry —
        // so the factory closure closes over an `Arc<WorkflowActivityConfig>`
        // (plans + per-activity options), never a serialized plan and never a
        // `Ctx`-generic value. `AgentPlan` must not be serialized (its
        // `OutputType` validator fails closed after a round-trip), so the map
        // is built in-process here and cloned into each workflow instance.
        let agent_registry = Arc::new(self.registry);
        let activities = activities::build_activities(
            Arc::clone(&agent_registry),
            Arc::clone(&ctx_factory),
            self.posture,
        );

        // `Ctx`-free projection of the registry (`name → AgentPlan`). Iterating
        // the registry here (worker-setup side) is fine; the workflow never
        // iterates a map — it does a single keyed `get` by `agent_name`.
        let plans: HashMap<String, AgentPlan> = agent_registry
            .iter()
            .map(|(name, def)| (name.clone(), def.plan.clone()))
            .collect();
        let mut timeouts = crate::workflow::ActivityTimeouts::default();
        if let Some(d) = self.model_start_to_close {
            timeouts.model = d;
        }
        if let Some(d) = self.tool_start_to_close {
            timeouts.tool = d;
        }
        let workflow_config = Arc::new(crate::workflow::build_activity_config(
            plans,
            &self.model_retry_policy,
            &self.tool_retry_policy,
            &timeouts,
        ));

        let telemetry_options = temporalio_common::telemetry::TelemetryOptions::builder().build();
        let runtime_options = temporalio_sdk_core::RuntimeOptions::builder()
            .telemetry_options(telemetry_options)
            .build()
            .map_err(WorkerBuildError::Runtime)?;
        let runtime = temporalio_sdk_core::CoreRuntime::new_assume_tokio(runtime_options)
            .map_err(|e| WorkerBuildError::Runtime(e.to_string()))?;

        // Serve both workflow and activity tasks: this worker now drives the
        // durable `DurableAgentWorkflow` (registered via factory below) in
        // addition to its activities. Registration is on the `WorkerOptions`
        // builder (returns a `Result`, hence the mid-chain `?`).
        let worker_options = temporalio_sdk::WorkerOptions::new(task_queue)
            .task_types(temporalio_common::worker::WorkerTaskTypes::all())
            .register_activities(activities)
            .register_workflow_with_factory::<crate::workflow::DurableAgentWorkflow, _>(move || {
                crate::workflow::DurableAgentWorkflow::new(Arc::clone(&workflow_config))
            })
            .map_err(|e| WorkerBuildError::Worker(e.to_string()))?
            .build();

        let worker = temporalio_sdk::Worker::new(&runtime, client, worker_options)
            .map_err(|e| WorkerBuildError::Worker(e.to_string()))?;

        Ok(TemporalAgentWorker {
            inner: worker,
            _runtime: runtime,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_core::stream::BoxStream;
    use paigasus_helikon_core::{
        Agent, AgentError, AgentEvent, AgentInput, CancellationToken, Guardrail, GuardrailError,
        GuardrailInput, GuardrailVerdict, Hook, HookDecision, HookEvent, LlmAgent,
        ModelCapabilities, ModelError, ModelEvent, RunContext,
    };

    struct StubModel;

    #[async_trait]
    impl Model for StubModel {
        async fn invoke(
            &self,
            _request: paigasus_helikon_core::ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
            Ok(Box::pin(futures_util::stream::empty()))
        }
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities::default()
        }
    }

    struct NoopHook;

    #[async_trait]
    impl Hook<()> for NoopHook {
        async fn on_event(&self, _ctx: &RunContext<()>, _event: &HookEvent) -> HookDecision {
            HookDecision::Allow
        }
    }

    struct NoopGuardrail;

    #[async_trait]
    impl Guardrail<()> for NoopGuardrail {
        async fn check(
            &self,
            _ctx: &RunContext<()>,
            _input: GuardrailInput<'_>,
        ) -> Result<GuardrailVerdict, GuardrailError> {
            Ok(GuardrailVerdict::Pass)
        }
    }

    struct StubHandoffTarget;

    #[async_trait]
    impl Agent<()> for StubHandoffTarget {
        fn name(&self) -> &str {
            "billing"
        }
        fn description(&self) -> &str {
            "billing team"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            Ok(Box::pin(futures_util::stream::empty()))
        }
    }

    fn plain_agent() -> Arc<LlmAgent<(), StubModel, String>> {
        Arc::new(
            LlmAgent::builder::<()>()
                .name("agent-1")
                .model(StubModel)
                .build(),
        )
    }

    /// `TemporalAgentWorkerBuilder` intentionally does not derive `Debug`
    /// (it holds a `temporalio_client::Client` and an
    /// `Arc<dyn Fn() -> Ctx + Send + Sync>`, neither of which do either), so
    /// `Result::expect_err` isn't usable directly on a `register()` failure.
    /// This is the test-only equivalent.
    fn expect_registration_err<Ctx: Send + Sync + 'static>(
        result: Result<TemporalAgentWorkerBuilder<Ctx>, RegistrationError>,
    ) -> RegistrationError {
        match result {
            Ok(_) => panic!("expected register() to fail, but it succeeded"),
            Err(e) => e,
        }
    }

    #[test]
    fn register_rejects_agent_with_hooks() {
        let agent = Arc::new(
            LlmAgent::builder::<()>()
                .name("agent-1")
                .model(StubModel)
                .hook(NoopHook)
                .build(),
        );
        let err = expect_registration_err(TemporalAgentWorker::builder::<()>().register(agent));
        assert_eq!(err, RegistrationError::UnsupportedFeature("hooks"));
    }

    #[test]
    fn register_rejects_agent_with_handoffs() {
        let agent = Arc::new(
            LlmAgent::builder::<()>()
                .name("agent-1")
                .model(StubModel)
                .handoff(StubHandoffTarget)
                .build(),
        );
        let err = expect_registration_err(TemporalAgentWorker::builder::<()>().register(agent));
        assert_eq!(err, RegistrationError::UnsupportedFeature("handoffs"));
    }

    #[test]
    fn register_rejects_agent_with_input_guardrails() {
        let agent = Arc::new(
            LlmAgent::builder::<()>()
                .name("agent-1")
                .model(StubModel)
                .input_guardrail(NoopGuardrail)
                .build(),
        );
        let err = expect_registration_err(TemporalAgentWorker::builder::<()>().register(agent));
        assert_eq!(
            err,
            RegistrationError::UnsupportedFeature("input_guardrails")
        );
    }

    #[test]
    fn register_rejects_agent_with_output_guardrails() {
        let agent = Arc::new(
            LlmAgent::builder::<()>()
                .name("agent-1")
                .model(StubModel)
                .output_guardrail(NoopGuardrail)
                .build(),
        );
        let err = expect_registration_err(TemporalAgentWorker::builder::<()>().register(agent));
        assert_eq!(
            err,
            RegistrationError::UnsupportedFeature("output_guardrails")
        );
    }

    #[test]
    fn register_rejects_duplicate_agent_name() {
        let builder = TemporalAgentWorker::builder::<()>()
            .register(plain_agent())
            .expect("first registration succeeds");
        let err = expect_registration_err(builder.register(plain_agent()));
        assert_eq!(
            err,
            RegistrationError::DuplicateAgentName("agent-1".to_owned())
        );
    }

    #[test]
    fn register_accepts_plain_agent_and_snapshots_tool_defs() {
        struct NoopTool;
        #[async_trait]
        impl paigasus_helikon_core::Tool<()> for NoopTool {
            fn name(&self) -> &str {
                "noop"
            }
            fn description(&self) -> &str {
                "does nothing"
            }
            fn schema(&self) -> &serde_json::Value {
                static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
                SCHEMA.get_or_init(|| serde_json::json!({ "type": "object" }))
            }
            async fn invoke(
                &self,
                _ctx: &paigasus_helikon_core::ToolContext<()>,
                _args: serde_json::Value,
            ) -> Result<paigasus_helikon_core::ToolOutput, paigasus_helikon_core::ToolError>
            {
                Ok(paigasus_helikon_core::ToolOutput::new(
                    serde_json::json!({}),
                ))
            }
        }

        let agent = Arc::new(
            LlmAgent::builder::<()>()
                .name("agent-1")
                .model(StubModel)
                .tool(NoopTool)
                .build(),
        );

        let builder = TemporalAgentWorker::builder::<()>()
            .register(agent)
            .expect("plain agent must be accepted");

        let def = builder
            .registry
            .get("agent-1")
            .expect("registered agent must be present under its name");
        assert_eq!(def.name, "agent-1");
        assert_eq!(def.plan.tool_defs.len(), 1);
        assert_eq!(def.plan.tool_defs[0].name, "noop");
        assert_eq!(def.tools.len(), 1);
    }

    #[test]
    fn worker_posture_default_matches_ephemeral_defaults() {
        use paigasus_helikon_core::RunContext;
        let ctx = WorkerPosture::<()>::default().apply(RunContext::ephemeral(()));
        let bare: RunContext<()> = RunContext::ephemeral(());
        assert_eq!(ctx.permission_mode(), bare.permission_mode());
        assert!(ctx.default_guards());
        assert!(ctx.redact_output());
        assert!(ctx.deny_rules().is_empty());
        assert!(ctx.allow_rules().is_empty());
        assert!(ctx.guard_rules().is_empty());
        assert!(ctx.extra_secrets().is_empty());
        assert!(ctx.permission_policy().is_none());
        assert!(ctx.approval_handler().is_none());
    }

    #[test]
    fn worker_posture_applies_each_knob() {
        use paigasus_helikon_core::{DenyRule, PermissionMode, RunContext};
        let posture = WorkerPosture::<()>::default()
            .with_permission_mode(PermissionMode::Plan)
            .with_deny_rules(vec![DenyRule::tool("Bash")])
            .with_extra_secrets(vec!["sk-123".to_owned()])
            .without_default_guards()
            .without_output_redaction();
        let ctx = posture.apply(RunContext::ephemeral(()));
        assert_eq!(ctx.permission_mode(), PermissionMode::Plan);
        assert_eq!(ctx.deny_rules().len(), 1);
        assert_eq!(ctx.extra_secrets(), ["sk-123".to_owned()]);
        assert!(!ctx.default_guards());
        assert!(!ctx.redact_output());
    }
}
