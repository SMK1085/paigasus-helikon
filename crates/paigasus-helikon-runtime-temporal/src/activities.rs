//! Thin Temporal activity layer over the SDK-free `*_inner` functions.
//!
//! [`call_model_inner`], [`invoke_tool_inner`], and [`render_instructions_inner`]
//! are unit-testable without a Temporal worker: they take a [`DurableAgentDef`]
//! (or its parts) directly and contain no `temporalio-*` types. The
//! `#[temporalio_macros::activities]` impl block at the bottom of this file is
//! the thin Temporal-facing wrapper Task 8's workflow calls via
//! `WorkflowContext::start_activity`.
//!
//! # Why the wrapper is `Ctx`-erased
//!
//! `temporalio_macros::activities` copies the annotated impl block's `Self`
//! type verbatim into fresh, non-generic `ActivityDefinition`/
//! `ActivityImplementer` impls — confirmed by reading
//! `temporalio-macros-0.5.0/src/activities_definitions.rs`: its codegen never
//! threads `self.impl_block.generics` into the generated code. A literal
//! `impl<Ctx> AgentActivities<Ctx> { #[activities] ... }` therefore does not
//! compile (`Ctx` is unbound in the generated impls). [`DurableAgentRuntime`]
//! erases `Ctx` behind a trait object so the `#[activities]` impl block
//! itself (on [`AgentActivities`]) can stay non-generic; [`TypedRuntime`] is
//! the concrete, `Ctx`-generic implementer built by
//! [`crate::worker::TemporalAgentWorkerBuilder::build`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::{
    execute_tool_call, CancellationToken, Instructions, Model, ModelRequest, ModelTurnAccumulator,
    RunContext, Tool, ToolCallOutcome, ToolCallRequest,
};
use temporalio_sdk::activities::{ActivityContext, ActivityError};
use temporalio_sdk::ApplicationFailure;

use crate::activity_input::{
    CallModelArgs, CallModelInput, InvokeToolArgs, InvokeToolInput, RenderInstructionsArgs,
    RenderInstructionsInput,
};
use crate::driver::AgentPlan;
use crate::error::ErrorKindPayload;
use crate::payloads::ModelTurnResult;

/// Static per-agent definition snapshot a Temporal worker plans and executes
/// against.
///
/// Built once per agent by
/// [`crate::worker::TemporalAgentWorkerBuilder::register`] and looked up by
/// name from the worker's process-local registry — **never serialized**.
/// `plan`'s `output` field (an `OutputType`) carries a `#[serde(skip)]`
/// validator that fails closed after any deserialize round-trip (SMA-332
/// Task 6), so `DurableAgentDef` must stay in-process for the lifetime of
/// the worker rather than crossing any wire boundary.
pub(crate) struct DurableAgentDef<Ctx> {
    /// Agent name — the registry key and the [`ModelTurnAccumulator`]
    /// attribution for reassembled turns.
    pub name: String,
    /// System-prompt renderer.
    pub instructions: Arc<dyn Instructions<Ctx>>,
    /// The model this agent calls each turn.
    pub model: Arc<dyn Model>,
    /// Tools available to this agent.
    pub tools: Vec<Arc<dyn Tool<Ctx>>>,
    /// Snapshot of tool/model/output configuration the durable driver plans
    /// against.
    ///
    /// Written by [`crate::worker::TemporalAgentWorkerBuilder::register`] and
    /// read by [`crate::worker::TemporalAgentWorkerBuilder::build`], which
    /// clones it (`def.plan.clone()`) into the `Ctx`-free `AgentPlan` map the
    /// durable workflow plans against via `DurableDriver::new`.
    pub plan: AgentPlan,
}

/// Invoke `model`, draining its event stream into one aggregated
/// [`ModelTurnResult`].
///
/// SDK-free: unit-testable without a Temporal worker. A
/// [`paigasus_helikon_core::ModelError`] from either [`Model::invoke`]
/// itself or a stream-level event degrades to [`ErrorKindPayload::Model`]; a
/// successfully-drained stream that fails to reassemble (invalid
/// tool-call-argument JSON) degrades to [`ErrorKindPayload::Other`].
pub(crate) async fn call_model_inner(
    model: &dyn Model,
    agent_name: &str,
    request: ModelRequest,
    cancel: CancellationToken,
) -> Result<ModelTurnResult, ErrorKindPayload> {
    use futures_util::StreamExt;

    let mut stream = model
        .invoke(request, cancel)
        .await
        .map_err(|e| ErrorKindPayload::Model {
            message: e.to_string(),
        })?;

    let mut acc = ModelTurnAccumulator::new(agent_name);
    while let Some(event) = stream.next().await {
        match event {
            Ok(ev) => acc.observe(&ev),
            Err(e) => {
                return Err(ErrorKindPayload::Model {
                    message: e.to_string(),
                });
            }
        }
    }

    acc.finish()
        .map(ModelTurnResult)
        .map_err(|message| ErrorKindPayload::Other { message })
}

/// Execute one tool call through the shared authorize -> invoke -> redact
/// pipeline ([`execute_tool_call`]) and return its outcome.
///
/// SDK-free. An unknown tool name or a denied/failed call is folded into
/// `outcome.result`'s `Err` string per [`execute_tool_call`]'s contract —
/// this function never panics and never returns an `Err` of its own. The
/// `AgentEvent::PermissionDenied` event `execute_tool_call` may also produce
/// is intentionally dropped here: v0's durable event log
/// (`DurableRunOutcome::events`) does not carry per-tool permission events;
/// the denial reason is already folded into the outcome's `Err` string.
pub(crate) async fn invoke_tool_inner<Ctx: Send + Sync + 'static>(
    def: &DurableAgentDef<Ctx>,
    run_ctx: &RunContext<Ctx>,
    call: ToolCallRequest,
) -> ToolCallOutcome {
    let tool_ctx = run_ctx.to_tool_context();
    execute_tool_call(&def.tools, run_ctx, &tool_ctx, &call)
        .await
        .0
}

/// Render this agent's system-prompt text for `run_ctx`.
///
/// SDK-free.
pub(crate) async fn render_instructions_inner<Ctx: Send + Sync + 'static>(
    def: &DurableAgentDef<Ctx>,
    run_ctx: &RunContext<Ctx>,
) -> String {
    def.instructions.render(run_ctx)
}

// ---------------------------------------------------------------------------
// Temporal activity layer (Step 4): thin, Ctx-erased wrapper over the inner
// functions above.
// ---------------------------------------------------------------------------

/// `Ctx`-erased seam behind the (necessarily non-generic) `#[activities]`
/// impl block on [`AgentActivities`]. See the module docs for why this
/// erasure is required.
#[async_trait]
trait DurableAgentRuntime: Send + Sync {
    /// See [`render_instructions_inner`].
    async fn render_instructions(
        &self,
        agent_name: &str,
        ctx_seed: Option<serde_json::Value>,
        cancel: CancellationToken,
    ) -> Result<String, ActivityError>;
    /// See [`call_model_inner`].
    async fn call_model(
        &self,
        agent_name: &str,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelTurnResult, ActivityError>;
    /// See [`invoke_tool_inner`].
    async fn invoke_tool(
        &self,
        agent_name: &str,
        call: ToolCallRequest,
        ctx_seed: Option<serde_json::Value>,
        cancel: CancellationToken,
    ) -> Result<ToolCallOutcome, ActivityError>;
}

/// The `Ctx`-generic [`DurableAgentRuntime`] implementer: the process-local
/// registry of every agent this worker was built with, plus the per-run
/// `Ctx` factory.
struct TypedRuntime<Ctx: Send + Sync + 'static> {
    registry: Arc<HashMap<String, Arc<DurableAgentDef<Ctx>>>>,
    ctx_factory: crate::worker::CtxFactory<Ctx>,
    posture: crate::worker::WorkerPosture<Ctx>,
}

impl<Ctx: Send + Sync + 'static> TypedRuntime<Ctx> {
    /// Look up the named agent, or a non-retryable [`ActivityError`] if no
    /// such agent is registered on this worker (a configuration error, not
    /// a transient one — retrying can't fix it).
    fn resolve(&self, agent_name: &str) -> Result<Arc<DurableAgentDef<Ctx>>, ActivityError> {
        self.registry.get(agent_name).cloned().ok_or_else(|| {
            ActivityError::application(ApplicationFailure::non_retryable(format!(
                "no agent named '{agent_name}' is registered on this worker"
            )))
        })
    }

    /// A fresh ephemeral [`RunContext`] (in-memory session, no hooks) for one
    /// activity invocation, built from `seed` and wired to `cancel`.
    ///
    /// A seed the `ctx_factory` rejects becomes a **non-retryable**
    /// [`ActivityError`] (the BLOCKER fix: a hostile/malformed seed must fail
    /// the run fast rather than retry-looping forever).
    fn run_context(
        &self,
        seed: Option<serde_json::Value>,
        cancel: CancellationToken,
    ) -> Result<RunContext<Ctx>, ActivityError> {
        let user_ctx = (self.ctx_factory)(seed).map_err(|e| {
            ActivityError::application(ApplicationFailure::non_retryable(format!(
                "ctx seed rejected: {e}"
            )))
        })?;
        let ctx = RunContext::ephemeral(user_ctx).with_cancel(cancel);
        Ok(self.posture.apply(ctx))
    }
}

#[async_trait]
impl<Ctx: Send + Sync + 'static> DurableAgentRuntime for TypedRuntime<Ctx> {
    async fn render_instructions(
        &self,
        agent_name: &str,
        ctx_seed: Option<serde_json::Value>,
        cancel: CancellationToken,
    ) -> Result<String, ActivityError> {
        let def = self.resolve(agent_name)?;
        let run_ctx = self.run_context(ctx_seed, cancel)?;
        Ok(render_instructions_inner(&def, &run_ctx).await)
    }

    async fn call_model(
        &self,
        agent_name: &str,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelTurnResult, ActivityError> {
        let def = self.resolve(agent_name)?;
        call_model_inner(def.model.as_ref(), &def.name, request, cancel)
            .await
            .map_err(error_kind_to_activity_error)
    }

    async fn invoke_tool(
        &self,
        agent_name: &str,
        call: ToolCallRequest,
        ctx_seed: Option<serde_json::Value>,
        cancel: CancellationToken,
    ) -> Result<ToolCallOutcome, ActivityError> {
        let def = self.resolve(agent_name)?;
        let run_ctx = self.run_context(ctx_seed, cancel)?;
        Ok(invoke_tool_inner(&def, &run_ctx, call).await)
    }
}

/// Fold a non-retryable [`ErrorKindPayload`] into an [`ActivityError`],
/// carrying `serde_json::to_string(&kind)` as the failure's message/details
/// per ADR-10 (no silent auto-retry inside the loop) — a model failure the
/// workflow's `DurableDriver` should observe as terminal, not something
/// Temporal's own activity-retry machinery should mask by retrying
/// transparently.
fn error_kind_to_activity_error(kind: ErrorKindPayload) -> ActivityError {
    let json = serde_json::to_string(&kind).unwrap_or_else(|e| {
        format!(r#"{{"Other":{{"message":"failed to serialize ErrorKindPayload: {e}"}}}}"#)
    });
    ActivityError::application(ApplicationFailure::non_retryable(json))
}

/// Generic cancellation/heartbeat race, decoupled from `ActivityContext` so it
/// is unit-testable. Polls `work` and `cancelled` before the heartbeat tick
/// (`biased`). On cancellation it runs `on_cancel` then **awaits `work` to
/// completion** (never drops it — no detached task leak). When
/// `heartbeat_interval` is `Some`, `on_heartbeat` fires each tick until `work`
/// completes.
async fn race_loop<T>(
    work: impl std::future::Future<Output = T>,
    cancelled: impl std::future::Future<Output = ()>,
    on_cancel: impl FnOnce(),
    heartbeat_interval: Option<Duration>,
    mut on_heartbeat: impl FnMut(),
) -> T {
    tokio::pin!(work, cancelled);
    let mut ticker = heartbeat_interval.map(tokio::time::interval);
    loop {
        tokio::select! {
            biased;
            result = &mut work => return result,
            () = &mut cancelled => {
                // Stop heartbeating during wind-down: the workflow's cancellation
                // branch is already tearing this attempt down, so a heartbeat
                // timeout here is moot. Still await `work` to completion so no
                // detached task leaks.
                on_cancel();
                return work.await;
            }
            _ = async {
                match ticker.as_mut() {
                    Some(t) => { t.tick().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {
                on_heartbeat();
            }
        }
    }
}

/// Race `work` against the activity's cancellation signal, emitting liveness
/// heartbeats every `heartbeat` while it runs (when configured).
///
/// If the activity is cancelled first, propagate that into `cancel` (so the
/// in-flight [`Model`]/[`Tool`] call can wind down per its own cancellation
/// contract) and then await `work` to completion, so callers still get a
/// coherent result instead of a dropped future — and so this never leaks a
/// detached task waiting on a cancellation signal that may never fire.
async fn race_with_activity_cancellation<T>(
    activity_ctx: &ActivityContext,
    cancel: CancellationToken,
    heartbeat: Option<Duration>,
    work: impl std::future::Future<Output = T>,
) -> T {
    race_loop(
        work,
        activity_ctx.cancelled(),
        || cancel.cancel(),
        heartbeat,
        || activity_ctx.record_heartbeat(Vec::new()),
    )
    .await
}

/// The `Ctx`-erased, non-generic activities struct registered on the
/// worker's task queue. See the module docs for why it holds a trait object
/// rather than a `Ctx`-generic field directly.
pub(crate) struct AgentActivities {
    runtime: Arc<dyn DurableAgentRuntime>,
    /// Liveness heartbeat interval for the `call_model`/`invoke_tool`
    /// activities (`None` disables heartbeating). Set via
    /// [`crate::worker::TemporalAgentWorkerBuilder::heartbeat_interval`].
    heartbeat_interval: Option<Duration>,
}

/// Build the [`AgentActivities`] instance a [`crate::worker::TemporalAgentWorker`]
/// registers for its task queue, from a `Ctx`-generic registry + ctx
/// factory. This is the erasure boundary: past this call, `Ctx` no longer
/// appears in any type the Temporal SDK holds onto.
pub(crate) fn build_activities<Ctx: Send + Sync + 'static>(
    registry: Arc<HashMap<String, Arc<DurableAgentDef<Ctx>>>>,
    ctx_factory: crate::worker::CtxFactory<Ctx>,
    posture: crate::worker::WorkerPosture<Ctx>,
    heartbeat_interval: Option<Duration>,
) -> AgentActivities {
    AgentActivities {
        runtime: Arc::new(TypedRuntime {
            registry,
            ctx_factory,
            posture,
        }),
        heartbeat_interval,
    }
}

#[temporalio_macros::activities]
impl AgentActivities {
    /// Render the named agent's system-prompt text.
    ///
    /// Render the named agent's system-prompt text.
    ///
    /// `pub(crate)` so `#[activities]` emits a `pub(crate)` associated const
    /// (`AgentActivities::render_instructions`) — the typed activity marker
    /// SMA-332 Task 8's workflow passes to `WorkflowContext::start_activity`
    /// from the sibling `workflow` module.
    #[activity]
    pub(crate) async fn render_instructions(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: RenderInstructionsInput,
    ) -> Result<String, ActivityError> {
        let RenderInstructionsArgs {
            agent_name,
            ctx_seed,
        } = input.0;
        let cancel = CancellationToken::new();
        race_with_activity_cancellation(
            &ctx,
            cancel.clone(),
            None,
            self.runtime
                .render_instructions(&agent_name, ctx_seed, cancel),
        )
        .await
    }

    /// Invoke the named agent's model for one turn.
    ///
    /// `pub(crate)` for the same marker-visibility reason as
    /// [`AgentActivities::render_instructions`].
    #[activity]
    pub(crate) async fn call_model(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: CallModelInput,
    ) -> Result<ModelTurnResult, ActivityError> {
        let CallModelArgs {
            agent_name,
            request,
        } = input.0;
        let cancel = CancellationToken::new();
        race_with_activity_cancellation(
            &ctx,
            cancel.clone(),
            self.heartbeat_interval,
            self.runtime.call_model(&agent_name, request, cancel),
        )
        .await
    }

    /// Execute one tool call for the named agent.
    ///
    /// `pub(crate)` for the same marker-visibility reason as
    /// [`AgentActivities::render_instructions`].
    #[activity]
    pub(crate) async fn invoke_tool(
        self: Arc<Self>,
        ctx: ActivityContext,
        input: InvokeToolInput,
    ) -> Result<ToolCallOutcome, ActivityError> {
        let InvokeToolArgs {
            agent_name,
            call,
            ctx_seed,
        } = input.0;
        let cancel = CancellationToken::new();
        race_with_activity_cancellation(
            &ctx,
            cancel.clone(),
            self.heartbeat_interval,
            self.runtime
                .invoke_tool(&agent_name, call, ctx_seed, cancel),
        )
        .await
    }
}

#[cfg(test)]
mod activity_marker_tests {
    use super::AgentActivities;

    /// `#[activities]` generates one associated const per `#[activity]`
    /// method (e.g. `AgentActivities::call_model`) as the typed marker
    /// `crate::workflow::DurableAgentWorkflow`'s `run_effects` passes to
    /// `WorkflowContext::start_activity`. This test doubles as a
    /// compile-time check that the marker names match what this module's
    /// docs promise.
    #[test]
    fn activity_markers_exist_with_expected_names() {
        let _ = AgentActivities::render_instructions;
        let _ = AgentActivities::call_model;
        let _ = AgentActivities::invoke_tool;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_core::stream::BoxStream;
    use paigasus_helikon_core::{
        ContentPart, FinishReason, Item, ModelCapabilities, ModelError, ModelEvent, ModelSettings,
        ToolContext, ToolError, ToolOutput,
    };
    use serde_json::json;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    // ---- error_kind_to_activity_error (ADR-10) ------------------------

    /// ADR-10: a model failure must be a **non-retryable** application error
    /// so Temporal's activity-retry machinery does not silently mask it — the
    /// workflow's `DurableDriver` must observe it as terminal. This is the
    /// embodiment of "the runner never retries model errors".
    #[test]
    fn error_kind_to_activity_error_model_is_non_retryable() {
        let err = error_kind_to_activity_error(ErrorKindPayload::Model {
            message: "connection lost".to_owned(),
        });
        match err {
            ActivityError::Application(app) => {
                assert!(
                    app.is_non_retryable(),
                    "ADR-10: model-failure activity errors must be non-retryable"
                );
            }
            other => panic!("expected ActivityError::Application, got {other:?}"),
        }
    }

    // ---- call_model_inner ---------------------------------------------

    struct ScriptedModel {
        events: Mutex<Option<Vec<Result<ModelEvent, ModelError>>>>,
    }

    impl ScriptedModel {
        fn new(events: Vec<Result<ModelEvent, ModelError>>) -> Self {
            Self {
                events: Mutex::new(Some(events)),
            }
        }
    }

    #[async_trait::async_trait]
    impl Model for ScriptedModel {
        async fn invoke(
            &self,
            _request: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
            let events = self.events.lock().unwrap().take().unwrap_or_default();
            Ok(Box::pin(futures_util::stream::iter(events)))
        }

        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities::default()
        }
    }

    struct InvokeErrorModel;

    #[async_trait::async_trait]
    impl Model for InvokeErrorModel {
        async fn invoke(
            &self,
            _request: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
            Err(ModelError::Unavailable)
        }

        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities::default()
        }
    }

    #[tokio::test]
    async fn call_model_inner_aggregates_happy_path() {
        let model = ScriptedModel::new(vec![
            Ok(ModelEvent::TokenDelta { text: "hel".into() }),
            Ok(ModelEvent::TokenDelta { text: "lo".into() }),
            Ok(ModelEvent::Usage {
                input_tokens: 3,
                output_tokens: 2,
                cached_input_tokens: None,
                reasoning_tokens: None,
            }),
            Ok(ModelEvent::Finish {
                reason: FinishReason::Stop,
            }),
        ]);

        let result = call_model_inner(
            &model,
            "agent-1",
            ModelRequest::new(),
            CancellationToken::new(),
        )
        .await
        .expect("happy path succeeds");

        assert_eq!(result.0.items.len(), 1);
        match &result.0.items[0] {
            Item::AssistantMessage { content, agent } => {
                assert_eq!(agent.as_deref(), Some("agent-1"));
                match &content[0] {
                    ContentPart::Text { text } => assert_eq!(text, "hello"),
                    other => panic!("expected Text, got {other:?}"),
                }
            }
            other => panic!("expected AssistantMessage, got {other:?}"),
        }
        assert_eq!(result.0.usage.input_tokens, 3);
        assert_eq!(result.0.usage.output_tokens, 2);
        assert_eq!(result.0.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn call_model_inner_invoke_error_maps_to_model_payload() {
        let model = InvokeErrorModel;
        let err = call_model_inner(
            &model,
            "agent-1",
            ModelRequest::new(),
            CancellationToken::new(),
        )
        .await
        .expect_err("invoke() error must surface as Err");
        assert!(
            matches!(err, ErrorKindPayload::Model { .. }),
            "expected ErrorKindPayload::Model, got {err:?}"
        );
    }

    #[tokio::test]
    async fn call_model_inner_stream_error_maps_to_model_payload() {
        let model = ScriptedModel::new(vec![Err(ModelError::Transport("boom".to_owned()))]);
        let err = call_model_inner(
            &model,
            "agent-1",
            ModelRequest::new(),
            CancellationToken::new(),
        )
        .await
        .expect_err("a stream-level Err event must surface as Err");
        match err {
            ErrorKindPayload::Model { message } => assert!(message.contains("boom")),
            other => panic!("expected ErrorKindPayload::Model, got {other:?}"),
        }
    }

    // ---- invoke_tool_inner ----------------------------------------------

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool<()> for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echoes its arguments back"
        }
        fn schema(&self) -> &serde_json::Value {
            static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
            SCHEMA.get_or_init(|| json!({ "type": "object" }))
        }
        async fn invoke(
            &self,
            _ctx: &ToolContext<()>,
            args: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new(args))
        }
    }

    fn def_with_tools(tools: Vec<Arc<dyn Tool<()>>>) -> DurableAgentDef<()> {
        DurableAgentDef {
            name: "agent-1".to_owned(),
            instructions: Arc::new("system prompt".to_owned()),
            model: Arc::new(InvokeErrorModel),
            tools,
            plan: AgentPlan {
                tool_defs: Vec::new(),
                model_settings: ModelSettings::new(),
                output: None,
            },
        }
    }

    #[tokio::test]
    async fn invoke_tool_inner_echoes_args_round_trip() {
        let def = def_with_tools(vec![Arc::new(EchoTool)]);
        let run_ctx: RunContext<()> = RunContext::ephemeral(());
        let call = ToolCallRequest {
            call_id: "c1".to_owned(),
            name: "echo".to_owned(),
            args: json!({"x": 1}),
        };

        let outcome = invoke_tool_inner(&def, &run_ctx, call).await;

        assert_eq!(outcome.call_id, "c1");
        let parts = outcome.result.expect("echo tool call must succeed");
        match &parts[0] {
            ContentPart::Text { text } => assert!(text.contains('1')),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invoke_tool_inner_unknown_tool_is_err_outcome_not_panic() {
        let def = def_with_tools(vec![]);
        let run_ctx: RunContext<()> = RunContext::ephemeral(());
        let call = ToolCallRequest {
            call_id: "c1".to_owned(),
            name: "does-not-exist".to_owned(),
            args: json!({}),
        };

        let outcome = invoke_tool_inner(&def, &run_ctx, call).await;

        assert_eq!(outcome.call_id, "c1");
        let message = outcome
            .result
            .expect_err("unknown tool must be an Err outcome, not a panic");
        assert!(message.contains("unknown tool"));
    }

    // ---- render_instructions_inner ---------------------------------------

    #[tokio::test]
    async fn render_instructions_inner_delegates_to_instructions() {
        let def = def_with_tools(vec![]);
        let run_ctx: RunContext<()> = RunContext::ephemeral(());

        let text = render_instructions_inner(&def, &run_ctx).await;

        assert_eq!(text, "system prompt");
    }

    // ---- TypedRuntime::run_context applies the worker posture ------------

    #[test]
    fn typed_runtime_run_context_applies_posture() {
        use crate::worker::WorkerPosture;
        use paigasus_helikon_core::{DenyRule, PermissionMode};
        let rt = TypedRuntime::<()> {
            registry: Arc::new(HashMap::new()),
            ctx_factory: Arc::new(|_seed| Ok(())),
            posture: WorkerPosture::default()
                .with_permission_mode(PermissionMode::Plan)
                .with_deny_rules(vec![DenyRule::tool("Bash")]),
        };
        let ctx = rt
            .run_context(None, CancellationToken::new())
            .expect("factory never rejects a seed here");
        assert_eq!(ctx.permission_mode(), PermissionMode::Plan);
        assert_eq!(ctx.deny_rules().len(), 1);
    }

    // ---- run_context: fallible seeded ctx factory (SMA-455) --------------

    #[tokio::test]
    async fn run_context_seed_error_is_non_retryable() {
        use crate::worker::{CtxSeedError, WorkerPosture};
        let rt = TypedRuntime::<()> {
            registry: Arc::new(HashMap::new()),
            ctx_factory: Arc::new(|_seed| Err(CtxSeedError::new("bad seed"))),
            posture: WorkerPosture::default(),
        };
        let err = match rt.run_context(Some(serde_json::json!({"x": 1})), CancellationToken::new())
        {
            Ok(_) => panic!("a rejected seed must be an Err"),
            Err(e) => e,
        };
        match err {
            ActivityError::Application(app) => assert!(
                app.is_non_retryable(),
                "seed-rejection activity errors must be non-retryable"
            ),
            other => panic!("expected ActivityError::Application, got {other:?}"),
        }
    }

    // ---- race_loop (SMA-455 Task 4) --------------------------------------

    #[tokio::test]
    async fn race_loop_awaits_work_after_cancel() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc as StdArc;
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let done = StdArc::new(AtomicBool::new(false));
        let done2 = StdArc::clone(&done);
        let cancelled_flag = StdArc::new(AtomicBool::new(false));
        let cf = StdArc::clone(&cancelled_flag);

        let work = async move {
            let _ = rx.await; // completes only after on_cancel fires tx
            done2.store(true, Ordering::SeqCst);
            7u8
        };
        let result = race_loop(
            work,
            async { /* cancelled: immediately */ },
            move || {
                cf.store(true, Ordering::SeqCst);
                let _ = tx.send(()); // let the work future wind down
            },
            None,
            || {},
        )
        .await;

        assert_eq!(result, 7);
        assert!(cancelled_flag.load(Ordering::SeqCst), "on_cancel ran");
        assert!(
            done.load(Ordering::SeqCst),
            "work was awaited to completion, not dropped"
        );
    }

    #[tokio::test]
    async fn race_loop_heartbeats_until_work_done() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc as StdArc;
        let beats = StdArc::new(AtomicU32::new(0));
        let b2 = StdArc::clone(&beats);
        let work = async {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            1u8
        };
        let result = race_loop(
            work,
            std::future::pending::<()>(), // never cancelled
            || {},
            Some(std::time::Duration::from_millis(10)),
            move || {
                b2.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await;
        assert_eq!(result, 1);
        assert!(
            beats.load(Ordering::SeqCst) >= 1,
            "at least one heartbeat fired"
        );
    }

    #[tokio::test]
    async fn seeded_ctx_feeds_request_scoped_policy() {
        use crate::worker::WorkerPosture;
        use paigasus_helikon_core::{PermissionDecision, PermissionPolicy, RunContext, ToolEffect};

        struct Tenant {
            name: String,
        }
        struct TenantPolicy;
        #[async_trait]
        impl PermissionPolicy<Tenant> for TenantPolicy {
            async fn check(
                &self,
                ctx: &RunContext<Tenant>,
                _tool: &str,
                _args: &serde_json::Value,
            ) -> PermissionDecision {
                if ctx.user_ctx().name == "acme" {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny {
                        reason: "not acme".to_owned(),
                    }
                }
            }
        }

        let rt = TypedRuntime::<Tenant> {
            registry: Arc::new(HashMap::new()),
            ctx_factory: Arc::new(|seed| {
                let name = seed
                    .and_then(|v| v.get("tenant").and_then(|t| t.as_str()).map(str::to_owned))
                    .unwrap_or_default();
                Ok(Tenant { name })
            }),
            posture: WorkerPosture::default().with_permission_policy(Arc::new(TenantPolicy)),
        };

        let acme = rt
            .run_context(
                Some(serde_json::json!({"tenant": "acme"})),
                CancellationToken::new(),
            )
            .expect("factory ok");
        assert!(matches!(
            acme.authorize_tool("AnyTool", ToolEffect::ReadOnly, &serde_json::json!({}))
                .await,
            PermissionDecision::Allow
        ));

        let other = rt
            .run_context(
                Some(serde_json::json!({"tenant": "evil"})),
                CancellationToken::new(),
            )
            .expect("factory ok");
        assert!(matches!(
            other
                .authorize_tool("AnyTool", ToolEffect::ReadOnly, &serde_json::json!({}))
                .await,
            PermissionDecision::Deny { .. }
        ));
    }
}
