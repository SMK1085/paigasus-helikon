//! The [`Agent`] trait and its carrier types.
//!
//! One trait covers LLM-driven agents (`LlmAgent`) and workflow agents
//! (`SequentialAgent`, `ParallelAgent`, `LoopAgent`, `SwarmAgent`,
//! `GraphAgent`) — see ADR-11.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use tracing::Instrument as _;

use crate::{
    GuardrailKind, Handoff, Item, ModelError, RunContext, SessionError, TokenUsage, ToolError,
};

/// One trait for both LLM-driven and workflow agents.
///
/// See ADR-11 (*Single Agent trait subsumes LLM-driven and workflow
/// agents*).
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use futures_core::stream::BoxStream;
/// use paigasus_helikon_core::{
///     Agent, AgentError, AgentEvent, AgentInput, RunContext,
/// };
///
/// struct NoopAgent;
///
/// #[async_trait]
/// impl Agent<()> for NoopAgent {
///     fn name(&self) -> &str { "noop" }
///     fn description(&self) -> &str { "Does nothing." }
///
///     async fn run(
///         &self,
///         _ctx: RunContext<()>,
///         _input: AgentInput,
///     ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
///         use std::pin::Pin;
///         use std::task::{Context, Poll};
///         use futures_core::stream::Stream;
///
///         struct Empty;
///         impl Stream for Empty {
///             type Item = AgentEvent;
///             fn poll_next(
///                 self: Pin<&mut Self>,
///                 _cx: &mut Context<'_>,
///             ) -> Poll<Option<AgentEvent>> {
///                 Poll::Ready(None)
///             }
///         }
///
///         Ok(Box::pin(Empty))
///     }
/// }
/// ```
#[async_trait]
pub trait Agent<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Agent name. Used as the `agent` field in `SessionEvent::AssistantMessage`
    /// and `HookEvent::OnHandoff`.
    fn name(&self) -> &str;
    /// Human-readable description.
    fn description(&self) -> &str;

    /// Run the agent.
    ///
    /// The outer `Result` covers failure to *start* the stream; fatal
    /// errors during the run surface as [`AgentEvent::RunFailed`] inside
    /// the stream.
    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError>;
}

/// The input envelope crossing the agent boundary.
///
/// User-supplied input that seeds the run.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AgentInput {
    /// The initial conversation. Typically one [`crate::Item::UserMessage`].
    pub messages: Vec<crate::Item>,
    resolutions: Vec<(String, crate::ToolResolution)>,
    resume_plan: Option<crate::ResumePlan>,
}

impl AgentInput {
    /// Construct an empty input. Populate `messages` directly.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed the run with one user text message — the common case.
    pub fn from_user_text(text: impl Into<String>) -> Self {
        Self {
            messages: vec![crate::Item::UserMessage {
                content: vec![crate::ContentPart::Text { text: text.into() }],
            }],
            ..Default::default()
        }
    }

    /// Add the host decision for one deferred tool call.
    ///
    /// Resolutions keep the order in which you add them.
    ///
    /// One writer per session at a time: two resumes of one session at the
    /// same time can both run an approved tool.
    #[must_use]
    pub fn with_resolution(
        mut self,
        deferral_id: impl Into<String>,
        resolution: crate::ToolResolution,
    ) -> Self {
        self.resolutions.push((deferral_id.into(), resolution));
        self
    }

    /// The host decisions for deferred tool calls, in the order added.
    pub fn resolutions(&self) -> &[(String, crate::ToolResolution)] {
        &self.resolutions
    }

    /// Attach a validated resume plan.
    ///
    /// A deferral-aware runner sets this. It makes the plan with
    /// [`crate::ResumePlan::from_events`] from the session log and this input.
    #[must_use]
    pub fn with_resume_plan(mut self, plan: crate::ResumePlan) -> Self {
        self.resume_plan = Some(plan);
        self
    }

    /// The resume plan that a deferral-aware runner attached, if any.
    pub fn resume_plan(&self) -> Option<&crate::ResumePlan> {
        self.resume_plan.as_ref()
    }
}

/// Structured-output type marker: the JSON Schema the model is asked to
/// produce, the schema's name, and a validator that proves text
/// deserializes into the original `T`.
///
/// The validator is a function pointer captured at [`OutputType::from_schema`]
/// time (where `T: DeserializeOwned` is in scope). It is the authoritative
/// gate the agent loop uses to decide success vs. repair; the typed value
/// itself is materialized later by `RunResultStreaming::collect_typed`.
///
/// `Serialize`/`Deserialize` (added in SMA-332 for durable runners that plan
/// against a serialized `AgentPlan`) only carry `name` and `schema` — the
/// captured `validate` closure cannot cross a serialization boundary, so a
/// deserialized `OutputType` installs a fail-closed stand-in validator
/// instead (every call returns `Err`).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct OutputType {
    /// The schema name (the `T` identifier / schema title). Echoed into the
    /// provider `response_format` name and into the repair instruction.
    pub name: String,
    /// The JSON Schema the model should produce (raw schemars output).
    pub schema: schemars::Schema,
    /// Authoritative validator: `Ok(())` iff the value deserializes into the
    /// original `T`; `Err` carries one or more human-readable error strings.
    ///
    /// Not `Serialize`/`Deserialize` (it's a captured function pointer), so
    /// it is skipped on the wire and reinstalled as a fail-closed stand-in
    /// (`unavailable_output_validate`) after deserialization.
    #[serde(skip, default = "default_output_validate")]
    validate: fn(&serde_json::Value) -> Result<(), Vec<String>>,
}

/// `#[serde(default = "...")]` provider for [`OutputType::validate`]: returns
/// the fail-closed [`unavailable_output_validate`] function pointer.
///
/// Serde's `default` attribute calls a zero-argument function that returns
/// the field's type — this is that function; [`unavailable_output_validate`]
/// is the actual validator it hands back.
fn default_output_validate() -> fn(&serde_json::Value) -> Result<(), Vec<String>> {
    unavailable_output_validate
}

/// Fallback validator installed on [`OutputType::validate`] after a serde
/// round-trip.
///
/// The original validator captured at [`OutputType::from_schema`] time is a
/// function pointer closing over `T`'s `DeserializeOwned` impl; it cannot be
/// serialized, so `#[serde(skip)]` drops it and deserialization needs a
/// stand-in. This fails **closed** (every call is an `Err`) rather than
/// open, so a durable runner that accidentally validates against a
/// deserialized `OutputType` gets a loud repair/failure loop instead of
/// silently accepting non-conformant output. Callers that need authoritative
/// validation on the far side of a serialization boundary must reconstruct a
/// fresh `OutputType` via [`OutputType::from_schema`] rather than trust a
/// deserialized copy.
fn unavailable_output_validate(_value: &serde_json::Value) -> Result<(), Vec<String>> {
    Err(vec![
        "OutputType::validate is unavailable on a deserialized OutputType; \
         reconstruct via OutputType::from_schema::<T>() instead"
            .to_owned(),
    ])
}

impl std::fmt::Debug for OutputType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputType")
            .field("name", &self.name)
            .field("schema", &self.schema)
            .finish_non_exhaustive()
    }
}

impl OutputType {
    /// Construct from a type that derives [`schemars::JsonSchema`] and
    /// [`serde::de::DeserializeOwned`].
    ///
    /// Captures a validator that attempts `serde_json::from_value::<T>` and
    /// derives `name` from the schema's `title` (falling back to
    /// `"StructuredOutput"` if absent).
    pub fn from_schema<T>() -> Self
    where
        T: schemars::JsonSchema + serde::de::DeserializeOwned,
    {
        let schema = schemars::schema_for!(T);
        let name = schema
            .as_value()
            .get("title")
            .and_then(|t| t.as_str())
            .map(|s| s.to_owned())
            .unwrap_or_else(|| "StructuredOutput".to_owned());
        Self {
            schema,
            name,
            validate: |v| {
                serde_json::from_value::<T>(v.clone())
                    .map(|_| ())
                    .map_err(|e| vec![e.to_string()])
            },
        }
    }

    /// Run the captured validator against `value`.
    pub fn validate(&self, value: &serde_json::Value) -> Result<(), Vec<String>> {
        (self.validate)(value)
    }
}

/// Renders the system prompt for one turn of the loop.
///
/// Implemented for `String`, `&'static str`, and any
/// `Fn(&RunContext<Ctx>) -> String + Send + Sync`.
///
/// ```
/// use std::sync::Arc;
/// use paigasus_helikon_core::{Instructions, RunContext};
///
/// let a: Arc<dyn Instructions<()>> = Arc::new("You are a helpful assistant.".to_string());
/// let b: Arc<dyn Instructions<()>> = Arc::new(|_: &RunContext<()>| "Dynamic".into());
/// let _ = (a, b);
/// ```
pub trait Instructions<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Produce the system-prompt text for this run.
    fn render(&self, ctx: &crate::RunContext<Ctx>) -> String;
}

impl<Ctx> Instructions<Ctx> for String
where
    Ctx: Send + Sync + 'static,
{
    fn render(&self, _ctx: &crate::RunContext<Ctx>) -> String {
        self.clone()
    }
}

impl<Ctx> Instructions<Ctx> for &'static str
where
    Ctx: Send + Sync + 'static,
{
    fn render(&self, _ctx: &crate::RunContext<Ctx>) -> String {
        (*self).to_owned()
    }
}

impl<Ctx, F> Instructions<Ctx> for F
where
    Ctx: Send + Sync + 'static,
    F: Fn(&crate::RunContext<Ctx>) -> String + Send + Sync,
{
    fn render(&self, ctx: &crate::RunContext<Ctx>) -> String {
        (self)(ctx)
    }
}

/// The concrete LLM-driven agent. Implements [`crate::Agent`].
///
/// Constructed via direct field assignment in SMA-314; the ergonomic
/// typestate builder lands via `LlmAgent::builder()`; struct-literal
/// construction stays available as an escape hatch. **Not**
/// `#[non_exhaustive]` — the typestate builder needs struct-literal
/// construction from outside the crate.
pub struct LlmAgent<Ctx, M, T = String>
where
    Ctx: Send + Sync + 'static,
{
    /// Agent identifier (used in events and trace spans).
    pub name: String,
    /// One-line description.
    pub description: String,
    /// System-prompt renderer.
    pub instructions: std::sync::Arc<dyn Instructions<Ctx>>,
    /// The model the agent calls each turn.
    pub model: std::sync::Arc<M>,
    /// Tools the model may call. Each invocation snapshots these into
    /// `ModelRequest.tools` via [`crate::ToolDef`].
    pub tools: Vec<std::sync::Arc<dyn crate::Tool<Ctx>>>,
    /// Candidate agents this one may hand off to, with the conversation
    /// transferred. Driven by the agent loop (SMA-324).
    pub handoffs: Vec<Handoff<Ctx>>,
    /// Structured-output type marker. SMA-320 makes this honest.
    pub output_type: Option<OutputType>,
    /// Pre-input guardrails. Stored but not driven in SMA-314.
    pub input_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    /// Post-output guardrails. Stored but not driven in SMA-314.
    pub output_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    /// Lifecycle hooks. Stored but not driven in SMA-314.
    pub hooks: Vec<std::sync::Arc<dyn crate::Hook<Ctx>>>,
    /// Provider-tuning knobs. Field shape lands with SMA-316 / SMA-317.
    pub model_settings: crate::ModelSettings,
    /// Per-run config. At SMA-314 only `max_turns` is meaningful.
    pub config: crate::RunConfig,
    /// SMA-319: marker for the structured-output type. Doesn't appear
    /// in any field's value — only exists so the builder can flow
    /// `T` across `.output_type::<T>()` transitions.
    pub _output: std::marker::PhantomData<fn() -> T>,
}

impl LlmAgent<(), (), String> {
    /// Construct a new [`crate::LlmAgentBuilder`] in its initial state.
    ///
    /// `Ctx` is the per-run context type carried by [`RunContext`] —
    /// pass it as a turbofish if no setter call pins it implicitly
    /// (e.g. `.instructions(|ctx: &RunContext<MyCtx>| …)`).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use async_trait::async_trait;
    /// # use futures_core::stream::BoxStream;
    /// # use paigasus_helikon_core::{
    /// #     CancellationToken, LlmAgent, Model, ModelCapabilities, ModelError,
    /// #     ModelEvent, ModelRequest,
    /// # };
    /// # struct MyModel;
    /// # #[async_trait]
    /// # impl Model for MyModel {
    /// #     async fn invoke(&self, _r: ModelRequest, _c: CancellationToken)
    /// #         -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>
    /// #     { Err(ModelError::Unavailable) }
    /// #     fn capabilities(&self) -> ModelCapabilities { ModelCapabilities::default() }
    /// # }
    /// let agent = LlmAgent::builder::<()>()
    ///     .name("triage")
    ///     .model(MyModel)
    ///     .build();
    /// ```
    pub fn builder<Ctx>() -> crate::LlmAgentBuilder<Ctx, (), String, crate::NoName, crate::NoModel>
    where
        Ctx: Send + Sync + 'static,
    {
        crate::LlmAgentBuilder::__new()
    }
}

/// The unified event stream emitted by an [`Agent`].
///
/// Twenty variants spanning lifecycle, raw streaming deltas,
/// post-aggregation semantic items, agent transitions, control signals,
/// and terminal outcomes. The semantic-item variants
/// (`MessageOutput`, `ToolCallItem`, `ToolOutputItem`) carry a full
/// [`Item`] — the doc on each variant names the expected inner variant.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentEvent {
    // --- Lifecycle ---
    /// The run has started; the named agent is active.
    RunStarted {
        /// Agent name.
        agent: String,
    },
    /// A new turn (one model invocation plus any tool calls) has begun.
    TurnStarted {
        /// Zero-based turn index within the run.
        turn: u32,
    },

    // --- Raw deltas (for low-latency UIs) ---
    /// An incremental assistant-text chunk.
    TokenDelta {
        /// Text fragment.
        text: String,
    },
    /// An incremental reasoning-text chunk.
    ReasoningDelta {
        /// Text fragment.
        text: String,
    },
    /// An incremental tool-call-arguments chunk.
    ToolCallDelta {
        /// Provider-assigned call identifier.
        call_id: String,
        /// `Some` exactly once per non-blank `call_id`, on the first delta
        /// for which the provider can establish the name is complete, and
        /// `None` on every other delta. When `Some`, the value is the whole
        /// name so far as the provider can determine — a provider receiving
        /// the name in fragments MUST buffer and concatenate them, and MUST
        /// NOT emit a name it can detect is still incomplete.
        ///
        /// `skip_serializing_if = "Option::is_none"` so subsequent deltas
        /// (which have no name) omit the field entirely rather than emitting
        /// `"name": null`.
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// JSON-encoded argument fragment.
        args_delta: String,
    },

    // --- Semantic items (post-aggregation; carry Item) ---
    /// A complete assistant message produced by the model. The inner
    /// [`Item`] is expected to be [`Item::AssistantMessage`].
    MessageOutput {
        /// The complete message.
        item: Item,
    },
    /// A complete tool call resolved during the turn. The inner [`Item`]
    /// is expected to be [`Item::ToolCall`].
    ToolCallItem {
        /// The complete tool call.
        item: Item,
    },
    /// A complete tool result returned by a tool. The inner [`Item`] is
    /// expected to be [`Item::ToolResult`].
    ToolOutputItem {
        /// The complete tool result.
        item: Item,
    },
    /// A handoff item recorded in the trajectory.
    HandoffItem {
        /// Outgoing agent name.
        from: String,
        /// Incoming agent name.
        to: String,
    },

    // --- Agent transitions ---
    /// The currently-active agent changed.
    AgentUpdated {
        /// Name of the newly-active agent.
        agent: String,
    },

    // --- Control ---
    /// A guardrail tripwire fired during the run.
    GuardrailTriggered {
        /// Which kind of tripwire fired.
        kind: GuardrailKind,
        /// Free-form context supplied by the guardrail.
        info: serde_json::Value,
    },
    /// The runner is awaiting an approval decision before proceeding.
    ApprovalRequested {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        tool: String,
        /// Effective JSON arguments (after `PreToolUse` and any policy `Replace`), redacted with the run's secret set when output redaction is on.
        args: serde_json::Value,
    },
    /// A tool call was denied by the permission layer. The model separately
    /// receives the denial as a synthetic tool result; this event is for
    /// observability.
    PermissionDenied {
        /// Tool name.
        tool: String,
        /// Human-readable denial reason.
        reason: String,
    },
    /// A structured-output repair turn has begun: validation of the prior
    /// constrained output failed and the loop is re-prompting once.
    RepairStarted {
        /// 1-based repair attempt index. Only ever `1` under the one-shot budget.
        attempt: u32,
    },
    /// Structured-output validation failed terminally (after the one repair
    /// attempt). Emitted immediately before the terminal [`AgentEvent::RunFailed`]
    /// so consumers can recover the structured detail.
    StructuredOutputFailed {
        /// Human-readable schema/validation errors.
        schema_errors: Vec<String>,
        /// The raw terminal assistant text that failed validation.
        final_text: String,
    },

    /// The host injected a user message at a turn boundary through the
    /// turn checkpoint (see [`crate::TurnCheckpoint`]).
    ///
    /// Commit rule: the run owns the injected content if and only if this
    /// event is in the event stream. Before this event, the host owns the
    /// content. The session recorder stores the content as a user message,
    /// so a later run of the same session sees it. This event is not
    /// terminal.
    UserMessageInjected {
        /// The injected user content parts.
        content: Vec<crate::ContentPart>,
    },

    // --- Terminal ---
    /// The run finished normally.
    RunCompleted {
        /// Aggregated usage across the run.
        usage: TokenUsage,
    },
    /// The run finished with an error.
    RunFailed {
        /// Human-readable error message.
        error: String,
    },
    /// The run stopped on tool calls that need a later decision.
    RunDeferred {
        /// The deferred tool calls.
        ///
        /// When output redaction is on, the args and the prompt of these
        /// calls are the redacted copy. Do not use them to run or to hash a
        /// call. The stored `SessionEvent::ToolDeferred` events hold the raw
        /// effective args and the raw prompt. They are the source of truth
        /// for the resume.
        calls: Vec<crate::PendingToolCall>,
        /// Aggregated usage across the run.
        usage: TokenUsage,
    },
    /// The host stopped the run at a turn boundary through the turn
    /// checkpoint. This is a controlled end, not a failure.
    RunStopped {
        /// The reason the host gave for the stop.
        reason: String,
        /// Aggregated usage across every turn that ran.
        usage: TokenUsage,
    },
}

impl AgentEvent {
    /// `true` for the four events that end a run: [`AgentEvent::RunCompleted`],
    /// [`AgentEvent::RunFailed`], [`AgentEvent::RunDeferred`] and
    /// [`AgentEvent::RunStopped`].
    ///
    /// This is the single definition of "terminal" every runner shares. A
    /// runner's cancel/timeout boundary loses to a terminal that already
    /// occurred; the `terminal_tests::classify` guard keeps a newly added
    /// variant from silently defaulting to non-terminal here.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::RunCompleted { .. }
                | Self::RunFailed { .. }
                | Self::RunDeferred { .. }
                | Self::RunStopped { .. }
        )
    }
}

// ── Private helpers for the LlmAgent driver ─────────────────────────────────

/// Concatenate the text of all `Item::UserMessage` parts in the seed
/// conversation — the text input guardrails inspect.
fn user_text_of(conversation: &[crate::Item]) -> String {
    let mut s = String::new();
    for item in conversation {
        if let crate::Item::UserMessage { content } = item {
            for part in content {
                if let crate::ContentPart::Text { text } = part {
                    s.push_str(text);
                }
            }
        }
    }
    s
}

/// The outcome of one tool call of a turn.
#[derive(Debug)]
enum ConcurrentOutcome {
    /// The call ran, failed, or was denied. Its result goes into the conversation.
    Done(crate::ToolCallOutcome),
    /// The call was deferred and did not run. Holds the raw effective args.
    Deferred(crate::PendingToolCall),
}

/// Map one event of a handoff child run to the event that the parent
/// stream yields. Drops the child `RunStarted`. Adds `parent_usage` to the
/// usage of `RunCompleted`, `RunDeferred` and `RunStopped`, and records the
/// total on `run_span`. Passes every other event through.
fn reyield_child_event(
    ev: AgentEvent,
    parent_usage: TokenUsage,
    run_span: &tracing::Span,
) -> Option<AgentEvent> {
    let record_usage = |total: &TokenUsage| {
        run_span.record("gen_ai.usage.input_tokens", total.input_tokens as i64);
        run_span.record("gen_ai.usage.output_tokens", total.output_tokens as i64);
    };
    match ev {
        AgentEvent::RunStarted { .. } => None,
        AgentEvent::RunCompleted { usage } => {
            let mut total = parent_usage;
            total.add(usage);
            record_usage(&total);
            Some(AgentEvent::RunCompleted { usage: total })
        }
        AgentEvent::RunDeferred { calls, usage } => {
            let mut total = parent_usage;
            total.add(usage);
            record_usage(&total);
            run_span.record("paigasus.deferred_calls", calls.len() as i64);
            Some(AgentEvent::RunDeferred {
                calls,
                usage: total,
            })
        }
        AgentEvent::RunStopped { reason, usage } => {
            let mut total = parent_usage;
            total.add(usage);
            record_usage(&total);
            run_span.record("paigasus.stop_reason", reason.as_str());
            Some(AgentEvent::RunStopped {
                reason,
                usage: total,
            })
        }
        other => Some(other),
    }
}

/// The `tool.execute` span of one tool call. `resumed` marks a resumed
/// deferred call (`paigasus.tool.resumed = true`); a normal call leaves that
/// field empty, so it is not exported.
pub(crate) fn tool_execute_span(
    parent: &tracing::Span,
    name: &str,
    resumed: bool,
) -> tracing::Span {
    let span = tracing::info_span!(
        target: "paigasus::core::agent",
        parent: parent,
        "tool.execute",
        otel.name = tracing::field::Empty,
        otel.kind = "internal",
        gen_ai.operation.name = "execute_tool",
        gen_ai.tool.name = %name,
        paigasus.tool.resumed = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
    );
    span.record("otel.name", format!("execute_tool {name}").as_str());
    if resumed {
        span.record("paigasus.tool.resumed", true);
    }
    span
}

/// Invoke `tool` with `args`, fire `PostToolUse` on the raw JSON output, and
/// finalize the output. Redaction is the final transform. Call it inside the
/// call's `tool.execute` span: a tool error marks that span as `ERROR`.
pub(crate) async fn invoke_and_finalize<Ctx>(
    tool: &std::sync::Arc<dyn crate::Tool<Ctx>>,
    tool_ctx: &crate::ToolContext<Ctx>,
    args: serde_json::Value,
    interceptors: &crate::control::Interceptors<'_, Ctx>,
    redact_output: bool,
    secrets: &crate::redaction::SecretSet,
) -> Result<Vec<crate::ContentPart>, String>
where
    Ctx: Send + Sync + 'static,
{
    // Keep the tool's raw JSON output so the PostToolUse hook sees the
    // structured value, not a stringified form.
    let output_json = match tool.invoke(tool_ctx, args).await {
        Ok(output) => output.content,
        Err(e) => {
            tracing::Span::current().record("otel.status_code", "ERROR");
            return Err(e.to_string());
        }
    };
    // PostToolUse hook (ReplaceOutput / Deny→denial). The hook receives the
    // raw `serde_json::Value`; a `ReplaceOutput` value (or the original) is
    // rendered to content parts only at the end.
    let post = interceptors
        .fire(&crate::HookEvent::PostToolUse {
            tool: tool.name().to_owned(),
            output: output_json.clone(),
        })
        .await;
    if let Some(reason) = post.denied {
        return Err(format!("blocked by PostToolUse hook: {reason}"));
    }
    let final_json = post.replacement.unwrap_or(output_json);
    // Redaction is the FINAL transform — after user PostToolUse hooks — so a
    // hook cannot reintroduce an unredacted secret. Shared with the
    // durable-runner pipeline in `tool_exec.rs`.
    Ok(crate::finalize_tool_output(
        final_json,
        redact_output,
        secrets,
    ))
}

/// Await `futures` in their order: all at once, or at most `limit` at a time.
pub(crate) async fn run_with_limit<F>(
    futures: impl Iterator<Item = F>,
    limit: Option<std::num::NonZeroUsize>,
) -> Vec<F::Output>
where
    F: std::future::Future,
{
    match limit {
        None => futures_util::future::join_all(futures).await,
        Some(n) => {
            use futures_util::stream::StreamExt as _;
            let collected: Vec<_> = futures.collect();
            futures_util::stream::iter(collected)
                .buffered(n.get())
                .collect()
                .await
        }
    }
}

/// Run the tool calls of one turn. Returns one outcome per call, in call
/// order, and the `PermissionDenied` events of the turn.
async fn run_tools_concurrent<Ctx>(
    tools: &[std::sync::Arc<dyn crate::Tool<Ctx>>],
    calls: &[crate::ToolCallRequest],
    interceptors: &crate::control::Interceptors<'_, Ctx>,
    tool_ctx: &crate::ToolContext<Ctx>,
    limit: Option<std::num::NonZeroUsize>,
    parent: &tracing::Span,
) -> (Vec<ConcurrentOutcome>, Vec<crate::AgentEvent>)
where
    Ctx: Send + Sync + 'static,
{
    let denied_events: std::sync::Mutex<Vec<crate::AgentEvent>> = std::sync::Mutex::new(Vec::new());
    let redact_output = interceptors.ctx.redact_output();
    let secrets = crate::redaction::SecretSet::from_env_and_extra(interceptors.ctx.extra_secrets());
    let handoff_path = interceptors.ctx.handoff_path();

    let futures = calls.iter().map(|call| {
        let tool = tools.iter().find(|t| t.name() == call.name).cloned();
        let effect = tool
            .as_ref()
            .map(|t| t.effect())
            .unwrap_or(crate::ToolEffect::SideEffect);
        let call_id = call.call_id.clone();
        let name = call.name.clone();
        let orig_args = call.args.clone();
        let denied_events = &denied_events;
        let secrets = &secrets;
        let span = tool_execute_span(parent, &name, false);
        async move {
            // PreToolUse hook.
            let pre = interceptors
                .fire(&crate::HookEvent::PreToolUse {
                    tool: name.clone(),
                    args: orig_args.clone(),
                })
                .await;
            if let Some(reason) = pre.denied {
                return ConcurrentOutcome::Done(crate::ToolCallOutcome {
                    call_id,
                    result: Err(format!("blocked by PreToolUse hook: {reason}")),
                });
            }
            let mut args = pre.replacement.unwrap_or(orig_args);

            // Permission authorize on the effective args. This is the pipeline
            // of `RunContext::authorize_tool`, with the detailed result. It
            // gives `Defer` only when deferred tool calls are on for this run.
            let decision = interceptors
                .authorize_detailed(&name, effect, &args, crate::context::AskPoints::Ask)
                .await;
            match decision {
                crate::context::Authorization::Allow => {}
                crate::context::Authorization::Replace { args: sanitized } => {
                    args = sanitized;
                }
                crate::context::Authorization::Deny { reason } => {
                    denied_events
                        .lock()
                        .unwrap()
                        .push(crate::AgentEvent::PermissionDenied {
                            tool: name.clone(),
                            reason: reason.clone(),
                        });
                    return ConcurrentOutcome::Done(crate::ToolCallOutcome {
                        call_id,
                        result: Err(format!("permission denied: {reason}")),
                    });
                }
                crate::context::Authorization::Defer {
                    prompt,
                    args: effective,
                } => {
                    // A call to a tool that the agent does not have is not
                    // deferred: an approval for it would be wasted.
                    if tool.is_none() {
                        tracing::Span::current().record("otel.status_code", "ERROR");
                        return ConcurrentOutcome::Done(crate::ToolCallOutcome {
                            call_id,
                            result: Err(format!("unknown tool: {name}")),
                        });
                    }
                    // A transfer call above this agent with the same call
                    // id has no result while the run goes on. The session
                    // log cannot keep such a deferral open, so deny the call.
                    if interceptors.ctx.open_call_ids().contains(&call_id) {
                        let reason =
                            format!("deferral not possible: call id {call_id} repeats in this run");
                        denied_events
                            .lock()
                            .unwrap()
                            .push(crate::AgentEvent::PermissionDenied {
                                tool: name.clone(),
                                reason: reason.clone(),
                            });
                        return ConcurrentOutcome::Done(crate::ToolCallOutcome {
                            call_id,
                            result: Err(format!("permission denied: {reason}")),
                        });
                    }
                    return ConcurrentOutcome::Deferred(crate::PendingToolCall::new(
                        crate::deferral::new_deferral_id(),
                        call_id,
                        name,
                        effective,
                        prompt,
                        handoff_path.to_vec(),
                    ));
                }
            }

            // Invoke, PostToolUse, then redaction (the final transform).
            let outcome = match tool {
                Some(t) => {
                    invoke_and_finalize(&t, tool_ctx, args, interceptors, redact_output, secrets)
                        .await
                }
                None => {
                    tracing::Span::current().record("otel.status_code", "ERROR");
                    Err(format!("unknown tool: {name}"))
                }
            };

            ConcurrentOutcome::Done(crate::ToolCallOutcome {
                call_id,
                result: outcome,
            })
        }
        .instrument(span)
    });

    let outcomes = run_with_limit(futures, limit).await;
    (outcomes, denied_events.into_inner().unwrap())
}

// ── Agent impl for LlmAgent ──────────────────────────────────────────────────

#[async_trait::async_trait]
impl<Ctx, M, T> crate::Agent<Ctx> for LlmAgent<Ctx, M, T>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
    T: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    async fn run(
        &self,
        ctx: crate::RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<futures_core::stream::BoxStream<'static, crate::AgentEvent>, AgentError> {
        use futures_util::stream::StreamExt as _;

        // This agent is the last name in the handoff path of its deferred calls.
        let mut ctx = ctx;
        ctx.push_handoff_path(self.name.clone());

        // Snapshot everything the stream needs — it outlives `&self`.
        let model = std::sync::Arc::clone(&self.model);
        let tools = self.tools.clone();
        let effective_config = ctx
            .run_config()
            .cloned()
            .unwrap_or_else(|| self.config.clone());
        let max_turns = effective_config.max_turns;
        let parallel_tool_call_limit = effective_config.parallel_tool_call_limit;
        let model_settings = self.model_settings.clone();
        let agent_name = self.name.clone();
        let instructions_text = self.instructions.render(&ctx);
        let output_type = self.output_type.clone();
        let tool_defs: Vec<crate::ToolDef> = tools
            .iter()
            .map(|t| crate::ToolDef {
                name: t.name().to_owned(),
                description: t.description().to_owned(),
                schema: t.schema().clone(),
            })
            .collect();
        let handoffs = self.handoffs.clone();
        let input_guardrails = self.input_guardrails.clone();
        let output_guardrails = self.output_guardrails.clone();
        let agent_hooks = self.hooks.clone();
        let max_agent_depth = effective_config.max_agent_depth;
        let mut input = input;
        let has_resolutions = !input.resolutions.is_empty();
        let resume_plan = input.resume_plan.take();

        let stream = async_stream::stream! {
            // SMA-346: structured failures are recorded here and read by the
            // boundary after the stream drains (see RunResultStreaming::collect).
            // Invariant: every terminal-failure path must `failure.set(...)`
            // before it `return`s (direct sites), or rely on the `Terminate`
            // arm (state-machine sites via LoopState::Failed).
            let failure = ctx.failure_handle();

            // Seed conversation: optional system message + the resume history
            // (if any) + user input. The results of the resumed calls go
            // between the history and the new input (at `results_at`).
            let mut conversation: Vec<crate::Item> = Vec::new();
            if !instructions_text.is_empty() {
                conversation.push(crate::Item::System {
                    content: vec![crate::ContentPart::Text { text: instructions_text }],
                });
            }
            if let Some(plan) = &resume_plan {
                conversation.extend(plan.history().iter().cloned());
            }
            let results_at = conversation.len();
            conversation.extend(input.messages.iter().cloned());
            // A resume that routes to a handoff target gives it the new input.
            let resume_messages = if resume_plan.is_some() {
                input.messages.clone()
            } else {
                Vec::new()
            };
            // The synthetic results for a run that ends before the resume ran
            // its calls. Each failure exit before that point yields them first.
            let not_run = resume_plan
                .as_ref()
                .map(|p| crate::resume_exec::not_run_results(p.calls()))
                .unwrap_or_default();

            let mut loop_state = crate::LoopState::CallingModel { turn: 0, usage: crate::TokenUsage::default() };
            let mut tx_input = crate::TransitionInput::Start { messages: input.messages };

            let run_span = tracing::info_span!(
                target: "paigasus::core::agent",
                "agent.run",
                otel.name = tracing::field::Empty,
                otel.kind = "internal",
                gen_ai.operation.name = "invoke_agent",
                gen_ai.agent.name = %agent_name,
                langfuse.session.id = tracing::field::Empty,
                langfuse.user.id = tracing::field::Empty,
                langfuse.trace.tags = tracing::field::Empty,
                gen_ai.usage.input_tokens = tracing::field::Empty,
                gen_ai.usage.output_tokens = tracing::field::Empty,
                paigasus.deferred_calls = tracing::field::Empty,
                paigasus.stop_reason = tracing::field::Empty,
                otel.status_code = tracing::field::Empty,
            );
            run_span.record("otel.name", format!("invoke_agent {agent_name}").as_str());
            if let Some(v) = ctx.tracer().session_id() {
                run_span.record("langfuse.session.id", v);
            }
            if let Some(v) = ctx.tracer().user_id() {
                run_span.record("langfuse.user.id", v);
            }
            if !ctx.tracer().tags().is_empty() {
                if let Ok(json) = serde_json::to_string(ctx.tracer().tags()) {
                    run_span.record("langfuse.trace.tags", json.as_str());
                }
            }
            let mut turn_span: Option<tracing::Span> = None;

            yield crate::AgentEvent::RunStarted { agent: agent_name.clone() };

            // Resolutions without a resume plan: only a deferral-aware runner
            // can validate them against the session log.
            if has_resolutions && resume_plan.is_none() {
                let msg = "resolutions require a deferral-aware runner";
                run_span.record("otel.status_code", "ERROR");
                failure.set(crate::AgentError::Other(anyhow::anyhow!(msg)));
                yield crate::AgentEvent::RunFailed { error: msg.to_owned() };
                return;
            }

            // SMA-324: synthetic transfer tools; fail fast on name collisions.
            let handoff_defs: Vec<crate::HandoffDef> =
                handoffs.iter().map(|h| h.to_def()).collect();
            {
                let real: std::collections::HashSet<&str> =
                    tool_defs.iter().map(|t| t.name.as_str()).collect();
                let mut seen = std::collections::HashSet::new();
                for d in &handoff_defs {
                    if !seen.insert(d.tool_name.as_str()) || real.contains(d.tool_name.as_str())
                    {
                        let err = crate::AgentError::Other(anyhow::anyhow!(
                            "handoff transfer-tool name collision: '{}'",
                            d.tool_name
                        ));
                        let msg = err.to_string();
                        failure.set(err);
                        for ev in not_run {
                            yield ev;
                        }
                        yield crate::AgentEvent::RunFailed { error: msg };
                        return;
                    }
                }
            }

            let mut pending_injections: Vec<String> = Vec::new();
            let interceptors = crate::control::Interceptors {
                ctx: &ctx,
                input_guardrails: &input_guardrails,
                output_guardrails: &output_guardrails,
                agent_hooks: &agent_hooks,
            };

            // OnRunStart hook: Deny aborts; injected system messages seed the conversation.
            let on_start = interceptors.fire(&crate::HookEvent::OnRunStart).await;
            if let Some(reason) = on_start.denied {
                let err = crate::AgentError::HookDenied {
                    event: "OnRunStart".to_owned(),
                    reason,
                };
                let msg = err.to_string();
                run_span.record("otel.status_code", "ERROR");
                failure.set(err);
                for ev in not_run {
                    yield ev;
                }
                yield crate::AgentEvent::RunFailed { error: msg };
                return;
            }
            pending_injections.extend(on_start.injections);

            // Input guardrails — blocking gate (AC1: zero model calls on a tripwire).
            let seed_text = user_text_of(&conversation);
            if let Some((kind, info)) = interceptors.run_input_guardrails(&seed_text).await {
                run_span.record("otel.status_code", "ERROR");
                for ev in not_run {
                    yield ev;
                }
                yield crate::AgentEvent::GuardrailTriggered { kind: kind.clone(), info };
                failure.set(crate::AgentError::Guardrail { kind });
                yield crate::AgentEvent::RunFailed {
                    error: "input guardrail tripwire".to_owned(),
                };
                return;
            }

            // Resume: run the resolved deferred calls before the first turn,
            // so their results come before any `TurnStarted` (the session
            // recorder takes them by that position).
            if let Some(plan) = resume_plan {
                // The routing rules of spec §4.7. A renamed root (only
                // possible at the root level) denies every call here.
                let renamed = plan.expected_agent() != agent_name;
                let next = if renamed {
                    None
                } else {
                    plan.next_agent().map(str::to_owned)
                };
                let target_agent = next.as_deref().and_then(|next| {
                    handoffs
                        .iter()
                        .find(|h| h.agent().name() == next)
                        .map(|h| std::sync::Arc::clone(h.agent()))
                });

                // The path continues to a handoff target: the target runs the
                // calls. The session already holds this handoff, so there is
                // no `OnHandoff` and no `HandoffItem`, and this agent emits no
                // `TurnStarted` (the recorder takes the resume results up to
                // the first one).
                if let (Some(next), Some(target_agent)) = (next.as_ref(), target_agent) {
                    let child = ctx.handoff_child();
                    if child.agent_depth() > max_agent_depth {
                        let err = crate::AgentError::MaxAgentDepthExceeded {
                            depth: child.agent_depth(),
                            max: max_agent_depth,
                        };
                        let msg = err.to_string();
                        run_span.record("otel.status_code", "ERROR");
                        failure.set(err);
                        // The calls do not run, so their transfer calls stay
                        // open unless this exit closes them.
                        for item in crate::resume_exec::close_open_transfers(&plan, &mut conversation) {
                            yield crate::AgentEvent::ToolOutputItem { item };
                        }
                        for ev in not_run {
                            yield ev;
                        }
                        yield crate::AgentEvent::RunFailed { error: msg };
                        return;
                    }
                    yield crate::AgentEvent::AgentUpdated { agent: next.clone() };
                    let child_input = crate::AgentInput {
                        messages: resume_messages,
                        resume_plan: Some(plan.descend()),
                        ..Default::default()
                    };
                    let mut sub = match target_agent.run(child, child_input).await {
                        Ok(s) => s,
                        Err(e) => {
                            let msg = e.to_string();
                            run_span.record("otel.status_code", "ERROR");
                            failure.set(e);
                            for item in crate::resume_exec::close_open_transfers(&plan, &mut conversation) {
                                yield crate::AgentEvent::ToolOutputItem { item };
                            }
                            for ev in not_run {
                                yield ev;
                            }
                            yield crate::AgentEvent::RunFailed { error: msg };
                            return;
                        }
                    };
                    while let Some(ev) = sub.next().await {
                        if let Some(ev) =
                            reyield_child_event(ev, crate::TokenUsage::default(), &run_span)
                        {
                            yield ev;
                        }
                    }
                    let _ = interceptors
                        .fire(&crate::HookEvent::OnSubagentStop { agent: next.clone() })
                        .await;
                    return;
                }

                // This agent runs the calls: the plan ends here, or the root
                // was renamed, or the next agent is not a handoff target any
                // more. In the last two cases every call is denied.
                let plan = if renamed {
                    plan.with_all_denied(&format!(
                        "agent no longer available: {}",
                        plan.expected_agent()
                    ))
                } else if let Some(next) = &next {
                    plan.with_all_denied(&format!("agent no longer available: {next}"))
                } else {
                    plan
                };
                // Close the open transfer calls after this level, so that the
                // next model request is valid. Their results come first in
                // the resume block. They are in the history, before
                // `results_at`.
                let closed = crate::resume_exec::close_open_transfers(&plan, &mut conversation);
                let results_at = results_at + closed.len();
                for item in closed {
                    yield crate::AgentEvent::ToolOutputItem { item };
                }
                let tool_ctx = ctx.to_tool_context();
                let (outcomes, denied) = crate::resume_exec::run_resolved_calls(
                    &tools,
                    plan.calls(),
                    &interceptors,
                    &tool_ctx,
                    parallel_tool_call_limit,
                    &run_span,
                )
                .await;
                for ev in denied {
                    yield ev;
                }
                let mut results = Vec::with_capacity(outcomes.len());
                for o in outcomes {
                    let item = crate::Item::ToolResult {
                        call_id: o.call_id,
                        content: o.result.unwrap_or_else(|e| {
                            vec![crate::ContentPart::Text { text: e }]
                        }),
                    };
                    yield crate::AgentEvent::ToolOutputItem { item: item.clone() };
                    results.push(item);
                }
                conversation.splice(results_at..results_at, results);
            }

            loop {
                let tx_ctx = crate::TransitionCtx {
                    tools: &tool_defs,
                    model_settings: &model_settings,
                    max_turns,
                    conversation: &conversation,
                    output: output_type.as_ref(),
                    handoffs: &handoff_defs,
                };
                let outcome = crate::transition(&loop_state, tx_input, &tx_ctx);
                let crate::TransitionOutcome { next_state, events, next_action, conversation_appends } = outcome;

                // Output-guardrail gate: a tripwire on the terminal output
                // suppresses RunCompleted and fails the run instead.
                let output_trip = if let crate::LoopState::Done(out) = &next_state {
                    interceptors.run_output_guardrails(&out.as_text()).await
                } else {
                    None
                };
                // The call ids of the transfer calls of a handoff. The parent
                // emits a result for each of them after the handoff.
                let transfer_call_ids: Vec<String> =
                    if matches!(next_action, crate::NextAction::Handoff) {
                        events
                            .iter()
                            .filter_map(|ev| match ev {
                                crate::AgentEvent::ToolCallItem {
                                    item: crate::Item::ToolCall { call_id, .. },
                                } => Some(call_id.clone()),
                                _ => None,
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                let (events, mut next_action, next_state) = if let Some((kind, info)) = output_trip {
                    run_span.record("otel.status_code", "ERROR");
                    failure.set(crate::AgentError::Guardrail { kind: kind.clone() });
                    (
                        vec![
                            crate::AgentEvent::GuardrailTriggered { kind, info },
                            crate::AgentEvent::RunFailed {
                                error: "output guardrail tripwire".to_owned(),
                            },
                        ],
                        crate::NextAction::Terminate,
                        next_state,
                    )
                } else {
                    (events, next_action, next_state)
                };
                for ev in events {
                    match &ev {
                        crate::AgentEvent::TurnStarted { turn } => {
                            // SMA-804: turn-boundary checkpoint. The batch that
                            // closes a tool turn and opens the next model turn
                            // is the boundary; the checkpoint runs before the
                            // turn span and the `OnTurnStart` hook.
                            let boundary_turn = match (&loop_state, &next_action) {
                                (
                                    crate::LoopState::ExecutingTools { turn: t, .. },
                                    crate::NextAction::CallModel { .. },
                                ) => Some(*t),
                                _ => None,
                            };
                            if let (Some(t), Some(cp)) = (boundary_turn, ctx.turn_checkpoint()) {
                                let boundary = crate::TurnBoundary::new(
                                    t,
                                    *turn,
                                    agent_name.clone(),
                                    ctx.agent_depth(),
                                );
                                let cp_span = tracing::info_span!(
                                    target: "paigasus::core::agent",
                                    parent: &run_span,
                                    "agent.turn_checkpoint",
                                    otel.kind = "internal",
                                    decision = tracing::field::Empty,
                                );
                                let decision = cp
                                    .at_boundary(&ctx, &boundary)
                                    .instrument(cp_span.clone())
                                    .await;
                                // One private stop path, shared by `Stop` and the
                                // guardrail tripwire. SMA-804 Task 4 fills it in.
                                let mut stop_reason: Option<String> = None;
                                match decision {
                                    crate::CheckpointDecision::Continue => {
                                        cp_span.record("decision", "continue");
                                    }
                                    crate::CheckpointDecision::Stop { reason } => {
                                        cp_span.record("decision", "stop");
                                        stop_reason = Some(reason);
                                    }
                                    crate::CheckpointDecision::Inject(parts) if parts.is_empty() => {
                                        cp_span.record("decision", "inject");
                                    }
                                    crate::CheckpointDecision::Inject(parts) => {
                                        cp_span.record("decision", "inject");
                                        // Allow-list: `ContentPart` is `#[non_exhaustive]`,
                                        // so a new variant must fail until it is allowed here.
                                        let bad = parts.iter().find_map(|p| match p {
                                            crate::ContentPart::Text { .. }
                                            | crate::ContentPart::Image { .. }
                                            | crate::ContentPart::Audio { .. } => None,
                                            crate::ContentPart::ToolUse { .. } => Some("ToolUse"),
                                            crate::ContentPart::ToolResult { .. } => Some("ToolResult"),
                                            crate::ContentPart::Reasoning { .. } => Some("Reasoning"),
                                            // Unreachable in this crate today. A new variant
                                            // lands here until it is allowed above.
                                            #[allow(unreachable_patterns)]
                                            _ => Some("unsupported"),
                                        });
                                        if let Some(kind) = bad {
                                            let err = crate::AgentError::InvalidInjection {
                                                reason: format!(
                                                    "injected content contains a {kind} part; only text, image and audio parts are allowed"
                                                ),
                                            };
                                            let msg = err.to_string();
                                            run_span.record("otel.status_code", "ERROR");
                                            failure.set(err);
                                            yield crate::AgentEvent::RunFailed { error: msg };
                                            return;
                                        }
                                        let item = crate::Item::UserMessage { content: parts };
                                        let text = user_text_of(std::slice::from_ref(&item));
                                        if let Some((kind, info)) =
                                            interceptors.run_input_guardrails(&text).await
                                        {
                                            yield crate::AgentEvent::GuardrailTriggered {
                                                kind,
                                                info,
                                            };
                                            stop_reason = Some("input guardrail tripwire".to_owned());
                                        } else {
                                            if let crate::Item::UserMessage { content } = &item {
                                                yield crate::AgentEvent::UserMessageInjected {
                                                    content: content.clone(),
                                                };
                                            }
                                            conversation.push(item.clone());
                                            if let crate::NextAction::CallModel { request } =
                                                &mut next_action
                                            {
                                                request.messages.push(item);
                                            }
                                        }
                                    }
                                }
                                if let Some(reason) = stop_reason {
                                    // A clean stop: not an error, so the failure
                                    // slot stays empty and `OnRunComplete` fires
                                    // through the `Terminate` arm.
                                    let stop_usage = match &loop_state {
                                        crate::LoopState::ExecutingTools { usage, .. } => *usage,
                                        _ => TokenUsage::default(),
                                    };
                                    run_span.record(
                                        "gen_ai.usage.input_tokens",
                                        stop_usage.input_tokens as i64,
                                    );
                                    run_span.record(
                                        "gen_ai.usage.output_tokens",
                                        stop_usage.output_tokens as i64,
                                    );
                                    run_span.record("paigasus.stop_reason", reason.as_str());
                                    yield crate::AgentEvent::RunStopped {
                                        reason,
                                        usage: stop_usage,
                                    };
                                    next_action = crate::NextAction::Terminate;
                                    break;
                                }
                            }
                            let s = tracing::info_span!(
                                target: "paigasus::core::agent",
                                parent: &run_span,
                                "agent.turn",
                                otel.kind = "internal",
                                turn = *turn,
                                langfuse.session.id = tracing::field::Empty,
                                langfuse.user.id = tracing::field::Empty,
                                langfuse.trace.tags = tracing::field::Empty,
                            );
                            if let Some(v) = ctx.tracer().session_id() {
                                s.record("langfuse.session.id", v);
                            }
                            if let Some(v) = ctx.tracer().user_id() {
                                s.record("langfuse.user.id", v);
                            }
                            if !ctx.tracer().tags().is_empty() {
                                if let Ok(json) = serde_json::to_string(ctx.tracer().tags()) {
                                    s.record("langfuse.trace.tags", json.as_str());
                                }
                            }
                            let on_turn = interceptors
                                .fire(&crate::HookEvent::OnTurnStart { turn: *turn })
                                .await;
                            if let Some(reason) = on_turn.denied {
                                let err = crate::AgentError::HookDenied {
                                    event: "OnTurnStart".to_owned(),
                                    reason,
                                };
                                let msg = err.to_string();
                                run_span.record("otel.status_code", "ERROR");
                                failure.set(err);
                                yield crate::AgentEvent::RunFailed { error: msg };
                                return;
                            }
                            pending_injections.extend(on_turn.injections);
                            turn_span = Some(s);
                        }
                        crate::AgentEvent::RunCompleted { usage } => {
                            run_span.record("gen_ai.usage.input_tokens", usage.input_tokens as i64);
                            run_span.record("gen_ai.usage.output_tokens", usage.output_tokens as i64);
                        }
                        crate::AgentEvent::RunDeferred { calls, usage } => {
                            run_span.record("gen_ai.usage.input_tokens", usage.input_tokens as i64);
                            run_span.record("gen_ai.usage.output_tokens", usage.output_tokens as i64);
                            run_span.record("paigasus.deferred_calls", calls.len() as i64);
                        }
                        crate::AgentEvent::RunFailed { .. } => {
                            run_span.record("otel.status_code", "ERROR");
                        }
                        _ => {}
                    }
                    yield ev;
                }
                loop_state = next_state;
                conversation.extend(conversation_appends);

                match next_action {
                    crate::NextAction::CallModel { request } => {
                        let mut request = request;
                        for text in pending_injections.drain(..) {
                            request.messages.push(crate::Item::System {
                                content: vec![crate::ContentPart::Text { text }],
                            });
                        }
                        let chat_parent = turn_span.as_ref().unwrap_or(&run_span);
                        let chat_span = tracing::info_span!(
                            target: "paigasus::core::agent",
                            parent: chat_parent,
                            "gen_ai.chat",
                            otel.name = tracing::field::Empty,
                            otel.kind = "client",
                            gen_ai.operation.name = "chat",
                            gen_ai.provider.name = %model.provider(),
                            gen_ai.request.model = %model.model(),
                            gen_ai.usage.input_tokens = tracing::field::Empty,
                            gen_ai.usage.output_tokens = tracing::field::Empty,
                            otel.status_code = tracing::field::Empty,
                        );
                        chat_span.record("otel.name", format!("chat {}", model.model()).as_str());
                        let cancel = ctx.cancel().clone();
                        let mut model_stream = match model.invoke(request, cancel).await {
                            Ok(s) => s,
                            Err(e) => {
                                let msg = e.to_string();
                                chat_span.record("otel.status_code", "ERROR");
                                run_span.record("otel.status_code", "ERROR");
                                failure.set(crate::AgentError::Model(e));
                                yield crate::AgentEvent::RunFailed { error: msg };
                                return;
                            }
                        };

                        let mut acc = crate::ModelTurnAccumulator::new(agent_name.clone());

                        while let Some(evt) = model_stream.next().await {
                            match evt {
                                Ok(ev) => {
                                    acc.observe(&ev);
                                    match ev {
                                        crate::ModelEvent::TokenDelta { text } => {
                                            yield crate::AgentEvent::TokenDelta { text };
                                        }
                                        crate::ModelEvent::ReasoningDelta { text } => {
                                            yield crate::AgentEvent::ReasoningDelta { text };
                                        }
                                        crate::ModelEvent::ToolCallDelta {
                                            call_id,
                                            name,
                                            args_delta,
                                        } => {
                                            yield crate::AgentEvent::ToolCallDelta {
                                                call_id,
                                                name,
                                                args_delta,
                                            };
                                        }
                                        crate::ModelEvent::Usage { .. }
                                        | crate::ModelEvent::Finish { .. } => {}
                                    }
                                }
                                Err(e) => {
                                    let msg = e.to_string();
                                    chat_span.record("otel.status_code", "ERROR");
                                    run_span.record("otel.status_code", "ERROR");
                                    failure.set(crate::AgentError::Model(e));
                                    yield crate::AgentEvent::RunFailed { error: msg };
                                    return;
                                }
                            }
                        }

                        let crate::ModelTurn { items, usage, finish_reason } = match acc.finish() {
                            Ok(turn) => turn,
                            Err(e) => {
                                chat_span.record("otel.status_code", "ERROR");
                                run_span.record("otel.status_code", "ERROR");
                                failure.set(crate::AgentError::Other(anyhow::anyhow!("{e}")));
                                yield crate::AgentEvent::RunFailed { error: e };
                                return;
                            }
                        };
                        conversation.extend(items.iter().cloned());
                        // Per-turn chat span records the FINAL retained Usage snapshot
                        // (Anthropic emits incremental updates; retain the LAST, never sum
                        // within a turn). Cross-turn run totals now accumulate inside the
                        // state machine (SMA-402) and arrive on RunCompleted.usage.
                        chat_span.record("gen_ai.usage.input_tokens", usage.input_tokens as i64);
                        chat_span.record("gen_ai.usage.output_tokens", usage.output_tokens as i64);
                        tx_input = crate::TransitionInput::ModelResponse {
                            items,
                            usage,
                            finish_reason,
                        };
                    }
                    crate::NextAction::ExecuteTools { calls } => {
                        let tool_ctx = ctx.to_tool_context();
                        let tool_parent = turn_span.as_ref().unwrap_or(&run_span);
                        let (results, denied) = run_tools_concurrent(
                            &tools,
                            &calls,
                            &interceptors,
                            &tool_ctx,
                            parallel_tool_call_limit,
                            tool_parent,
                        )
                        .await;
                        for ev in denied {
                            yield ev;
                        }
                        let mut outcomes = Vec::with_capacity(results.len());
                        let mut raw_pendings = Vec::new();
                        for r in results {
                            match r {
                                ConcurrentOutcome::Done(o) => {
                                    conversation.push(crate::Item::ToolResult {
                                        call_id: o.call_id.clone(),
                                        content: o.result.clone().unwrap_or_else(|e| {
                                            vec![crate::ContentPart::Text { text: e }]
                                        }),
                                    });
                                    outcomes.push(o);
                                }
                                ConcurrentOutcome::Deferred(p) => raw_pendings.push(p),
                            }
                        }
                        tx_input = if raw_pendings.is_empty() {
                            crate::TransitionInput::ToolResults { outcomes }
                        } else {
                            // The events reach clients, so they carry redacted
                            // args and prompts. The slot keeps the raw calls
                            // for the session recorder and the resume.
                            let event_copies: Vec<crate::PendingToolCall> = if ctx.redact_output() {
                                let secrets = crate::redaction::SecretSet::from_env_and_extra(
                                    ctx.extra_secrets(),
                                );
                                raw_pendings
                                    .iter()
                                    .map(|p| p.redacted(&secrets))
                                    .collect()
                            } else {
                                raw_pendings.clone()
                            };
                            ctx.deferral_handle().record(raw_pendings);
                            crate::TransitionInput::ToolResultsWithDeferrals {
                                outcomes,
                                deferred: event_copies,
                            }
                        };
                    }
                    crate::NextAction::Terminate => {
                        let _ = interceptors.fire(&crate::HookEvent::OnRunComplete).await;
                        // On a terminal failure the driver left the structured
                        // error in loop_state; hand it to the slot. (Every
                        // LoopState::Failed branch in loop_state.rs Terminates,
                        // so this is the single capture point for all of them.)
                        // This runs AFTER the RunFailed event was yielded, which
                        // is why the boundary must drain-then-read.
                        if let crate::LoopState::Failed(err) = loop_state {
                            failure.set(err);
                        }
                        return;
                    }
                    crate::NextAction::Handoff => {
                        let (target, transcript, parent_usage) = match loop_state {
                            crate::LoopState::ApplyingHandoff {
                                target,
                                transcript,
                                usage,
                            } => (target, transcript, usage),
                            _ => return,
                        };

                        let child = ctx
                            .handoff_child()
                            .with_open_call_ids(transfer_call_ids.clone());
                        if child.agent_depth() > max_agent_depth {
                            let err = crate::AgentError::MaxAgentDepthExceeded {
                                depth: child.agent_depth(),
                                max: max_agent_depth,
                            };
                            let msg = err.to_string();
                            run_span.record("otel.status_code", "ERROR");
                            failure.set(err);
                            yield crate::AgentEvent::RunFailed { error: msg };
                            return;
                        }

                        let Some(target_agent) = handoffs
                            .iter()
                            .find(|h| h.agent().name() == target)
                            .map(|h| std::sync::Arc::clone(h.agent()))
                        else {
                            let err = crate::AgentError::Other(anyhow::anyhow!(
                                "unknown handoff target: {target}"
                            ));
                            let msg = err.to_string();
                            run_span.record("otel.status_code", "ERROR");
                            failure.set(err);
                            yield crate::AgentEvent::RunFailed { error: msg };
                            return;
                        };

                        // Fire OnHandoff BEFORE emitting the transition events, so a
                        // Deny vetoes the handoff without consumers observing a
                        // completed agent switch.
                        let on_handoff = interceptors
                            .fire(&crate::HookEvent::OnHandoff {
                                from: agent_name.clone(),
                                to: target.clone(),
                            })
                            .await;
                        if let Some(reason) = on_handoff.denied {
                            let err = crate::AgentError::HookDenied {
                                event: "OnHandoff".to_owned(),
                                reason,
                            };
                            let msg = err.to_string();
                            run_span.record("otel.status_code", "ERROR");
                            failure.set(err);
                            yield crate::AgentEvent::RunFailed { error: msg };
                            return;
                        }

                        yield crate::AgentEvent::HandoffItem {
                            from: agent_name.clone(),
                            to: target.clone(),
                        };

                        let input = crate::AgentInput { messages: transcript, ..Default::default() };
                        // Starting the child emits no event. Start it before the
                        // transfer results, because their text tells whether
                        // the target started.
                        let started = target_agent.run(child, input).await;
                        for item in transfer_results(&transfer_call_ids, &target, started.is_ok()) {
                            yield crate::AgentEvent::ToolOutputItem { item };
                        }
                        let mut sub = match started {
                            Ok(s) => s,
                            Err(e) => {
                                let msg = e.to_string();
                                run_span.record("otel.status_code", "ERROR");
                                failure.set(e);
                                yield crate::AgentEvent::RunFailed { error: msg };
                                return;
                            }
                        };
                        yield crate::AgentEvent::AgentUpdated {
                            agent: target.clone(),
                        };
                        while let Some(ev) = sub.next().await {
                            if let Some(ev) = reyield_child_event(ev, parent_usage, &run_span) {
                                yield ev;
                            }
                        }
                        let _ = interceptors
                            .fire(&crate::HookEvent::OnSubagentStop { agent: target.clone() })
                            .await;
                        return;
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

/// The tool results of the transfer calls of one handoff, in call order.
///
/// `started` tells whether the target agent's `run` returned `Ok`. When it
/// did, the first call gets `transferred to {target}`, and each other call
/// of the same response gets a "transfer not performed" text. When it did
/// not, every call gets `transfer failed: {target} did not start`. The text
/// holds no error detail, because the result goes to a model.
fn transfer_results(call_ids: &[String], target: &str, started: bool) -> Vec<crate::Item> {
    call_ids
        .iter()
        .enumerate()
        .map(|(i, call_id)| {
            let text = if !started {
                format!("transfer failed: {target} did not start")
            } else if i == 0 {
                format!("transferred to {target}")
            } else {
                format!("transfer not performed: this response already transferred to {target}")
            };
            crate::Item::ToolResult {
                call_id: call_id.clone(),
                content: vec![crate::ContentPart::Text { text }],
            }
        })
        .collect()
}

// ── Error types ───────────────────────────────────────────────────────────────

/// Errors raised by [`Agent::run`] or [`crate::Runner`] methods.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    /// A downstream model call failed.
    #[error("model failed: {0}")]
    Model(#[from] ModelError),

    /// A downstream tool call failed.
    #[error("tool failed: {0}")]
    Tool(#[from] ToolError),

    /// A session-backend call failed.
    #[error("session failed: {0}")]
    Session(#[from] SessionError),

    /// A guardrail tripwire fired and halted the run.
    #[error("guardrail tripped: {kind:?}")]
    Guardrail {
        /// Which kind of tripwire fired.
        kind: GuardrailKind,
    },

    /// A hook denied a lifecycle event, aborting the run.
    #[error("hook denied {event}: {reason}")]
    HookDenied {
        /// The lifecycle event that was denied (e.g. `"OnRunStart"`).
        event: String,
        /// Reason surfaced by the hook.
        reason: String,
    },

    /// The model produced output that could not be coerced into the
    /// requested structured type, even after the one-shot repair attempt
    /// allowed by ADR-10.
    #[error("invalid structured output after one repair attempt: {schema_errors:?}")]
    InvalidStructuredOutput {
        /// Human-readable schema/validation errors.
        schema_errors: Vec<String>,
        /// The raw terminal assistant text that failed validation.
        final_text: String,
    },

    /// New in SMA-314: `max_turns` budget exhausted.
    #[error("max turns ({0}) exceeded")]
    MaxTurnsExceeded(u32),

    /// New in SMA-325: a [`crate::LoopAgent`] ran `max_iterations` without a
    /// sub-agent escalating.
    #[error("max iterations ({max}) exceeded")]
    MaxIterationsExceeded {
        /// The configured iteration budget.
        max: u32,
    },

    /// New in SMA-314: reached a `LoopState` variant SMA-314 does not
    /// yet drive (handoff, compaction, approval).
    #[error("not yet implemented: {feature}")]
    NotImplemented {
        /// The unimplemented loop feature.
        feature: &'static str,
    },

    /// New in SMA-804: a [`crate::TurnCheckpoint`] returned content that cannot
    /// be injected as a user message (a tool-use, tool-result or reasoning part).
    #[error("invalid injection: {reason}")]
    InvalidInjection {
        /// Why the injected content was rejected.
        reason: String,
    },

    /// A handoff chain or `AgentAsTool` nesting exceeded
    /// [`crate::RunConfig::max_agent_depth`].
    #[error("agent nesting depth ({depth}) exceeded max ({max})")]
    MaxAgentDepthExceeded {
        /// The depth that would have been entered.
        depth: u32,
        /// The configured maximum.
        max: u32,
    },

    /// A [`crate::SwarmAgent`] exceeded its configured handoff budget
    /// before any member produced a final output.
    #[error("max handoffs ({limit}) exceeded")]
    MaxHandoffsExceeded {
        /// The configured budget that was exceeded.
        limit: u32,
    },

    /// The run stopped without a final output (for example, a deferral or a
    /// turn checkpoint stop).
    #[error("run did not complete")]
    Incomplete {
        /// Why the run stopped.
        stop_reason: crate::StopReason,
    },

    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Out-of-band carrier for a run's terminal structured [`AgentError`].
///
/// The [`crate::AgentEvent`] stream stays string-based
/// ([`crate::AgentEvent::RunFailed`]` { error: String }`) so it remains `Clone`
/// and snapshot-stable; the structured error rides this side-channel instead.
/// One slot lives on each [`RunContext`]; the agent records into it at the
/// moment of failure and a [`crate::Runner`] (or
/// [`crate::RunResultStreaming::collect`]) reads it **after the event stream is
/// fully drained** — see [`crate::RunResultStreaming::collect`] for why the
/// read must come after draining.
#[derive(Clone, Default, Debug)]
pub struct FailureSlot(Arc<Mutex<Option<AgentError>>>);

impl FailureSlot {
    /// Create an empty slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the terminal structured error. Called once per run, at any point
    /// before the stream terminates; last write wins.
    pub fn set(&self, err: AgentError) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(err);
    }

    /// Take the recorded error, if any. Read once at the boundary, after the
    /// event stream has been fully drained.
    pub fn take(&self) -> Option<AgentError> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

// A non-`Send`/`Sync` payload added to `AgentError` would silently break the
// agent's `BoxStream<'static, AgentEvent>: Send` bound far downstream. Fail here
// instead, with a clear pointer to the cause.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FailureSlot>();
};

#[cfg(test)]
mod failure_slot_tests {
    use super::{AgentError, FailureSlot};

    #[test]
    fn set_then_take_returns_the_error() {
        let slot = FailureSlot::new();
        assert!(slot.take().is_none(), "empty slot yields None");
        slot.set(AgentError::MaxTurnsExceeded(3));
        match slot.take() {
            Some(AgentError::MaxTurnsExceeded(n)) => assert_eq!(n, 3),
            other => panic!("expected MaxTurnsExceeded(3), got {other:?}"),
        }
        assert!(slot.take().is_none(), "take() drains the slot");
    }

    #[test]
    fn clone_shares_the_same_slot() {
        let a = FailureSlot::new();
        let b = a.clone();
        b.set(AgentError::NotImplemented { feature: "handoff" });
        assert!(
            matches!(
                a.take(),
                Some(AgentError::NotImplemented { feature: "handoff" })
            ),
            "a clone observes a write through the original handle"
        );
    }

    #[test]
    fn set_overwrites_previous() {
        let slot = FailureSlot::new();
        slot.set(AgentError::MaxTurnsExceeded(1));
        slot.set(AgentError::MaxTurnsExceeded(2));
        assert!(matches!(slot.take(), Some(AgentError::MaxTurnsExceeded(2))));
    }

    #[test]
    fn max_iterations_exceeded_displays() {
        assert_eq!(
            AgentError::MaxIterationsExceeded { max: 3 }.to_string(),
            "max iterations (3) exceeded"
        );
    }
}

#[cfg(test)]
mod error_display_tests {
    use crate::AgentError;

    #[test]
    fn hook_denied_displays() {
        let e = AgentError::HookDenied {
            event: "OnRunStart".into(),
            reason: "blocked".into(),
        };
        assert_eq!(e.to_string(), "hook denied OnRunStart: blocked");
    }
}

#[cfg(test)]
mod output_type_tests {
    use super::OutputType;
    use serde_json::json;

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Answer {
        value: u32,
    }

    #[test]
    fn from_schema_populates_name_and_schema() {
        let ot = OutputType::from_schema::<Answer>();
        assert_eq!(ot.name, "Answer");
        // schema is the schemars schema for Answer
        let v = serde_json::to_value(&ot.schema).unwrap();
        assert_eq!(v["properties"]["value"]["type"], json!("integer"));
    }

    #[test]
    fn validate_accepts_conformant_and_rejects_nonconformant() {
        let ot = OutputType::from_schema::<Answer>();
        assert!(ot.validate(&json!({"value": 7})).is_ok());
        let err = ot.validate(&json!({"value": "not a number"})).unwrap_err();
        assert!(!err.is_empty(), "expected at least one error string");
    }
}

#[cfg(test)]
mod redaction_e2e_tests {
    use super::*;
    use crate::{
        CancellationToken, ContentPart, HookRegistry, MemorySession, RunContext, Session, Tool,
        ToolContext, ToolError, ToolOutput, TracerHandle,
    };
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;

    struct SecretTool;

    #[async_trait]
    impl Tool<()> for SecretTool {
        fn name(&self) -> &str {
            "secret"
        }

        fn description(&self) -> &str {
            "returns a secret"
        }

        fn schema(&self) -> &serde_json::Value {
            use std::sync::OnceLock;
            static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
            SCHEMA.get_or_init(|| json!({ "type": "object" }))
        }

        async fn invoke(
            &self,
            _ctx: &ToolContext<()>,
            _args: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new(
                json!({ "stdout": "FOO_API_KEY=supersecretvalue" }),
            ))
        }
    }

    #[tokio::test]
    async fn tool_output_secret_is_redacted_before_model() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        let tool_ctx = ctx.to_tool_context();
        let interceptors = crate::control::Interceptors {
            ctx: &ctx,
            input_guardrails: &[],
            output_guardrails: &[],
            agent_hooks: &[],
        };
        let tools: Vec<Arc<dyn Tool<()>>> = vec![Arc::new(SecretTool)];
        let calls = vec![crate::ToolCallRequest {
            call_id: "c1".to_owned(),
            name: "secret".to_owned(),
            args: json!({}),
        }];
        let span = tracing::Span::none();
        let (outcomes, _events) =
            run_tools_concurrent(&tools, &calls, &interceptors, &tool_ctx, None, &span).await;

        assert_eq!(outcomes.len(), 1);
        let ConcurrentOutcome::Done(outcome) = &outcomes[0] else {
            panic!("expected a Done outcome, got {:?}", outcomes[0]);
        };
        let parts = outcome.result.as_ref().expect("tool ran ok");
        // Collect all text across content parts and assert redaction.
        let rendered: String = parts
            .iter()
            .filter_map(|p| {
                if let ContentPart::Text { text } = p {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");
        assert!(
            rendered.contains("FOO_API_KEY=***"),
            "expected redacted key, got: {rendered}"
        );
        assert!(
            !rendered.contains("supersecretvalue"),
            "secret leaked into tool output: {rendered}"
        );
    }
}

#[cfg(test)]
mod terminal_tests {
    use super::*;

    /// Independent classification of every [`AgentEvent`] variant.
    ///
    /// The `match` is deliberately **exhaustive, with no wildcard arm**.
    /// `#[non_exhaustive]` has no effect inside the defining crate, so adding a
    /// variant to `AgentEvent` fails to compile *here* until someone makes an
    /// explicit terminal / non-terminal decision for it. That is what stops a
    /// newly added terminal variant from silently defaulting to non-terminal
    /// inside `AgentEvent::is_terminal`'s `matches!`.
    fn classify(ev: &AgentEvent) -> bool {
        match ev {
            AgentEvent::RunCompleted { .. }
            | AgentEvent::RunFailed { .. }
            | AgentEvent::RunDeferred { .. }
            | AgentEvent::RunStopped { .. } => true,
            AgentEvent::RunStarted { .. }
            | AgentEvent::TurnStarted { .. }
            | AgentEvent::TokenDelta { .. }
            | AgentEvent::ReasoningDelta { .. }
            | AgentEvent::ToolCallDelta { .. }
            | AgentEvent::MessageOutput { .. }
            | AgentEvent::ToolCallItem { .. }
            | AgentEvent::ToolOutputItem { .. }
            | AgentEvent::HandoffItem { .. }
            | AgentEvent::AgentUpdated { .. }
            | AgentEvent::GuardrailTriggered { .. }
            | AgentEvent::ApprovalRequested { .. }
            | AgentEvent::PermissionDenied { .. }
            | AgentEvent::RepairStarted { .. }
            | AgentEvent::UserMessageInjected { .. }
            | AgentEvent::StructuredOutputFailed { .. } => false,
        }
    }

    fn sample_item() -> Item {
        Item::AssistantMessage {
            content: vec![crate::ContentPart::Text {
                text: "hi".to_owned(),
            }],
            agent: None,
        }
    }

    /// One instance of every variant, so the two classifications are compared
    /// across the whole surface rather than a hand-picked sample.
    fn every_variant() -> Vec<AgentEvent> {
        vec![
            AgentEvent::RunStarted {
                agent: "a".to_owned(),
            },
            AgentEvent::TurnStarted { turn: 0 },
            AgentEvent::TokenDelta {
                text: "t".to_owned(),
            },
            AgentEvent::ReasoningDelta {
                text: "r".to_owned(),
            },
            AgentEvent::ToolCallDelta {
                call_id: "c".to_owned(),
                name: None,
                args_delta: "{}".to_owned(),
            },
            AgentEvent::MessageOutput {
                item: sample_item(),
            },
            AgentEvent::ToolCallItem {
                item: sample_item(),
            },
            AgentEvent::ToolOutputItem {
                item: sample_item(),
            },
            AgentEvent::HandoffItem {
                from: "a".to_owned(),
                to: "b".to_owned(),
            },
            AgentEvent::AgentUpdated {
                agent: "b".to_owned(),
            },
            AgentEvent::GuardrailTriggered {
                kind: GuardrailKind::InputPolicy,
                info: serde_json::Value::Null,
            },
            AgentEvent::ApprovalRequested {
                call_id: "c".to_owned(),
                tool: "t".to_owned(),
                args: serde_json::Value::Null,
            },
            AgentEvent::PermissionDenied {
                tool: "t".to_owned(),
                reason: "no".to_owned(),
            },
            AgentEvent::RepairStarted { attempt: 1 },
            AgentEvent::StructuredOutputFailed {
                schema_errors: vec!["e".to_owned()],
                final_text: "x".to_owned(),
            },
            AgentEvent::UserMessageInjected {
                content: vec![crate::ContentPart::Text {
                    text: "hi".to_owned(),
                }],
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
            AgentEvent::RunFailed {
                error: "boom".to_owned(),
            },
            AgentEvent::RunDeferred {
                calls: vec![],
                usage: TokenUsage::default(),
            },
            AgentEvent::RunStopped {
                reason: "stop".to_owned(),
                usage: TokenUsage::default(),
            },
        ]
    }

    #[test]
    fn every_variant_covers_the_whole_enum() {
        // Assert on *distinct* discriminants, not length: a plain count would
        // also pass for 20 copies of one variant, letting a newly added variant
        // slip past `is_terminal_agrees_with_the_exhaustive_classification`.
        let discriminants: std::collections::HashSet<_> =
            every_variant().iter().map(std::mem::discriminant).collect();
        assert_eq!(
            discriminants.len(),
            20,
            "every_variant() must construct one instance of each distinct AgentEvent variant"
        );
    }

    #[test]
    fn is_terminal_agrees_with_the_exhaustive_classification() {
        for ev in &every_variant() {
            assert_eq!(
                ev.is_terminal(),
                classify(ev),
                "{ev:?}: is_terminal disagrees with the exhaustive classification"
            );
        }
    }

    #[test]
    fn with_resolution_accumulates_in_order() {
        let i = AgentInput::new()
            .with_resolution("a", crate::ToolResolution::Approved)
            .with_resolution(
                "b",
                crate::ToolResolution::Denied {
                    reason: "no".into(),
                },
            );
        assert_eq!(i.resolutions().len(), 2);
        assert_eq!(i.resolutions()[0].0, "a");
    }

    #[test]
    fn exactly_four_variants_are_terminal() {
        let terminal: Vec<_> = every_variant()
            .into_iter()
            .filter(AgentEvent::is_terminal)
            .collect();
        assert_eq!(
            terminal.len(),
            4,
            "expected exactly RunCompleted + RunFailed + RunDeferred + RunStopped: {terminal:?}"
        );
    }
}
