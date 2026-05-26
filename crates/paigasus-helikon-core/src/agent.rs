//! The [`Agent`] trait and its carrier types.
//!
//! One trait covers LLM-driven agents (`LlmAgent`) and workflow agents
//! (`SequentialAgent`, `ParallelAgent`, `LoopAgent`, `SwarmAgent`,
//! `GraphAgent`) — see ADR-11.

use async_trait::async_trait;
use futures_core::stream::BoxStream;

use crate::{GuardrailKind, Item, ModelError, RunContext, SessionError, TokenUsage, ToolError};

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

/// Structured-output type marker — the JSON Schema the model is asked
/// to produce.
///
/// SMA-320 promotes the typed-output path (`output_type::<T>()`
/// honesty); SMA-314 only defines the field type so `LlmAgent` has a
/// place to store it.
#[derive(Debug, Clone)]
pub struct OutputType {
    /// The JSON Schema the model should produce.
    pub schema: schemars::Schema,
}

impl OutputType {
    /// Construct from a type that derives [`schemars::JsonSchema`].
    pub fn from_schema<T: schemars::JsonSchema>() -> Self {
        Self {
            schema: schemars::schema_for!(T),
        }
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
/// typestate builder lands in SMA-319. **Not** `#[non_exhaustive]` —
/// the typestate builder needs struct-literal construction from
/// outside the crate.
pub struct LlmAgent<Ctx, M>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
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
    /// Candidate agents this one may hand off to. Stored but not
    /// driven in SMA-314.
    pub handoffs: Vec<std::sync::Arc<dyn crate::Agent<Ctx>>>,
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
) -> Vec<crate::ToolCallOutcome>
where
    Ctx: Send + Sync + 'static,
{
    let futures = calls.iter().map(|call| {
        let tool = tools.iter().find(|t| t.name() == call.name).cloned();
        let call_id = call.call_id.clone();
        let args = call.args.clone();
        let name = call.name.clone();
        async move {
            match tool {
                Some(t) => match t.invoke(tool_ctx, args).await {
                    Ok(output) => crate::ToolCallOutcome {
                        call_id,
                        result: Ok(tool_output_to_content_parts(&output)),
                    },
                    Err(e) => crate::ToolCallOutcome {
                        call_id,
                        result: Err(e.to_string()),
                    },
                },
                None => crate::ToolCallOutcome {
                    call_id,
                    result: Err(format!("unknown tool: {name}")),
                },
            }
        }
    });
    futures_util::future::join_all(futures).await
}

// ── Agent impl for LlmAgent ──────────────────────────────────────────────────

#[async_trait::async_trait]
impl<Ctx, M> crate::Agent<Ctx> for LlmAgent<Ctx, M>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
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
        let max_turns = self.config.max_turns;
        let model_settings = self.model_settings.clone();
        let agent_name = self.name.clone();
        let instructions_text = self.instructions.render(&ctx);
        let tool_defs: Vec<crate::ToolDef> = tools
            .iter()
            .map(|t| crate::ToolDef {
                name: t.name().to_owned(),
                description: t.description().to_owned(),
                schema: t.schema().clone(),
            })
            .collect();

        let stream = async_stream::stream! {
            // Seed conversation: optional system message + user input.
            let mut conversation: Vec<crate::Item> = Vec::new();
            if !instructions_text.is_empty() {
                conversation.push(crate::Item::System {
                    content: vec![crate::ContentPart::Text { text: instructions_text }],
                });
            }
            conversation.extend(input.messages.iter().cloned());

            let mut loop_state = crate::LoopState::CallingModel { turn: 0 };
            let mut tx_input = crate::TransitionInput::Start { messages: input.messages };

            yield crate::AgentEvent::RunStarted { agent: agent_name.clone() };

            loop {
                let tx_ctx = crate::TransitionCtx {
                    tools: &tool_defs,
                    model_settings: &model_settings,
                    max_turns,
                    conversation: &conversation,
                };
                let outcome = crate::transition(&loop_state, tx_input, &tx_ctx);
                let crate::TransitionOutcome { next_state, events, next_action } = outcome;
                for ev in events { yield ev; }
                loop_state = next_state;

                match next_action {
                    crate::NextAction::CallModel { request } => {
                        let cancel = ctx.cancel().clone();
                        let mut model_stream = match model.invoke(request, cancel).await {
                            Ok(s) => s,
                            Err(e) => {
                                yield crate::AgentEvent::RunFailed { error: e.to_string() };
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
                                    yield crate::AgentEvent::RunFailed { error: e.to_string() };
                                    return;
                                }
                            }
                        }

                        let items = match build_items(&agent_name, text, reasoning, tool_accum) {
                            Ok(items) => items,
                            Err(e) => {
                                yield crate::AgentEvent::RunFailed { error: e };
                                return;
                            }
                        };
                        conversation.extend(items.iter().cloned());
                        let usage = latest_usage.unwrap_or_default();
                        tx_input = crate::TransitionInput::ModelResponse {
                            items,
                            usage,
                            finish_reason,
                        };
                    }
                    crate::NextAction::ExecuteTools { calls } => {
                        let tool_ctx = ctx.to_tool_context();
                        let outcomes =
                            run_tools_concurrent(&tools, &calls, &tool_ctx).await;
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
                    crate::NextAction::Terminate => return,
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
    #[error("invalid structured output after one repair attempt")]
    InvalidStructuredOutput,

    /// New in SMA-314: `max_turns` budget exhausted.
    #[error("max turns ({0}) exceeded")]
    MaxTurnsExceeded(u32),

    /// New in SMA-314: reached a `LoopState` variant SMA-314 does not
    /// yet drive (handoff, compaction, approval).
    #[error("not yet implemented: {feature}")]
    NotImplemented {
        /// The unimplemented loop feature.
        feature: &'static str,
    },

    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
