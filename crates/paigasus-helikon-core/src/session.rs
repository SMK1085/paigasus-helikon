//! The [`Session`] trait and its carrier types.
//!
//! `Session` models conversation persistence as an **append-only event
//! log**, not a flat message list. The event-log shape gives evals
//! (deterministic replay), durability (Temporal/Restate-style event
//! sourcing), and an audit trail for regulated deployments.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Conversation persistence as an append-only event log.
///
/// See the *Sessions* concept page for the rationale.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{
///     ConversationSnapshot, SequenceId, Session, SessionError, SessionEvent,
/// };
///
/// struct MemorySession;
///
/// #[async_trait]
/// impl Session for MemorySession {
///     async fn append(
///         &self,
///         _events: &[SessionEvent],
///     ) -> Result<(), SessionError> {
///         Ok(())
///     }
///
///     async fn events(
///         &self,
///         _since: Option<SequenceId>,
///     ) -> Result<Vec<SessionEvent>, SessionError> {
///         Ok(Vec::new())
///     }
///
///     async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
///         Ok(ConversationSnapshot::default())
///     }
/// }
/// ```
#[async_trait]
pub trait Session: Send + Sync {
    /// Append events to the log.
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError>;

    /// Read events from the log, optionally only those after `since`.
    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError>;

    /// Compute (or read) a [`ConversationSnapshot`] projection of the log.
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError>;
}

/// One entry in the conversation event log.
///
/// Variant fields (content/timestamp shapes) will graduate from the
/// placeholder types here to the canonical `Item` and `DateTime<Utc>`
/// types with later tickets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionEvent {
    /// A user-authored message.
    UserMessage {
        /// Message text.
        text: String,
    },
    /// An assistant-authored message attributed to a named agent.
    AssistantMessage {
        /// Message text.
        text: String,
        /// Name of the emitting [`crate::Agent`].
        agent: String,
    },
    /// The runner invoked a tool.
    ToolCalled {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        name: String,
        /// JSON arguments.
        args: serde_json::Value,
    },
    /// The tool returned.
    ToolReturned {
        /// Matching call identifier.
        call_id: String,
        /// JSON output.
        output: serde_json::Value,
    },
    /// Control transferred from one agent to another.
    HandoffOccurred {
        /// Outgoing agent name.
        from: String,
        /// Incoming agent name.
        to: String,
    },
    /// Older events were compacted into a summary.
    Compacted {
        /// LLM-produced summary.
        summary: String,
        /// Number of events the summary replaces.
        original_count: usize,
    },
}

/// Monotonic position in a [`Session`]'s append-only log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SequenceId(pub u64);

/// A computed projection of a [`Session`]'s log into a single conversation
/// state. Field shape lands with later tickets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConversationSnapshot {}

/// Errors raised by [`Session`] methods.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// Backend unreachable (database down, file locked, …).
    #[error("session backend unavailable")]
    Unavailable,

    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
