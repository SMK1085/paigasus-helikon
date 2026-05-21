//! The [`Runner`] trait and its carrier types.
//!
//! The runner is the durability seam (per ADR-6): swappable between
//! ephemeral tokio (`paigasus-helikon-runtime-tokio`), durable Temporal
//! (`paigasus-helikon-runtime-temporal`), and AWS AgentCore
//! (`paigasus-helikon-runtime-agentcore`).

use async_trait::async_trait;

use crate::{Agent, AgentError, AgentEvent, AgentInput, RunContext};

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
/// SMA-314 ships only `max_turns`. SMA-321 (TokioRunner) adds
/// `timeout`, `parallel_tool_call_limit`, `retry_policy`, and
/// `cancellation`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunConfig {
    /// Maximum number of model turns before the loop fails with
    /// [`crate::AgentError::MaxTurnsExceeded`]. Default `16`.
    pub max_turns: u32,
}

impl Default for RunConfig {
    fn default() -> Self { Self { max_turns: 16 } }
}

impl RunConfig {
    /// Construct a default config (`max_turns = 16`).
    pub fn new() -> Self { Self::default() }
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
    /// `final_output` is the concatenated text from every
    /// `AgentEvent::TokenDelta`. Structured-output callers go through
    /// `RunResult::<String>::parse_final::<T>()` (SMA-313).
    pub async fn collect(mut self) -> Result<RunResult, RunError> {
        use futures_util::stream::StreamExt;
        let mut events = Vec::new();
        let mut final_output = String::new();
        let mut usage = crate::TokenUsage::default();
        let mut failed: Option<String> = None;

        while let Some(ev) = self.events.next().await {
            match &ev {
                crate::AgentEvent::TokenDelta { text } => final_output.push_str(text),
                crate::AgentEvent::RunCompleted { usage: u } => usage = *u,
                crate::AgentEvent::RunFailed { error } => failed = Some(error.clone()),
                _ => {}
            }
            events.push(ev);
        }

        if let Some(e) = failed {
            return Err(RunError::Other(anyhow::anyhow!(e)));
        }

        Ok(RunResult { final_output, events, usage })
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

    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
