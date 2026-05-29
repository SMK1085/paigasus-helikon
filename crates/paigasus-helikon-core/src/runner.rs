//! The [`Runner`] trait and its carrier types.
//!
//! The runner is the durability seam (per ADR-6): swappable between
//! ephemeral tokio (`paigasus-helikon-runtime-tokio`), durable Temporal
//! (`paigasus-helikon-runtime-temporal`), and AWS AgentCore
//! (`paigasus-helikon-runtime-agentcore`).

use std::num::NonZeroUsize;
use std::time::Duration;

use async_trait::async_trait;

use crate::{Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext};

/// Pluggable execution backend.
///
/// `Runner` is object-safe: both methods accept `&dyn Agent<Ctx>` rather
/// than a generic `<A: Agent<Ctx>>` parameter, which keeps the trait
/// vtable-friendly while remaining compatible with both concrete and
/// trait-object agent references.
///
/// See ADR-6 (*Library + pluggable Runner trait*).
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use futures_core::stream::BoxStream;
/// use futures_util::stream::empty;
/// use paigasus_helikon_core::{
///     Agent, AgentEvent, AgentInput, RunConfig, RunContext, RunError,
///     RunResult, RunResultStreaming, Runner,
/// };
///
/// struct NoopRunner;
///
/// #[async_trait]
/// impl Runner<()> for NoopRunner {
///     async fn run(
///         &self,
///         _agent: &(dyn Agent<()> + '_),
///         _ctx: RunContext<()>,
///         _input: AgentInput,
///         _config: RunConfig,
///     ) -> Result<RunResult, RunError> {
///         Ok(RunResult::default())
///     }
///
///     async fn run_streamed(
///         &self,
///         _agent: &(dyn Agent<()> + '_),
///         _ctx: RunContext<()>,
///         _input: AgentInput,
///         _config: RunConfig,
///     ) -> Result<RunResultStreaming, RunError> {
///         let stream: BoxStream<'static, AgentEvent> = Box::pin(empty());
///         Ok(RunResultStreaming::new(stream))
///     }
/// }
/// ```
#[async_trait]
pub trait Runner<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Run the agent to completion and return the aggregated result.
    async fn run(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResult, RunError>;

    /// Run the agent and return a streaming result handle.
    async fn run_streamed(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResultStreaming, RunError>;
}

/// Per-run configuration.
///
/// SMA-314 shipped `max_turns`; SMA-321 added `timeout` and
/// `parallel_tool_call_limit`. Cancellation is intentionally *not* a field
/// here — the canonical token lives on [`crate::RunContext`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunConfig {
    /// `[driver-scoped]` Maximum number of model turns before the loop fails
    /// with [`crate::AgentError::MaxTurnsExceeded`]. Honored by the core loop
    /// driver, including on a bare `agent.run()` with no runner. Default `16`.
    pub max_turns: u32,
    /// `[runner-scoped]` Wall-clock deadline for the whole run. Honored ONLY by
    /// a runtime backend (e.g. `TokioRunner`); a bare `agent.run()` cannot time
    /// out (core has no timer). `None` = no deadline.
    pub timeout: Option<Duration>,
    /// `[driver-scoped]` Cap on concurrently-executing tool calls. Honored by
    /// the core loop driver. `None` = unbounded (today's behavior).
    pub parallel_tool_call_limit: Option<NonZeroUsize>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            max_turns: 16,
            timeout: None,
            parallel_tool_call_limit: None,
        }
    }
}

impl RunConfig {
    /// Construct a default config (`max_turns = 16`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the wall-clock run deadline. Honored by a runtime backend (e.g. `TokioRunner`).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Cap the number of tool calls executed concurrently. Honored by the core loop driver.
    pub fn with_parallel_tool_call_limit(mut self, limit: NonZeroUsize) -> Self {
        self.parallel_tool_call_limit = Some(limit);
        self
    }
}

/// The aggregated outcome of a non-streaming [`Runner::run`].
///
/// Generic over the structured-output type. The default `T = String`
/// makes the common case ergonomic; structured-output callers build
/// `RunResult<MyStruct>` via [`RunResult::parse_final`].
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct RunResult<T = String> {
    /// The model's final assistant output, deserialized into `T`. For
    /// the default `T = String` this is the literal text.
    pub final_output: T,
    /// Every [`AgentEvent`] emitted during the run, in order.
    pub events: Vec<AgentEvent>,
    /// Aggregated token usage across every turn of the run.
    pub usage: TokenUsage,
}

impl RunResult<String> {
    /// Deserialize `final_output` into `T`, producing a typed
    /// [`RunResult`].
    ///
    /// The `T: JsonSchema` bound is the marker that the caller has
    /// configured structured output upstream — without it,
    /// `parse_final` is just a JSON parse over unstructured text.
    pub fn parse_final<T>(self) -> Result<RunResult<T>, serde_json::Error>
    where
        T: serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        let final_output = serde_json::from_str::<T>(&self.final_output)?;
        Ok(RunResult {
            final_output,
            events: self.events,
            usage: self.usage,
        })
    }
}

/// Streaming counterpart of [`RunResult`].
///
/// Wraps the unified [`crate::AgentEvent`] stream emitted by an agent
/// and offers an `async fn collect` that drains the stream into a
/// `RunResult<String>`. Callers may consume `events` directly for raw
/// streaming.
pub struct RunResultStreaming {
    /// The event stream produced by the agent's run.
    pub events: futures_core::stream::BoxStream<'static, crate::AgentEvent>,
}

impl RunResultStreaming {
    /// Wrap an event stream.
    pub fn new(events: futures_core::stream::BoxStream<'static, crate::AgentEvent>) -> Self {
        Self { events }
    }

    /// Drain the stream and aggregate into a `RunResult<String>`.
    ///
    /// `final_output` is the concatenated text from the *last*
    /// `AgentEvent::MessageOutput { item: AssistantMessage }`. In multi-turn
    /// flows, each new assistant message resets `final_output`, ensuring
    /// the result is the terminal assistant output, not intermediate text.
    /// Structured-output callers go through `RunResult::<String>::parse_final::<T>()`
    /// (SMA-313).
    pub async fn collect(mut self) -> Result<RunResult, RunError> {
        use futures_util::stream::StreamExt;
        let mut events = Vec::new();
        let mut final_output = String::new();
        let mut usage = crate::TokenUsage::default();

        while let Some(ev) = self.events.next().await {
            match &ev {
                crate::AgentEvent::MessageOutput {
                    item: crate::Item::AssistantMessage { content, .. },
                } => {
                    final_output.clear();
                    for part in content {
                        if let crate::ContentPart::Text { text } = part {
                            final_output.push_str(text);
                        }
                    }
                }
                crate::AgentEvent::RunCompleted { usage: u } => usage = *u,
                crate::AgentEvent::RunFailed { error } => {
                    let err_msg = error.clone();
                    events.push(ev);
                    return Err(RunError::Other(anyhow::anyhow!(err_msg)));
                }
                _ => {}
            }
            events.push(ev);
        }

        Ok(RunResult {
            final_output,
            events,
            usage,
        })
    }

    /// Drain the stream and deserialize the terminal assistant text into `T`.
    ///
    /// The terminal output is the concatenated text of the last
    /// [`AgentEvent::MessageOutput`]. On a correctly configured structured run
    /// the agent loop has already validated that text against `T`, so the parse
    /// here is expected to succeed; if it fails (e.g. `collect_typed` is called
    /// on a plain-text run), the parse error surfaces as [`AgentError::Other`].
    /// A failed run surfaces the underlying [`AgentError`]:
    /// structured-validation failures (carried by
    /// [`AgentEvent::StructuredOutputFailed`]) become
    /// [`AgentError::InvalidStructuredOutput`]; any other terminal
    /// [`AgentEvent::RunFailed`] becomes [`AgentError::Other`].
    pub async fn collect_typed<T>(mut self) -> Result<RunResult<T>, AgentError>
    where
        T: serde::de::DeserializeOwned,
    {
        use futures_util::stream::StreamExt;
        let mut events = Vec::new();
        let mut final_text = String::new();
        let mut usage = crate::TokenUsage::default();
        let mut structured_err: Option<(Vec<String>, String)> = None;

        while let Some(ev) = self.events.next().await {
            match &ev {
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage { content, .. },
                } => {
                    final_text.clear();
                    for part in content {
                        if let ContentPart::Text { text } = part {
                            final_text.push_str(text);
                        }
                    }
                }
                AgentEvent::RunCompleted { usage: u } => usage = *u,
                AgentEvent::StructuredOutputFailed {
                    schema_errors,
                    final_text: ft,
                } => {
                    structured_err = Some((schema_errors.clone(), ft.clone()));
                }
                AgentEvent::RunFailed { error } => {
                    let error = error.clone();
                    events.push(ev);
                    if let Some((schema_errors, final_text)) = structured_err {
                        return Err(AgentError::InvalidStructuredOutput {
                            schema_errors,
                            final_text,
                        });
                    }
                    return Err(AgentError::Other(anyhow::anyhow!(error)));
                }
                _ => {}
            }
            events.push(ev);
        }

        let final_output = serde_json::from_str::<T>(final_text.trim()).map_err(|e| {
            AgentError::Other(anyhow::anyhow!(
                "collect_typed: failed to deserialize final output: {e}"
            ))
        })?;
        Ok(RunResult {
            final_output,
            events,
            usage,
        })
    }
}

/// Token usage aggregated across all turns of a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct TokenUsage {
    /// Prompt tokens billed for this run.
    pub input_tokens: u64,
    /// Completion tokens billed for this run.
    pub output_tokens: u64,
    /// Tokens served from prompt cache (OpenAI prompt-caching, Anthropic
    /// prompt-caching). Counted as `input_tokens` by the provider; this
    /// field is informational.
    pub cached_input_tokens: u64,
    /// Reasoning tokens billed (OpenAI o-series, Anthropic extended
    /// thinking).
    pub reasoning_tokens: u64,
    /// Provider-reported total. May differ from
    /// `input_tokens + output_tokens` when the provider excludes cached or
    /// reasoning tokens from the billed total. Preserve the provider's
    /// value; do not recompute it.
    pub total_tokens: u64,
}

impl TokenUsage {
    /// Add another usage record (per-turn aggregation across a run).
    ///
    /// `total_tokens` is summed alongside the other fields rather than
    /// recomputed from them, preserving each turn's provider-reported total.
    pub fn add(&mut self, other: TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.total_tokens += other.total_tokens;
    }
}

/// Errors raised by [`Runner`] methods.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    /// The underlying agent failed.
    #[error("agent failed: {0}")]
    Agent(#[from] AgentError),

    /// The runner hit the configured maximum iteration count.
    #[error("max iterations reached")]
    MaxIterations,

    /// The run was cancelled (e.g. via [`crate::CancellationToken`]).
    #[error("cancelled")]
    Cancelled,

    /// The run exceeded its configured [`RunConfig::timeout`].
    #[error("run timed out")]
    Timeout,

    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[cfg(test)]
mod runconfig_tests {
    use super::*;

    #[test]
    fn run_error_timeout_displays() {
        assert_eq!(RunError::Timeout.to_string(), "run timed out");
    }

    #[test]
    fn run_config_defaults_and_builders() {
        let c = RunConfig::default();
        assert_eq!(c.max_turns, 16);
        assert!(c.timeout.is_none());
        assert!(c.parallel_tool_call_limit.is_none());

        let c = RunConfig::new()
            .with_timeout(std::time::Duration::from_secs(5))
            .with_parallel_tool_call_limit(std::num::NonZeroUsize::new(3).unwrap());
        assert_eq!(c.timeout, Some(std::time::Duration::from_secs(5)));
        assert_eq!(c.parallel_tool_call_limit, std::num::NonZeroUsize::new(3));
    }
}
