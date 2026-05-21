//! The [`Runner`] trait and its carrier types.
//!
//! The runner is the durability seam (per ADR-6): swappable between
//! ephemeral tokio (`paigasus-helikon-runtime-tokio`), durable Temporal
//! (`paigasus-helikon-runtime-temporal`), and AWS AgentCore
//! (`paigasus-helikon-runtime-agentcore`).

use async_trait::async_trait;

use crate::{Agent, AgentError, AgentInput, RunContext};

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
/// use paigasus_helikon_core::{
///     Agent, AgentInput, RunConfig, RunContext, RunError, RunResult,
///     RunResultStreaming, Runner,
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
///         Ok(RunResultStreaming::default())
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

/// Configuration for a single [`Runner::run`] / [`Runner::run_streamed`]
/// invocation. Field shape (max iterations, retry policy, tracing
/// settings) lands with the runner ticket.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RunConfig {}

/// The aggregated outcome of a non-streaming [`Runner::run`]. Field shape
/// (final response, trajectory, token counts) lands with the runner
/// ticket.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct RunResult {}

/// A streaming handle returned by [`Runner::run_streamed`]. Field shape
/// (the inner `BoxStream<AgentEvent>` and the final-result future) lands
/// with the runner ticket.
#[derive(Default)]
#[non_exhaustive]
pub struct RunResultStreaming {}

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
