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
/// `Runner` is object-safe: the per-method bound `A: Agent<Ctx> + ?Sized`
/// (rather than a `<A: Agent<Ctx>>` parameter on the trait) keeps the
/// trait itself dyn-safe while accepting both concrete `&LlmAgent<…>` and
/// `&dyn Agent<Ctx>` at the call site.
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
///     async fn run<A>(
///         &self,
///         _agent: &A,
///         _ctx: RunContext<()>,
///         _input: AgentInput,
///         _config: RunConfig,
///     ) -> Result<RunResult, RunError>
///     where
///         A: Agent<()> + ?Sized,
///     {
///         Ok(RunResult::default())
///     }
///
///     async fn run_streamed<A>(
///         &self,
///         _agent: &A,
///         _ctx: RunContext<()>,
///         _input: AgentInput,
///         _config: RunConfig,
///     ) -> Result<RunResultStreaming, RunError>
///     where
///         A: Agent<()> + ?Sized,
///     {
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
    async fn run<A>(
        &self,
        agent: &A,
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResult, RunError>
    where
        A: Agent<Ctx> + ?Sized;

    /// Run the agent and return a streaming result handle.
    async fn run_streamed<A>(
        &self,
        agent: &A,
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResultStreaming, RunError>
    where
        A: Agent<Ctx> + ?Sized;
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
