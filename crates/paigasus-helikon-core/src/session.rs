//! The [`Session`] trait and its carrier types.
//!
//! `Session` models conversation persistence as an **append-only event
//! log**, not a flat message list. The event-log shape gives evals
//! (deterministic replay), durability (Temporal/Restate-style event
//! sourcing), and an audit trail for regulated deployments.

use async_trait::async_trait;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{AgentEvent, ContentPart, Item};

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

    /// The serde tag for this variant (`"user_message"`, `"compacted"`, …).
    /// Matches the `type` field written to the persisted log.
    pub fn kind(&self) -> &'static str {
        // No `_ =>` arm: a new #[non_exhaustive] variant must fail to compile
        // here, in core, rather than silently mis-tagging in a backend.
        match self {
            SessionEvent::UserMessage { .. } => "user_message",
            SessionEvent::AssistantMessage { .. } => "assistant_message",
            SessionEvent::ToolCalled { .. } => "tool_called",
            SessionEvent::ToolReturned { .. } => "tool_returned",
            SessionEvent::HandoffOccurred { .. } => "handoff_occurred",
            SessionEvent::Compacted { .. } => "compacted",
        }
    }

    /// The wall-clock instant this event was recorded.
    pub fn ts(&self) -> Timestamp {
        match self {
            SessionEvent::UserMessage { ts, .. }
            | SessionEvent::AssistantMessage { ts, .. }
            | SessionEvent::ToolCalled { ts, .. }
            | SessionEvent::ToolReturned { ts, .. }
            | SessionEvent::HandoffOccurred { ts, .. }
            | SessionEvent::Compacted { ts, .. } => *ts,
        }
    }

    /// [`Self::ts`] as `i64` nanoseconds since the Unix epoch, saturating to
    /// `i64::MIN`/`i64::MAX` outside ±292 years from 1970. For denormalized
    /// audit-index columns; the canonical timestamp lives in the JSON payload.
    pub fn ts_nanos_saturating(&self) -> i64 {
        let nanos_i128 = self.ts().as_nanosecond();
        let saturated = if nanos_i128 < 0 { i64::MIN } else { i64::MAX };
        i64::try_from(nanos_i128).unwrap_or(saturated)
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
        // instead of wrapping past `u32::MAX`; `saturating_add` then guards
        // the +1 against the SequenceId(u64::MAX) edge case on 64-bit where
        // usize::MAX + 1 would overflow. Both branches are unreachable in
        // practice; the saturated result clamps `start` so the slice below
        // returns an empty Vec rather than wrapping.
        let start = match since {
            Some(s) => usize::try_from(s.0)
                .expect("SequenceId exceeds platform usize")
                .saturating_add(1),
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

/// Accumulates one run's semantic items as [`SessionEvent`]s for the runner to
/// persist at run exit.
///
/// The runner pre-seeds the recorder with the run's `AgentInput` messages (the
/// new turn — which the agent does not re-emit on its event stream), then feeds
/// it every [`crate::AgentEvent`] as the stream flows. [`SessionRecorder::drain`]
/// returns the accumulated log, synthesizing a [`SessionEvent::ToolReturned`]
/// for any tool call left without a result (a run interrupted mid-tool) so the
/// log always [`project`]s to a provider-valid conversation.
#[derive(Debug)]
pub struct SessionRecorder {
    events: Vec<SessionEvent>,
    current_agent: String,
}

impl SessionRecorder {
    /// Create a recorder seeded with the root agent's name. Used to attribute
    /// assistant messages whose [`Item::AssistantMessage`] `agent` is `None`,
    /// before any `RunStarted`/`AgentUpdated` updates the tracked agent.
    pub fn new(root_agent: impl Into<String>) -> Self {
        Self {
            events: Vec::new(),
            current_agent: root_agent.into(),
        }
    }

    /// Record the run's input messages (the new turn). The agent never re-emits
    /// these on its event stream, so the runner records them directly.
    /// [`Item::System`] has no [`SessionEvent`] equivalent and is skipped.
    pub fn record_input(&mut self, messages: &[Item]) {
        for item in messages {
            match item {
                Item::UserMessage { content } => {
                    self.events
                        .push(SessionEvent::user_message(content.clone()));
                }
                Item::AssistantMessage { content, agent } => {
                    let name = agent.clone().unwrap_or_else(|| self.current_agent.clone());
                    self.events
                        .push(SessionEvent::assistant_message(content.clone(), name));
                }
                Item::ToolCall {
                    call_id,
                    name,
                    args,
                } => {
                    self.events.push(SessionEvent::tool_called(
                        call_id.clone(),
                        name.clone(),
                        args.clone(),
                    ));
                }
                Item::ToolResult { call_id, content } => {
                    self.events.push(SessionEvent::tool_returned(
                        call_id.clone(),
                        content.clone(),
                    ));
                }
                Item::System { .. } => {
                    tracing::debug!(
                        "SessionRecorder: skipping Item::System in input (no SessionEvent variant)"
                    );
                }
            }
        }
    }

    /// Observe one [`crate::AgentEvent`] from the run's stream: record the
    /// semantic items (assistant messages, tool calls, tool results, handoffs)
    /// and track the active agent from `RunStarted` / `AgentUpdated`.
    pub fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::RunStarted { agent } | AgentEvent::AgentUpdated { agent } => {
                self.current_agent = agent.clone();
            }
            AgentEvent::MessageOutput {
                item: Item::AssistantMessage { content, agent },
            } => {
                let name = agent.clone().unwrap_or_else(|| self.current_agent.clone());
                self.events
                    .push(SessionEvent::assistant_message(content.clone(), name));
            }
            AgentEvent::ToolCallItem {
                item:
                    Item::ToolCall {
                        call_id,
                        name,
                        args,
                    },
            } => {
                self.events.push(SessionEvent::tool_called(
                    call_id.clone(),
                    name.clone(),
                    args.clone(),
                ));
            }
            AgentEvent::ToolOutputItem {
                item: Item::ToolResult { call_id, content },
            } => {
                self.events.push(SessionEvent::tool_returned(
                    call_id.clone(),
                    content.clone(),
                ));
            }
            AgentEvent::HandoffItem { from, to } => {
                self.events
                    .push(SessionEvent::handoff_occurred(from.clone(), to.clone()));
            }
            _ => {}
        }
    }

    /// Drain the accumulated events, appending a synthesized
    /// [`SessionEvent::ToolReturned`] for every [`SessionEvent::ToolCalled`]
    /// left without a matching result (a run cancelled/timed out mid-tool).
    /// The returned log always [`project`]s to a provider-valid conversation.
    pub fn drain(&mut self) -> Vec<SessionEvent> {
        let returned: std::collections::HashSet<String> = self
            .events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::ToolReturned { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        let mut synthesized = Vec::new();
        for e in &self.events {
            if let SessionEvent::ToolCalled { call_id, .. } = e {
                if !returned.contains(call_id) {
                    synthesized.push(SessionEvent::tool_returned(
                        call_id.clone(),
                        vec![ContentPart::Text {
                            text: "tool call did not complete (run cancelled/timed out)".to_owned(),
                        }],
                    ));
                }
            }
        }
        let mut out = std::mem::take(&mut self.events);
        out.extend(synthesized);
        out
    }
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

#[cfg(test)]
mod recorder_tests {
    use super::*;
    use crate::AgentEvent;

    fn text(s: &str) -> Vec<ContentPart> {
        vec![ContentPart::Text { text: s.to_owned() }]
    }

    #[test]
    fn record_input_maps_user_and_skips_system() {
        let mut r = SessionRecorder::new("root");
        r.record_input(&[
            Item::System {
                content: text("sys"),
            },
            Item::UserMessage {
                content: text("hi"),
            },
        ]);
        let out = r.drain();
        assert_eq!(
            out.len(),
            1,
            "System is skipped; only the user message remains"
        );
        assert!(
            matches!(&out[0], SessionEvent::UserMessage { content, .. } if content == &text("hi"))
        );
    }

    #[test]
    fn observe_records_assistant_with_item_agent() {
        let mut r = SessionRecorder::new("root");
        r.observe(&AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: text("yo"),
                agent: Some("speaker".into()),
            },
        });
        let out = r.drain();
        assert!(
            matches!(&out[0], SessionEvent::AssistantMessage { agent, .. } if agent == "speaker")
        );
    }

    #[test]
    fn observe_falls_back_to_tracked_agent_then_agent_updated() {
        let mut r = SessionRecorder::new("root");
        // No item.agent => falls back to the seeded root.
        r.observe(&AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: text("a"),
                agent: None,
            },
        });
        // AgentUpdated changes the tracked agent for subsequent None items.
        r.observe(&AgentEvent::AgentUpdated {
            agent: "specialist".into(),
        });
        r.observe(&AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: text("b"),
                agent: None,
            },
        });
        let out = r.drain();
        assert!(matches!(&out[0], SessionEvent::AssistantMessage { agent, .. } if agent == "root"));
        assert!(
            matches!(&out[1], SessionEvent::AssistantMessage { agent, .. } if agent == "specialist")
        );
    }

    #[test]
    fn observe_records_tool_call_result_and_handoff() {
        let mut r = SessionRecorder::new("root");
        r.observe(&AgentEvent::ToolCallItem {
            item: Item::ToolCall {
                call_id: "c1".into(),
                name: "echo".into(),
                args: serde_json::json!({}),
            },
        });
        r.observe(&AgentEvent::ToolOutputItem {
            item: Item::ToolResult {
                call_id: "c1".into(),
                content: text("ok"),
            },
        });
        r.observe(&AgentEvent::HandoffItem {
            from: "a".into(),
            to: "b".into(),
        });
        let out = r.drain();
        assert!(matches!(&out[0], SessionEvent::ToolCalled { call_id, .. } if call_id == "c1"));
        assert!(matches!(&out[1], SessionEvent::ToolReturned { call_id, .. } if call_id == "c1"));
        assert!(
            matches!(&out[2], SessionEvent::HandoffOccurred { from, to, .. } if from == "a" && to == "b")
        );
    }

    #[test]
    fn drain_synthesizes_result_for_unmatched_tool_call() {
        let mut r = SessionRecorder::new("root");
        r.observe(&AgentEvent::ToolCallItem {
            item: Item::ToolCall {
                call_id: "c1".into(),
                name: "slow".into(),
                args: serde_json::json!({}),
            },
        });
        // No ToolOutputItem (interrupted mid-tool).
        let out = r.drain();
        assert_eq!(out.len(), 2, "the call plus a synthesized result");
        assert!(matches!(&out[0], SessionEvent::ToolCalled { call_id, .. } if call_id == "c1"));
        match &out[1] {
            SessionEvent::ToolReturned {
                call_id, content, ..
            } => {
                assert_eq!(call_id, "c1");
                assert!(
                    matches!(&content[0], ContentPart::Text { text } if text.contains("did not complete"))
                );
            }
            other => panic!("expected synthesized ToolReturned, got {other:?}"),
        }
        // project() yields a matched call/result pair (provider-valid).
        assert_eq!(project(&out).messages.len(), 2);
    }

    #[test]
    fn drain_does_not_synthesize_when_result_present() {
        let mut r = SessionRecorder::new("root");
        r.observe(&AgentEvent::ToolCallItem {
            item: Item::ToolCall {
                call_id: "c1".into(),
                name: "echo".into(),
                args: serde_json::json!({}),
            },
        });
        r.observe(&AgentEvent::ToolOutputItem {
            item: Item::ToolResult {
                call_id: "c1".into(),
                content: text("ok"),
            },
        });
        assert_eq!(r.drain().len(), 2, "no extra synthesized event");
    }

    #[test]
    fn record_input_maps_assistant_and_tool_items() {
        let mut r = SessionRecorder::new("root");
        r.record_input(&[
            Item::AssistantMessage {
                content: text("reply"),
                agent: None,
            },
            Item::ToolCall {
                call_id: "c1".into(),
                name: "fn".into(),
                args: serde_json::json!({}),
            },
            Item::ToolResult {
                call_id: "c1".into(),
                content: text("res"),
            },
        ]);
        let out = r.drain();
        assert_eq!(out.len(), 3);
        assert!(matches!(&out[0], SessionEvent::AssistantMessage { agent, .. } if agent == "root"));
        assert!(matches!(&out[1], SessionEvent::ToolCalled { call_id, .. } if call_id == "c1"));
        assert!(matches!(&out[2], SessionEvent::ToolReturned { call_id, .. } if call_id == "c1"));
    }

    #[test]
    fn observe_run_started_sets_tracked_agent() {
        let mut r = SessionRecorder::new("root");
        r.observe(&AgentEvent::RunStarted {
            agent: "starter".into(),
        });
        r.observe(&AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: text("x"),
                agent: None,
            },
        });
        let out = r.drain();
        assert!(
            matches!(&out[0], SessionEvent::AssistantMessage { agent, .. } if agent == "starter")
        );
    }
}

#[cfg(test)]
mod accessor_tests {
    use super::*;
    use crate::ContentPart;
    use jiff::Timestamp;

    fn epoch() -> Timestamp {
        Timestamp::from_second(0).unwrap()
    }

    #[test]
    fn kind_matches_serde_tag_for_every_variant() {
        let cases: Vec<(SessionEvent, &str)> = vec![
            (
                SessionEvent::UserMessage {
                    content: vec![],
                    ts: epoch(),
                },
                "user_message",
            ),
            (
                SessionEvent::AssistantMessage {
                    content: vec![],
                    agent: "a".into(),
                    ts: epoch(),
                },
                "assistant_message",
            ),
            (
                SessionEvent::ToolCalled {
                    call_id: "c".into(),
                    name: "n".into(),
                    args: serde_json::json!({}),
                    ts: epoch(),
                },
                "tool_called",
            ),
            (
                SessionEvent::ToolReturned {
                    call_id: "c".into(),
                    content: vec![],
                    ts: epoch(),
                },
                "tool_returned",
            ),
            (
                SessionEvent::HandoffOccurred {
                    from: "a".into(),
                    to: "b".into(),
                    ts: epoch(),
                },
                "handoff_occurred",
            ),
            (
                SessionEvent::Compacted {
                    summary: "s".into(),
                    original_count: 1,
                    ts: epoch(),
                },
                "compacted",
            ),
        ];
        for (ev, tag) in cases {
            assert_eq!(ev.kind(), tag);
            // kind() must equal the serde tag actually written to the wire.
            let json = serde_json::to_value(&ev).unwrap();
            assert_eq!(json["type"], tag);
        }
    }

    #[test]
    fn ts_returns_the_variant_timestamp_and_nanos_saturate() {
        let ev = SessionEvent::UserMessage {
            content: vec![ContentPart::Text { text: "x".into() }],
            ts: Timestamp::from_second(1_700_000_000).unwrap(),
        };
        assert_eq!(ev.ts(), Timestamp::from_second(1_700_000_000).unwrap());
        assert_eq!(ev.ts_nanos_saturating(), 1_700_000_000_000_000_000);
    }
}
