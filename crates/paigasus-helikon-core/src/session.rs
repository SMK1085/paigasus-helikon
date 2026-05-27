//! The [`Session`] trait and its carrier types.
//!
//! `Session` models conversation persistence as an **append-only event
//! log**, not a flat message list. The event-log shape gives evals
//! (deterministic replay), durability (Temporal/Restate-style event
//! sourcing), and an audit trail for regulated deployments.

use async_trait::async_trait;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{ContentPart, Item};

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
/// `UserMessage` / `AssistantMessage` / `ToolReturned` carry
/// `Vec<ContentPart>` directly (not `Item`) because the SessionEvent
/// variant *is* the role — wrapping `Item::UserMessage` inside
/// `SessionEvent::UserMessage` would double-tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionEvent {
    /// A user-authored message.
    ///
    /// The enum is marked `#[non_exhaustive]` at the enum level (not per
    /// variant) so downstream tests and fixtures can construct variants
    /// by struct-init to pin a deterministic `ts`. Don't tighten this to
    /// per-variant `#[non_exhaustive]`.
    UserMessage {
        /// Content blocks of the message.
        content: Vec<ContentPart>,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
    /// An assistant-authored message attributed to a named agent.
    AssistantMessage {
        /// Content blocks of the message.
        content: Vec<ContentPart>,
        /// Name of the emitting [`crate::Agent`]. `String` (not `Option`)
        /// because the runner always knows which agent emitted when
        /// appending to the log.
        agent: String,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
    /// The runner invoked a tool.
    ToolCalled {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        name: String,
        /// JSON arguments.
        args: serde_json::Value,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
    /// The tool returned.
    ToolReturned {
        /// Matching call identifier.
        call_id: String,
        /// Content blocks of the tool's output (Anthropic permits
        /// text + image inside a tool result).
        content: Vec<ContentPart>,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
    /// Control transferred from one agent to another.
    HandoffOccurred {
        /// Outgoing agent name.
        from: String,
        /// Incoming agent name.
        to: String,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
    /// Older events were compacted into a summary.
    ///
    /// **Provider-translator caveat:** [`project`] renders this as
    /// [`Item::System`]. Both shipped provider translators reshape
    /// `Item::System`: Anthropic hoists every system block into the
    /// top-level `system` field, and OpenAI concatenates multiple system
    /// blocks into one at the top of the conversation. The "summary
    /// replaces turns 1..N at this position" semantic is therefore
    /// observation-only in the event log; the model sees the summary text
    /// but as a top-level system instruction, not a positional cutover.
    Compacted {
        /// LLM-produced summary.
        summary: String,
        /// Number of events the summary replaces. `u64` (not `usize`)
        /// because the value is serialized into the persisted log — a
        /// 32-bit consumer must read what a 64-bit producer wrote.
        original_count: u64,
        /// Wall-clock instant the event was recorded. UTC, nanosecond precision.
        ts: Timestamp,
    },
}

impl SessionEvent {
    /// Construct a [`SessionEvent::UserMessage`] with `ts = Timestamp::now()`.
    pub fn user_message(content: Vec<ContentPart>) -> Self {
        Self::UserMessage {
            content,
            ts: Timestamp::now(),
        }
    }

    /// Construct a [`SessionEvent::AssistantMessage`] with `ts = Timestamp::now()`.
    pub fn assistant_message(content: Vec<ContentPart>, agent: impl Into<String>) -> Self {
        Self::AssistantMessage {
            content,
            agent: agent.into(),
            ts: Timestamp::now(),
        }
    }

    /// Construct a [`SessionEvent::ToolCalled`] with `ts = Timestamp::now()`.
    pub fn tool_called(
        call_id: impl Into<String>,
        name: impl Into<String>,
        args: serde_json::Value,
    ) -> Self {
        Self::ToolCalled {
            call_id: call_id.into(),
            name: name.into(),
            args,
            ts: Timestamp::now(),
        }
    }

    /// Construct a [`SessionEvent::ToolReturned`] with `ts = Timestamp::now()`.
    pub fn tool_returned(call_id: impl Into<String>, content: Vec<ContentPart>) -> Self {
        Self::ToolReturned {
            call_id: call_id.into(),
            content,
            ts: Timestamp::now(),
        }
    }

    /// Construct a [`SessionEvent::HandoffOccurred`] with `ts = Timestamp::now()`.
    pub fn handoff_occurred(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self::HandoffOccurred {
            from: from.into(),
            to: to.into(),
            ts: Timestamp::now(),
        }
    }

    /// Construct a [`SessionEvent::Compacted`] with `ts = Timestamp::now()`.
    pub fn compacted(summary: impl Into<String>, original_count: u64) -> Self {
        Self::Compacted {
            summary: summary.into(),
            original_count,
            ts: Timestamp::now(),
        }
    }
}

/// In-memory [`Session`] backend backed by a `std::sync::Mutex<Vec<_>>`.
///
/// Suitable for tests and ephemeral runs. One instance is one session by
/// construction — there is no `session_id`. For persistent or multi-session
/// storage, see `paigasus-helikon-sessions-sqlite`.
///
/// Lock poisoning panics (`expect`): if a panic occurred inside a critical
/// section, an invariant is already broken. Fail loud.
///
/// **Platform assumption:** [`MemorySession::events`] panics on 32-bit
/// targets if a single session accumulates more than `u32::MAX` events
/// (the `SequenceId` → `usize` conversion via [`usize::try_from`] then
/// fails). Sessions of that size are unreachable in practice; the panic
/// is intentional rather than a typed `SessionError` so the failure
/// surfaces immediately rather than being silently treated as a backend
/// error.
#[derive(Debug, Default)]
pub struct MemorySession {
    inner: std::sync::Mutex<Vec<SessionEvent>>,
}

impl MemorySession {
    /// Create an empty [`MemorySession`].
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Session for MemorySession {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        let mut guard = self.inner.lock().expect("MemorySession mutex poisoned");
        guard.extend_from_slice(events);
        Ok(())
    }

    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        let guard = self.inner.lock().expect("MemorySession mutex poisoned");
        // `since` is *exclusive* — matches the existing trait doc ("those
        // after `since`"). `try_from` ensures 32-bit targets fail loudly
        // instead of wrapping past `u32::MAX`. Unreachable in practice.
        let start = match since {
            Some(s) => usize::try_from(s.0).expect("SequenceId exceeds platform usize") + 1,
            None => 0,
        };
        Ok(guard.get(start..).unwrap_or(&[]).to_vec())
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        let events = self.events(None).await?;
        Ok(project(&events))
    }
}

/// Project an append-only [`SessionEvent`] log into a [`ConversationSnapshot`]
/// — the canonical message-list view that providers consume.
///
/// **Provider-translator caveat:** `Compacted` events render as
/// [`Item::System`]. Both shipped provider translators (SMA-316 OpenAI,
/// SMA-317 Anthropic) reshape system messages — Anthropic hoists every
/// `Item::System` into the top-level `system` field; OpenAI concatenates
/// multiple system blocks into one at the top of the conversation. The
/// "summary replaces turns 1..N at this position" semantic is therefore
/// observation-only in the event log; the model sees the summary text but
/// as a top-level system instruction, not a positional cutover.
pub fn project(events: &[SessionEvent]) -> ConversationSnapshot {
    let mut messages: Vec<Item> = Vec::new();
    // Parallel vec: contributions[i] = number of messages event i produced.
    // Needed because Compacted has to undo the message yield of the previous
    // N events, and yield varies per variant (HandoffOccurred = 0, others = 1).
    let mut contributions: Vec<usize> = Vec::new();

    for ev in events {
        match ev {
            SessionEvent::UserMessage { content, .. } => {
                messages.push(Item::UserMessage {
                    content: content.clone(),
                });
                contributions.push(1);
            }
            SessionEvent::AssistantMessage { content, agent, .. } => {
                messages.push(Item::AssistantMessage {
                    content: content.clone(),
                    agent: Some(agent.clone()),
                });
                contributions.push(1);
            }
            SessionEvent::ToolCalled {
                call_id,
                name,
                args,
                ..
            } => {
                messages.push(Item::ToolCall {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    args: args.clone(),
                });
                contributions.push(1);
            }
            SessionEvent::ToolReturned {
                call_id, content, ..
            } => {
                messages.push(Item::ToolResult {
                    call_id: call_id.clone(),
                    content: content.clone(),
                });
                contributions.push(1);
            }
            SessionEvent::HandoffOccurred { .. } => {
                // Audit-only event; no message produced.
                contributions.push(0);
            }
            SessionEvent::Compacted {
                summary,
                original_count,
                ..
            } => {
                let n = usize::try_from(*original_count).unwrap_or(usize::MAX);
                if n == 0 {
                    tracing::warn!(
                        "Compacted event with original_count = 0; emitting summary as a system message without compacting any history (likely producer bug — use SessionEvent::compacted with a positive count)"
                    );
                }
                if n > contributions.len() {
                    tracing::warn!(
                        original_count = n,
                        events_seen = contributions.len(),
                        "Compacted event references more events than have been seen; treating as 'compact everything observed so far' (likely corrupt log)"
                    );
                }
                let drop_from_idx = contributions.len().saturating_sub(n);
                let drop_msg_count: usize = contributions[drop_from_idx..].iter().sum();
                let new_len = messages.len() - drop_msg_count;
                messages.truncate(new_len);
                // Drop the contribution entries the Compacted event subsumed
                // so a subsequent Compacted's `n` indexes back over the
                // remaining events, not the already-summarized ones.
                contributions.truncate(drop_from_idx);
                messages.push(Item::System {
                    content: vec![ContentPart::Text {
                        text: summary.clone(),
                    }],
                });
                contributions.push(1);
            }
        }
    }

    ConversationSnapshot { messages }
}

/// Monotonic position in a [`Session`]'s append-only log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SequenceId(pub u64);

/// A computed projection of a [`Session`]'s log into a single
/// conversation state. The `messages` field is the canonical view a
/// session emits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConversationSnapshot {
    /// Canonical message list, in conversational order.
    pub messages: Vec<Item>,
}

/// Errors raised by [`Session`] methods.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// Backend unreachable (database down, file locked, …).
    #[error("session backend unavailable")]
    Unavailable,

    /// A backend-specific error, type-erased so core stays free of any
    /// particular backend dependency. The `'static` bound is required for
    /// `<dyn Error>::downcast_ref` to work; callers who care about
    /// the underlying type can do `err.downcast_ref::<sqlx::Error>()`.
    #[error(transparent)]
    Backend(Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl SessionError {
    /// Wrap a backend-specific error as [`SessionError::Backend`].
    ///
    /// Saves the
    /// `.map_err(|e| SessionError::Backend(Box::new(e)))` boilerplate at
    /// every query call site — use as `.map_err(SessionError::backend)`.
    pub fn backend<E>(e: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Backend(Box::new(e))
    }
}
