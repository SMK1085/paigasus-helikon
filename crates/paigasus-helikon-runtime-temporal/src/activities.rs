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

use async_trait::async_trait;
use paigasus_helikon_core::{
    execute_tool_call, CancellationToken, Instructions, Model, ModelRequest, ModelTurnAccumulator,
    RunContext, Tool, ToolCallOutcome, ToolCallRequest,
};
use temporalio_sdk::activities::{ActivityContext, ActivityError};
use temporalio_sdk::ApplicationFailure;

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
    /// Not read by anything in this crate yet — SMA-332 Task 8's workflow
    /// resolves it (`def.plan.clone()`) to construct `DurableDriver::new`.
    /// Written by [`crate::worker::TemporalAgentWorkerBuilder::register`]
    /// and asserted by its tests today; consumed for real once Task 8 lands.
    #[allow(dead_code)]
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
        cancel: CancellationToken,
    ) -> Result<ToolCallOutcome, ActivityError>;
}

/// The `Ctx`-generic [`DurableAgentRuntime`] implementer: the process-local
/// registry of every agent this worker was built with, plus the per-run
/// `Ctx` factory.
struct TypedRuntime<Ctx> {
    registry: Arc<HashMap<String, Arc<DurableAgentDef<Ctx>>>>,
    ctx_factory: Arc<dyn Fn() -> Ctx + Send + Sync>,
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
    /// activity invocation, wired to `cancel`.
    fn run_context(&self, cancel: CancellationToken) -> RunContext<Ctx> {
        RunContext::ephemeral((self.ctx_factory)()).with_cancel(cancel)
    }
}

#[async_trait]
impl<Ctx: Send + Sync + 'static> DurableAgentRuntime for TypedRuntime<Ctx> {
    async fn render_instructions(
        &self,
        agent_name: &str,
        cancel: CancellationToken,
    ) -> Result<String, ActivityError> {
        let def = self.resolve(agent_name)?;
        let run_ctx = self.run_context(cancel);
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
        cancel: CancellationToken,
    ) -> Result<ToolCallOutcome, ActivityError> {
        let def = self.resolve(agent_name)?;
        let run_ctx = self.run_context(cancel);
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

/// Race `work` against the activity's own cancellation signal.
///
/// If the activity is cancelled first, propagate that into `cancel` (so the
/// in-flight [`Model`]/[`Tool`] call can wind down per its own cancellation
/// contract) and then await `work` to completion, so callers still get a
/// coherent result instead of a dropped future — and so this never leaks a
/// detached task waiting on a cancellation signal that may never fire.
async fn race_with_activity_cancellation<T>(
    activity_ctx: &ActivityContext,
    cancel: CancellationToken,
    work: impl std::future::Future<Output = T>,
) -> T {
    tokio::pin!(work);
    tokio::select! {
        biased;
        result = &mut work => result,
        _ = activity_ctx.cancelled() => {
            cancel.cancel();
            work.await
        }
    }
}

/// The `Ctx`-erased, non-generic activities struct registered on the
/// worker's task queue. See the module docs for why it holds a trait object
/// rather than a `Ctx`-generic field directly.
pub(crate) struct AgentActivities {
    runtime: Arc<dyn DurableAgentRuntime>,
}

/// Build the [`AgentActivities`] instance a [`crate::worker::TemporalAgentWorker`]
/// registers for its task queue, from a `Ctx`-generic registry + ctx
/// factory. This is the erasure boundary: past this call, `Ctx` no longer
/// appears in any type the Temporal SDK holds onto.
pub(crate) fn build_activities<Ctx: Send + Sync + 'static>(
    registry: Arc<HashMap<String, Arc<DurableAgentDef<Ctx>>>>,
    ctx_factory: Arc<dyn Fn() -> Ctx + Send + Sync>,
) -> AgentActivities {
    AgentActivities {
        runtime: Arc::new(TypedRuntime {
            registry,
            ctx_factory,
        }),
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
        agent_name: String,
    ) -> Result<String, ActivityError> {
        let cancel = CancellationToken::new();
        race_with_activity_cancellation(
            &ctx,
            cancel.clone(),
            self.runtime.render_instructions(&agent_name, cancel),
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
        agent_name: String,
        request: ModelRequest,
    ) -> Result<ModelTurnResult, ActivityError> {
        let cancel = CancellationToken::new();
        race_with_activity_cancellation(
            &ctx,
            cancel.clone(),
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
        agent_name: String,
        call: ToolCallRequest,
    ) -> Result<ToolCallOutcome, ActivityError> {
        let cancel = CancellationToken::new();
        race_with_activity_cancellation(
            &ctx,
            cancel.clone(),
            self.runtime.invoke_tool(&agent_name, call, cancel),
        )
        .await
    }
}

#[cfg(test)]
mod activity_marker_tests {
    use super::AgentActivities;

    /// `#[activities]` generates one associated const per `#[activity]`
    /// method (e.g. `AgentActivities::call_model`) as the typed marker
    /// Task 8's workflow will pass to `WorkflowContext::start_activity`.
    /// Nothing in this crate references them yet since no workflow exists
    /// to call `start_activity` from; this test both keeps them from being
    /// flagged as dead code before Task 8 lands and doubles as a
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
}
