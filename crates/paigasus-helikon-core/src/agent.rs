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
        }
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
#[derive(Clone)]
pub struct OutputType {
    /// The schema name (the `T` identifier / schema title). Echoed into the
    /// provider `response_format` name and into the repair instruction.
    pub name: String,
    /// The JSON Schema the model should produce (raw schemars output).
    pub schema: schemars::Schema,
    /// Authoritative validator: `Ok(())` iff the value deserializes into the
    /// original `T`; `Err` carries one or more human-readable error strings.
    validate: fn(&serde_json::Value) -> Result<(), Vec<String>>,
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
/// Fourteen variants spanning lifecycle, raw streaming deltas,
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
        /// Tool name; `Some` on the first delta only.
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
        /// JSON arguments the model proposed to call the tool with.
        args: serde_json::Value,
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
}

// ── Private helpers for the LlmAgent driver ─────────────────────────────────

/// Accumulates the in-progress tool call across `ModelEvent::ToolCallDelta` chunks.
#[derive(Default)]
struct ToolCallAccum {
    name: Option<String>,
    args_str: String,
}

/// Reassemble streamed model output into [`Item`]s.
fn build_items(
    agent_name: &str,
    text: String,
    reasoning: String,
    tool_accum: std::collections::BTreeMap<String, ToolCallAccum>,
) -> Result<Vec<crate::Item>, String> {
    let mut items = Vec::new();
    if !text.is_empty() || !reasoning.is_empty() {
        let mut content = Vec::new();
        if !reasoning.is_empty() {
            content.push(crate::ContentPart::Reasoning { text: reasoning });
        }
        if !text.is_empty() {
            content.push(crate::ContentPart::Text { text });
        }
        items.push(crate::Item::AssistantMessage {
            content,
            agent: Some(agent_name.to_owned()),
        });
    }
    for (call_id, accum) in tool_accum {
        let args = serde_json::from_str(&accum.args_str).map_err(|e| {
            format!(
                "invalid tool args for call_id={call_id} (name={}): {e}",
                accum.name.as_deref().unwrap_or("?")
            )
        })?;
        items.push(crate::Item::ToolCall {
            call_id,
            name: accum.name.unwrap_or_default(),
            args,
        });
    }
    Ok(items)
}

/// Conversion convention: `ToolOutput.content` (SMA-313's
/// `serde_json::Value`) becomes one `ContentPart::Text`.
/// `Value::String(s) -> ContentPart::Text { text: s }`; other JSON
/// values are stringified via `Value::to_string()`.
fn tool_output_to_content_parts(output: &crate::ToolOutput) -> Vec<crate::ContentPart> {
    let text = match &output.content {
        serde_json::Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    vec![crate::ContentPart::Text { text }]
}

async fn run_tools_concurrent<Ctx>(
    tools: &[std::sync::Arc<dyn crate::Tool<Ctx>>],
    calls: &[crate::ToolCallRequest],
    tool_ctx: &crate::ToolContext<Ctx>,
    limit: Option<std::num::NonZeroUsize>,
    parent: &tracing::Span,
) -> Vec<crate::ToolCallOutcome>
where
    Ctx: Send + Sync + 'static,
{
    let futures = calls.iter().map(|call| {
        let tool = tools.iter().find(|t| t.name() == call.name).cloned();
        let call_id = call.call_id.clone();
        let args = call.args.clone();
        let name = call.name.clone();
        let span = tracing::info_span!(
            parent: parent,
            "tool.execute",
            otel.name = tracing::field::Empty,
            otel.kind = "internal",
            gen_ai.operation.name = "execute_tool",
            gen_ai.tool.name = %name,
            otel.status_code = tracing::field::Empty,
        );
        span.record("otel.name", format!("execute_tool {name}").as_str());
        async move {
            match tool {
                Some(t) => match t.invoke(tool_ctx, args).await {
                    Ok(output) => crate::ToolCallOutcome {
                        call_id,
                        result: Ok(tool_output_to_content_parts(&output)),
                    },
                    Err(e) => {
                        tracing::Span::current().record("otel.status_code", "ERROR");
                        crate::ToolCallOutcome {
                            call_id,
                            result: Err(e.to_string()),
                        }
                    }
                },
                None => {
                    tracing::Span::current().record("otel.status_code", "ERROR");
                    crate::ToolCallOutcome {
                        call_id,
                        result: Err(format!("unknown tool: {name}")),
                    }
                }
            }
        }
        .instrument(span)
    });
    match limit {
        None => futures_util::future::join_all(futures).await,
        Some(n) => {
            use futures_util::stream::StreamExt as _;
            // Collect to a Vec first: passing the `Map` iterator directly to
            // `stream::iter` trips an HRTB lifetime bound that `join_all` (above)
            // doesn't impose. `buffered` (not `buffer_unordered`) preserves call
            // order in the outcomes. Don't "simplify" this back to a chained call.
            let collected: Vec<_> = futures.collect();
            futures_util::stream::iter(collected)
                .buffered(n.get())
                .collect()
                .await
        }
    }
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
        let max_agent_depth = effective_config.max_agent_depth;

        let stream = async_stream::stream! {
            // SMA-346: structured failures are recorded here and read by the
            // boundary after the stream drains (see RunResultStreaming::collect).
            // Invariant: every terminal-failure path must `failure.set(...)`
            // before it `return`s (direct sites), or rely on the `Terminate`
            // arm (state-machine sites via LoopState::Failed).
            let failure = ctx.failure_handle();

            // Seed conversation: optional system message + user input.
            let mut conversation: Vec<crate::Item> = Vec::new();
            if !instructions_text.is_empty() {
                conversation.push(crate::Item::System {
                    content: vec![crate::ContentPart::Text { text: instructions_text }],
                });
            }
            conversation.extend(input.messages.iter().cloned());

            let mut loop_state = crate::LoopState::CallingModel { turn: 0, usage: crate::TokenUsage::default() };
            let mut tx_input = crate::TransitionInput::Start { messages: input.messages };

            let run_span = tracing::info_span!(
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
                        yield crate::AgentEvent::RunFailed { error: msg };
                        return;
                    }
                }
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
                for ev in events {
                    match &ev {
                        crate::AgentEvent::TurnStarted { turn } => {
                            let s = tracing::info_span!(
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
                            turn_span = Some(s);
                        }
                        crate::AgentEvent::RunCompleted { usage } => {
                            run_span.record("gen_ai.usage.input_tokens", usage.input_tokens as i64);
                            run_span.record("gen_ai.usage.output_tokens", usage.output_tokens as i64);
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
                        let chat_parent = turn_span.as_ref().unwrap_or(&run_span);
                        let chat_span = tracing::info_span!(
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

                        let mut text = String::new();
                        let mut reasoning = String::new();
                        let mut tool_accum: std::collections::BTreeMap<String, ToolCallAccum> =
                            std::collections::BTreeMap::new();
                        let mut finish_reason = crate::FinishReason::Stop;
                        let mut latest_usage: Option<crate::TokenUsage> = None;

                        while let Some(evt) = model_stream.next().await {
                            match evt {
                                Ok(crate::ModelEvent::TokenDelta { text: t }) => {
                                    text.push_str(&t);
                                    yield crate::AgentEvent::TokenDelta { text: t };
                                }
                                Ok(crate::ModelEvent::ReasoningDelta { text: t }) => {
                                    reasoning.push_str(&t);
                                    yield crate::AgentEvent::ReasoningDelta { text: t };
                                }
                                Ok(crate::ModelEvent::ToolCallDelta {
                                    call_id,
                                    name,
                                    args_delta,
                                }) => {
                                    let a = tool_accum.entry(call_id.clone()).or_default();
                                    if a.name.is_none() {
                                        if let Some(n) = name.as_deref() {
                                            a.name = Some(n.into());
                                        }
                                    }
                                    a.args_str.push_str(&args_delta);
                                    yield crate::AgentEvent::ToolCallDelta {
                                        call_id,
                                        name,
                                        args_delta,
                                    };
                                }
                                Ok(crate::ModelEvent::Usage {
                                    input_tokens,
                                    output_tokens,
                                    cached_input_tokens,
                                    reasoning_tokens,
                                }) => {
                                    latest_usage = Some(crate::TokenUsage {
                                        input_tokens: u64::from(input_tokens),
                                        output_tokens: u64::from(output_tokens),
                                        cached_input_tokens: cached_input_tokens
                                            .map(u64::from)
                                            .unwrap_or(0),
                                        reasoning_tokens: reasoning_tokens
                                            .map(u64::from)
                                            .unwrap_or(0),
                                        total_tokens: u64::from(input_tokens)
                                            + u64::from(output_tokens),
                                    });
                                }
                                Ok(crate::ModelEvent::Finish { reason }) => {
                                    finish_reason = reason;
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

                        let items = match build_items(&agent_name, text, reasoning, tool_accum) {
                            Ok(items) => items,
                            Err(e) => {
                                chat_span.record("otel.status_code", "ERROR");
                                run_span.record("otel.status_code", "ERROR");
                                failure.set(crate::AgentError::Other(anyhow::anyhow!("{e}")));
                                yield crate::AgentEvent::RunFailed { error: e };
                                return;
                            }
                        };
                        conversation.extend(items.iter().cloned());
                        let usage = latest_usage.unwrap_or_default();
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
                        let outcomes = run_tools_concurrent(
                            &tools,
                            &calls,
                            &tool_ctx,
                            parallel_tool_call_limit,
                            tool_parent,
                        )
                        .await;
                        for o in &outcomes {
                            conversation.push(crate::Item::ToolResult {
                                call_id: o.call_id.clone(),
                                content: o.result.clone().unwrap_or_else(|e| {
                                    vec![crate::ContentPart::Text { text: e }]
                                }),
                            });
                        }
                        tx_input = crate::TransitionInput::ToolResults { outcomes };
                    }
                    crate::NextAction::Terminate => {
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

                        let child = ctx.handoff_child();
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

                        yield crate::AgentEvent::HandoffItem {
                            from: agent_name.clone(),
                            to: target.clone(),
                        };
                        yield crate::AgentEvent::AgentUpdated {
                            agent: target.clone(),
                        };

                        let input = crate::AgentInput { messages: transcript };
                        let mut sub = match target_agent.run(child, input).await {
                            Ok(s) => s,
                            Err(e) => {
                                let msg = e.to_string();
                                run_span.record("otel.status_code", "ERROR");
                                failure.set(e);
                                yield crate::AgentEvent::RunFailed { error: msg };
                                return;
                            }
                        };
                        while let Some(ev) = sub.next().await {
                            match ev {
                                crate::AgentEvent::RunStarted { .. } => {}
                                crate::AgentEvent::RunCompleted { usage } => {
                                    let mut total = parent_usage;
                                    total.add(usage);
                                    run_span.record("gen_ai.usage.input_tokens", total.input_tokens as i64);
                                    run_span.record("gen_ai.usage.output_tokens", total.output_tokens as i64);
                                    yield crate::AgentEvent::RunCompleted { usage: total };
                                }
                                other => yield other,
                            }
                        }
                        return;
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
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

    /// A handoff chain or `AgentAsTool` nesting exceeded
    /// [`crate::RunConfig::max_agent_depth`].
    #[error("agent nesting depth ({depth}) exceeded max ({max})")]
    MaxAgentDepthExceeded {
        /// The depth that would have been entered.
        depth: u32,
        /// The configured maximum.
        max: u32,
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
