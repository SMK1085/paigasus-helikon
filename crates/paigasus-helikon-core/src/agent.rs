//! The [`Agent`] trait and its carrier types.
//!
//! One trait covers LLM-driven agents (`LlmAgent`) and workflow agents
//! (`SequentialAgent`, `ParallelAgent`, `LoopAgent`, `SwarmAgent`,
//! `GraphAgent`) — see ADR-11.

use async_trait::async_trait;
use futures_core::stream::BoxStream;

use crate::{GuardrailKind, ModelError, RunContext, SessionError, ToolError};

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
/// Field shape (user text, attachments, previous-response handles) lands
/// with the agent-loop ticket.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AgentInput {}

/// The unified event stream emitted by an [`Agent`].
///
/// The full 14-variant ADT (token deltas, semantic items, approvals,
/// guardrail signals, …) lands with the agent-loop ticket. This trimmed
/// set covers the lifecycle the trait surface needs.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AgentEvent {
    /// The run has started; the named agent is active.
    RunStarted {
        /// Agent name.
        agent: String,
    },
    /// A token-level delta in the assistant channel (for low-latency UIs).
    TokenDelta {
        /// Text fragment.
        text: String,
    },
    /// The run finished normally.
    RunCompleted,
    /// The run finished with an error.
    RunFailed {
        /// Human-readable error message.
        error: String,
    },
}

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

    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
