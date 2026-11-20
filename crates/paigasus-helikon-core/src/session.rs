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
    /// A tool call was deferred and waits for a host decision.
    ///
    /// Audit and resume record. It produces no conversation message.
    ///
    /// While a deferral is open, the log holds a `ToolCall` with no `ToolResult`.
    /// A `snapshot()` of that session is not valid provider input. Use
    /// [`open_deferrals`](crate::open_deferrals) to find the open deferrals.
    ///
    /// **Rollback note.** Once a host stores a `tool_deferred` event, it cannot roll
    /// back below this version, because an older reader cannot parse the row. Enable
    /// deferral only after every reader of the session store runs this version.
    ToolDeferred {
        /// The id the host uses to resolve the deferral.
        deferral_id: String,
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        name: String,
        /// The raw effective JSON arguments, not redacted. The resume runs the
        /// tool with exactly these arguments. Any secret in the arguments
        /// persists in the session store.
        args: serde_json::Value,
        /// The prompt for the approver.
        prompt: String,
        /// Agent names from the root agent to the deferring agent.
        handoff_path: Vec<String>,
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

    /// Construct a [`SessionEvent::ToolDeferred`] from a pending call, with
    /// `ts = Timestamp::now()`.
    pub fn tool_deferred(call: &crate::PendingToolCall) -> Self {
        Self::ToolDeferred {
            deferral_id: call.deferral_id().to_owned(),
            call_id: call.call_id().to_owned(),
            name: call.tool().to_owned(),
            args: call.args().clone(),
            prompt: call.prompt().to_owned(),
            handoff_path: call.handoff_path().to_vec(),
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
            SessionEvent::ToolDeferred { .. } => "tool_deferred",
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
            | SessionEvent::Compacted { ts, .. }
            | SessionEvent::ToolDeferred { ts, .. } => *ts,
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
///
/// **Repair of old handoff logs.** A session stored by core 0.6.0 or earlier
/// (before SMA-810) has the result of each handoff transfer call at the end
/// of the run, after the events of the target agent. `project` moves each such
/// result to directly after the transfer calls of its handoff response, in
/// call order. If a moved result has the synthetic cancel text, `project`
/// replaces it with `transferred to {target}` (the first transfer call of the
/// response) or `transfer not performed: this response already transferred
/// to {target}` (each other transfer call). No other result moves, and the
/// number of messages does not change. For such a log, the snapshot order
/// therefore no longer follows the event order one-to-one. The stored log
/// does not change. A log with no `HandoffOccurred` event, and a log in the
/// current layout, project in event order.
pub fn project(events: &[SessionEvent]) -> ConversationSnapshot {
    let (mut messages, sources) = project_events(events);
    let moved = repair_legacy_transfer_results(events, &mut messages, &sources);
    if moved > 0 {
        tracing::debug!(
            target: "paigasus::core::session",
            moved,
            "repaired legacy handoff transfer results"
        );
    }
    ConversationSnapshot { messages }
}

/// The index where the run of `ToolCalled` / `AssistantMessage` events
/// directly before `handoff_at` starts.
///
/// The `ToolCalled` events in `start..handoff_at` are the transfer calls of
/// the handoff response that the `HandoffOccurred` at `handoff_at` records.
pub(crate) fn transfer_run_start(events: &[SessionEvent], handoff_at: usize) -> usize {
    handoff_at
        - events[..handoff_at]
            .iter()
            .rev()
            .take_while(|e| {
                matches!(
                    e,
                    SessionEvent::ToolCalled { .. } | SessionEvent::AssistantMessage { .. }
                )
            })
            .count()
}

/// The transfer calls of one handoff response.
struct TransferResponse {
    /// The incoming agent of the `HandoffOccurred` event.
    to: String,
    /// The event index of each transfer call, in call order.
    calls: Vec<usize>,
}

/// Move the late result of each handoff transfer call to directly after the
/// transfer calls of its response (sessions stored before SMA-810), and give
/// a moved synthetic cancel result the transfer text. Returns the number of
/// moved results.
///
/// `sources[m]` is the event index of `messages[m]`. The repair adds and
/// removes no message. A result that is already directly after the transfer
/// calls (the current layout) does not move, and no other result moves.
fn repair_legacy_transfer_results(
    events: &[SessionEvent],
    messages: &mut Vec<Item>,
    sources: &[usize],
) -> usize {
    // Step 1: the transfer calls of each handoff response.
    let mut responses: Vec<TransferResponse> = Vec::new();
    let mut is_transfer = vec![false; events.len()];
    // `response_at[h]`: the response index of the `HandoffOccurred` at `h`.
    let mut response_at: Vec<Option<usize>> = vec![None; events.len()];
    // `response_of[c]`: the response index of the transfer call at `c`.
    let mut response_of: Vec<Option<usize>> = vec![None; events.len()];
    for (at, ev) in events.iter().enumerate() {
        if let SessionEvent::HandoffOccurred { to, .. } = ev {
            let calls: Vec<usize> = (transfer_run_start(events, at)..at)
                .filter(|&i| matches!(events[i], SessionEvent::ToolCalled { .. }))
                .collect();
            for &c in &calls {
                is_transfer[c] = true;
                response_of[c] = Some(responses.len());
            }
            if !calls.is_empty() {
                response_at[at] = Some(responses.len());
                responses.push(TransferResponse {
                    to: to.clone(),
                    calls,
                });
            }
        }
    }
    if responses.is_empty() {
        return 0;
    }

    // Step 2: the result event of each transfer call. A result closes, in
    // this order of preference:
    // 1. if it is in the run of `ToolReturned` events directly after a
    //    `HandoffOccurred`: the earliest open transfer call of that handoff
    //    response with its id;
    // 2. if it has a transfer text: the earliest open transfer call with its
    //    id (a transfer text never closes a non-transfer call while a
    //    transfer call with its id is open);
    // 3. the most recent open non-transfer call with its id;
    // 4. the earliest open transfer call with its id.
    let mut result_of: Vec<Option<usize>> = vec![None; events.len()];
    let mut open: Vec<(usize, &str)> = Vec::new();
    // The response index while the events directly after its handoff are
    // `ToolReturned` events.
    let mut after_handoff: Option<usize> = None;
    for (at, ev) in events.iter().enumerate() {
        let handoff_run = after_handoff.take();
        match ev {
            SessionEvent::HandoffOccurred { .. } => after_handoff = response_at[at],
            SessionEvent::ToolCalled { call_id, .. } => open.push((at, call_id)),
            SessionEvent::ToolReturned { call_id, .. }
            | SessionEvent::ToolDeferred { call_id, .. } => {
                let returned = matches!(ev, SessionEvent::ToolReturned { .. });
                if returned {
                    after_handoff = handoff_run;
                }
                let of_handoff = handoff_run.filter(|_| returned).and_then(|r| {
                    open.iter()
                        .position(|&(c, id)| id == call_id && response_of[c] == Some(r))
                });
                let transfer_text = || {
                    matches!(ev, SessionEvent::ToolReturned { content, .. }
                        if has_transfer_text(content))
                };
                let pos = of_handoff
                    .or_else(|| {
                        transfer_text()
                            .then(|| {
                                open.iter()
                                    .position(|&(c, id)| id == call_id && is_transfer[c])
                            })
                            .flatten()
                    })
                    .or_else(|| {
                        open.iter()
                            .rposition(|&(c, id)| id == call_id && !is_transfer[c])
                    })
                    .or_else(|| {
                        open.iter()
                            .position(|&(c, id)| id == call_id && is_transfer[c])
                    });
                if let Some(p) = pos {
                    let (call, _) = open.remove(p);
                    if is_transfer[call] && matches!(ev, SessionEvent::ToolReturned { .. }) {
                        result_of[call] = Some(at);
                    }
                }
            }
            _ => {}
        }
    }

    // Step 3 (done by the caller): the message index of each event.
    let mut message_of: Vec<Option<usize>> = vec![None; events.len()];
    for (m, &at) in sources.iter().enumerate() {
        message_of[at] = Some(m);
    }

    // Step 4: plan the moves. `inserts` holds (anchor message, moved results);
    // `rewrite[m]` holds the new text of a moved cancel result.
    let mut moved = vec![false; messages.len()];
    let mut rewrite: Vec<Option<String>> = vec![None; messages.len()];
    let mut inserts: Vec<(usize, Vec<usize>)> = Vec::new();
    for response in &responses {
        let Some(last_call) = response.calls.iter().rev().find_map(|&c| message_of[c]) else {
            continue;
        };
        // (message of the result, ordinal of its call in the response)
        let results: Vec<(usize, usize)> = response
            .calls
            .iter()
            .enumerate()
            .filter(|&(_, &c)| message_of[c].is_some())
            .filter_map(|(n, &c)| result_of[c].and_then(|r| message_of[r]).map(|m| (m, n)))
            .collect();
        let in_place = results
            .iter()
            .enumerate()
            .take_while(|&(k, &(m, _))| m == last_call + 1 + k)
            .count();
        if in_place == results.len() {
            continue;
        }
        let mut block = Vec::new();
        for &(m, n) in &results[in_place..] {
            moved[m] = true;
            if is_cancel_text(&messages[m]) {
                rewrite[m] = Some(if n == 0 {
                    format!("transferred to {}", response.to)
                } else {
                    format!(
                        "transfer not performed: this response already transferred to {}",
                        response.to
                    )
                });
            }
            block.push(m);
        }
        inserts.push((last_call + in_place, block));
    }
    if inserts.is_empty() {
        return 0;
    }
    inserts.sort_unstable_by_key(|&(anchor, _)| anchor);

    // One rebuild pass.
    let count = moved.iter().filter(|&&m| m).count();
    let mut slots: Vec<Option<Item>> = std::mem::take(messages).into_iter().map(Some).collect();
    for (m, text) in rewrite.into_iter().enumerate() {
        if let (Some(text), Some(Item::ToolResult { content, .. })) = (text, slots[m].as_mut()) {
            *content = vec![ContentPart::Text { text }];
        }
    }
    let mut out = Vec::with_capacity(slots.len());
    let mut next_insert = inserts.into_iter().peekable();
    for m in 0..slots.len() {
        if !moved[m] {
            out.push(slots[m].take().expect("each message is taken once"));
        }
        while let Some((_, block)) = next_insert.next_if(|(anchor, _)| *anchor == m) {
            for r in block {
                out.push(slots[r].take().expect("each moved result is taken once"));
            }
        }
    }
    *messages = out;
    count
}

/// Whether `content` starts with one of the texts that the runtime gives a
/// handoff transfer call: `transferred to `, `transfer not performed: ` or
/// `transfer failed: `.
fn has_transfer_text(content: &[ContentPart]) -> bool {
    matches!(content.first(), Some(ContentPart::Text { text })
        if ["transferred to ", "transfer not performed: ", "transfer failed: "]
            .iter()
            .any(|p| text.starts_with(p)))
}

/// Whether `item` is a tool result with exactly the synthetic cancel text.
fn is_cancel_text(item: &Item) -> bool {
    matches!(item, Item::ToolResult { content, .. }
        if matches!(content.as_slice(), [ContentPart::Text { text }] if text == CANCELLED_TOOL_CALL_TEXT))
}

/// Project each event to its message, in event order, with no repair.
///
/// Returns the messages and, for each message, the index of the event that
/// produced it (`sources`). The summary message of a `Compacted` event gets
/// the index of that `Compacted` event.
fn project_events(events: &[SessionEvent]) -> (Vec<Item>, Vec<usize>) {
    let mut messages: Vec<Item> = Vec::new();
    let mut sources: Vec<usize> = Vec::new();
    // Parallel vec: contributions[i] = number of messages event i produced.
    // Needed because Compacted has to undo the message yield of the previous
    // N events, and yield varies per variant (HandoffOccurred = 0, others = 1).
    let mut contributions: Vec<usize> = Vec::new();

    for (at, ev) in events.iter().enumerate() {
        match ev {
            SessionEvent::UserMessage { content, .. } => {
                messages.push(Item::UserMessage {
                    content: content.clone(),
                });
                sources.push(at);
                contributions.push(1);
            }
            SessionEvent::AssistantMessage { content, agent, .. } => {
                messages.push(Item::AssistantMessage {
                    content: content.clone(),
                    agent: Some(agent.clone()),
                });
                sources.push(at);
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
                sources.push(at);
                contributions.push(1);
            }
            SessionEvent::ToolReturned {
                call_id, content, ..
            } => {
                messages.push(Item::ToolResult {
                    call_id: call_id.clone(),
                    content: content.clone(),
                });
                sources.push(at);
                contributions.push(1);
            }
            SessionEvent::HandoffOccurred { .. } => {
                // Audit-only event; no message produced.
                contributions.push(0);
            }
            SessionEvent::ToolDeferred { .. } => {
                // Audit and resume record; no message produced.
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
                        target: "paigasus::core::session",
                        "Compacted event with original_count = 0; emitting summary as a system message without compacting any history (likely producer bug — use SessionEvent::compacted with a positive count)"
                    );
                }
                if n > contributions.len() {
                    tracing::warn!(
                        target: "paigasus::core::session",
                        original_count = n,
                        events_seen = contributions.len(),
                        "Compacted event references more events than have been seen; treating as 'compact everything observed so far' (likely corrupt log)"
                    );
                }
                let drop_from_idx = contributions.len().saturating_sub(n);
                let drop_msg_count: usize = contributions[drop_from_idx..].iter().sum();
                let new_len = messages.len() - drop_msg_count;
                messages.truncate(new_len);
                sources.truncate(new_len);
                // Drop the contribution entries the Compacted event subsumed
                // so a subsequent Compacted's `n` indexes back over the
                // remaining events, not the already-summarized ones.
                contributions.truncate(drop_from_idx);
                messages.push(Item::System {
                    content: vec![ContentPart::Text {
                        text: summary.clone(),
                    }],
                });
                sources.push(at);
                contributions.push(1);
            }
        }
    }

    (messages, sources)
}

/// Accumulates one run's semantic items as [`SessionEvent`]s for the runner to
/// persist at run exit.
///
/// The runner pre-seeds the recorder with the run's `AgentInput` messages (the
/// new turn — which the agent does not re-emit on its event stream), then feeds
/// it every [`crate::AgentEvent`] as the stream flows. [`SessionRecorder::drain`]
/// returns the accumulated log, synthesizing a [`SessionEvent::ToolReturned`]
/// for any tool call left without a result (a run interrupted mid-tool) so the
/// log always [`project`]s to a provider-valid conversation. A deferred call
/// gets no synthetic result: it waits for a host decision.
///
/// On a resume ([`SessionRecorder::with_resume_plan`]), the results of the
/// resumed calls go before the recorded input. If the agent rejects the resume
/// before it gives a result for a resumed call, the recorder records nothing,
/// and the deferrals stay open.
#[derive(Debug)]
pub struct SessionRecorder {
    events: Vec<SessionEvent>,
    current_agent: String,
    deferral_slot: Option<crate::DeferralSlot>,
    /// The call ids of the resumed calls (empty when the run is no resume).
    resume_call_ids: Vec<String>,
    /// The tool results that the agent emits before its first turn.
    resume_block: Vec<SessionEvent>,
    /// `true` until the first `TurnStarted` of a resumed run.
    in_resume_window: bool,
    /// `true` when the agent rejected the resume: a `RunFailed` came in the
    /// resume window before a result for a resumed call.
    resume_rejected: bool,
}

impl SessionRecorder {
    /// Create a recorder seeded with the root agent's name. Used to attribute
    /// assistant messages whose [`Item::AssistantMessage`] `agent` is `None`,
    /// before any `RunStarted`/`AgentUpdated` updates the tracked agent.
    pub fn new(root_agent: impl Into<String>) -> Self {
        Self {
            events: Vec::new(),
            current_agent: root_agent.into(),
            deferral_slot: None,
            resume_call_ids: Vec::new(),
            resume_block: Vec::new(),
            in_resume_window: false,
            resume_rejected: false,
        }
    }

    /// Wire the resume plan of a resumed run.
    ///
    /// The runner records the new input before the stream starts, but the
    /// results of the resumed calls must come before that input in the log.
    /// So the recorder keeps every tool result that the agent emits before its
    /// first `TurnStarted` apart, and [`SessionRecorder::drain`] puts these
    /// results first. A resumed call with no result gets the synthetic
    /// cancel result in the same place, so the deferral is closed.
    ///
    /// The one exception is a rejected resume. An agent that cannot resume
    /// (for example a workflow agent) emits `RunFailed` before it gives a
    /// result for a resumed call. No resumed call started, so
    /// [`SessionRecorder::drain`] then returns no events: the session keeps
    /// its open deferrals, and a later resume can still resolve them. An
    /// `LlmAgent` that ends before it runs the calls gives a "did not run"
    /// result for each call before its `RunFailed`, so this exception does
    /// not apply to it. A cancel or a timeout emits no `RunFailed`, so a
    /// resumed call that started still gets the synthetic cancel result.
    #[must_use]
    pub fn with_resume_plan(mut self, plan: &crate::ResumePlan) -> Self {
        self.resume_call_ids = plan.call_ids();
        self.in_resume_window = true;
        self
    }

    /// Wire the run's deferral slot. The slot holds the raw arguments of the
    /// deferred calls. The recorder persists these raw arguments in
    /// [`SessionEvent::ToolDeferred`], because the resume runs the tool with
    /// exactly these arguments. The arguments in [`AgentEvent::RunDeferred`]
    /// can be redacted.
    #[must_use]
    pub fn with_deferral_slot(mut self, slot: crate::DeferralSlot) -> Self {
        self.deferral_slot = Some(slot);
        self
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
                        target: "paigasus::core::session",
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
            AgentEvent::TurnStarted { .. } => {
                self.in_resume_window = false;
            }
            AgentEvent::RunFailed { .. } if self.in_resume_window && !self.has_resume_result() => {
                self.resume_rejected = true;
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
                let event = SessionEvent::tool_returned(call_id.clone(), content.clone());
                if self.in_resume_window {
                    self.resume_block.push(event);
                } else {
                    self.events.push(event);
                }
            }
            AgentEvent::HandoffItem { from, to } => {
                self.events
                    .push(SessionEvent::handoff_occurred(from.clone(), to.clone()));
            }
            AgentEvent::RunDeferred { calls, .. } => {
                let mut warned = false;
                for call in calls {
                    let raw = self
                        .deferral_slot
                        .as_ref()
                        .and_then(|slot| slot.take(call.deferral_id()));
                    match raw {
                        Some(raw) => self.events.push(SessionEvent::tool_deferred(&raw)),
                        None => {
                            if !warned {
                                warned = true;
                                tracing::warn!(
                                    target: "paigasus::core::session",
                                    "RunDeferred call has no raw arguments in the deferral slot; recording the event copy, whose arguments can be redacted"
                                );
                            }
                            self.events.push(SessionEvent::tool_deferred(call));
                        }
                    }
                }
            }
            AgentEvent::UserMessageInjected { content } => {
                self.events
                    .push(SessionEvent::user_message(content.clone()));
            }
            _ => {}
        }
    }

    /// Drain the accumulated events, appending a synthesized
    /// [`SessionEvent::ToolReturned`] for every [`SessionEvent::ToolCalled`]
    /// left without a matching result (a run cancelled/timed out mid-tool).
    /// The returned log always [`project`]s to a provider-valid conversation.
    ///
    /// On a resume, the log starts with the results of the resumed calls (and
    /// a synthesized result for each resumed call without one), then the
    /// recorded input and run events. A rejected resume returns no events.
    ///
    /// The match is by position, because a call id can repeat in one run: a
    /// `ToolCalled` event is closed only by a later `ToolReturned` (or
    /// `ToolDeferred`) event with the same call id, and each later event
    /// closes one call.
    pub fn drain(&mut self) -> Vec<SessionEvent> {
        if std::mem::take(&mut self.resume_rejected) {
            // A rejected resume: record nothing, so the deferrals stay open
            // and no input follows them (see `with_resume_plan`).
            self.resume_block.clear();
            self.resume_call_ids.clear();
            self.events.clear();
            return Vec::new();
        }
        let mut out = std::mem::take(&mut self.resume_block);
        let mut answered: Vec<String> = out
            .iter()
            .filter_map(|e| match e {
                SessionEvent::ToolReturned { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        for call_id in std::mem::take(&mut self.resume_call_ids) {
            match answered.iter().position(|a| *a == call_id) {
                Some(p) => {
                    answered.remove(p);
                }
                None => out.push(cancelled_result(call_id)),
            }
        }
        out.append(&mut self.events);

        let mut unanswered: Vec<String> = Vec::new();
        for e in &out {
            match e {
                SessionEvent::ToolCalled { call_id, .. } => unanswered.push(call_id.clone()),
                SessionEvent::ToolReturned { call_id, .. }
                | SessionEvent::ToolDeferred { call_id, .. } => {
                    if let Some(p) = unanswered.iter().position(|c| c == call_id) {
                        unanswered.remove(p);
                    }
                }
                _ => {}
            }
        }
        out.extend(unanswered.into_iter().map(cancelled_result));
        out
    }
}

impl SessionRecorder {
    /// Whether the resume block holds a result for a resumed call.
    fn has_resume_result(&self) -> bool {
        self.resume_block.iter().any(|e| {
            matches!(e, SessionEvent::ToolReturned { call_id, .. }
                if self.resume_call_ids.contains(call_id))
        })
    }
}

/// The text of the synthetic result for a tool call that a run left without a
/// result.
pub(crate) const CANCELLED_TOOL_CALL_TEXT: &str =
    "tool call did not complete (run cancelled/timed out)";

/// The synthetic result for a tool call that a run left without a result.
fn cancelled_result(call_id: String) -> SessionEvent {
    SessionEvent::tool_returned(
        call_id,
        vec![ContentPart::Text {
            text: CANCELLED_TOOL_CALL_TEXT.to_owned(),
        }],
    )
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

    fn pending(
        deferral_id: &str,
        call_id: &str,
        args: serde_json::Value,
    ) -> crate::PendingToolCall {
        crate::PendingToolCall::new(
            deferral_id,
            call_id,
            "pay",
            args,
            "ok?",
            vec!["root".into()],
        )
    }

    fn tool_call_item(call_id: &str) -> AgentEvent {
        AgentEvent::ToolCallItem {
            item: Item::ToolCall {
                call_id: call_id.into(),
                name: "pay".into(),
                args: serde_json::json!({}),
            },
        }
    }

    fn run_deferred(calls: Vec<crate::PendingToolCall>) -> AgentEvent {
        AgentEvent::RunDeferred {
            calls,
            usage: Default::default(),
        }
    }

    #[test]
    fn observe_run_deferred_records_raw_args_from_the_slot() {
        let slot = crate::DeferralSlot::new();
        slot.record(vec![pending(
            "d1",
            "a",
            serde_json::json!({"token": "sk-raw"}),
        )]);
        let mut r = SessionRecorder::new("root").with_deferral_slot(slot);
        r.observe(&tool_call_item("a"));
        r.observe(&run_deferred(vec![pending(
            "d1",
            "a",
            serde_json::json!({"token": "***"}),
        )]));
        let out = r.drain();
        let args = out
            .iter()
            .find_map(|e| match e {
                SessionEvent::ToolDeferred { args, .. } => Some(args.clone()),
                _ => None,
            })
            .expect("a ToolDeferred event");
        assert_eq!(args, serde_json::json!({"token": "sk-raw"}));
    }

    #[test]
    fn observe_run_deferred_without_a_slot_records_the_event_copy() {
        let mut r = SessionRecorder::new("root");
        r.observe(&tool_call_item("a"));
        r.observe(&run_deferred(vec![pending(
            "d1",
            "a",
            serde_json::json!({"token": "***"}),
        )]));
        let out = r.drain();
        assert!(matches!(
            out.last(),
            Some(SessionEvent::ToolDeferred { args, .. }) if args == &serde_json::json!({"token": "***"})
        ));
    }

    #[test]
    fn observe_does_not_record_approval_requested() {
        let mut r = SessionRecorder::new("root");
        r.observe(&AgentEvent::ApprovalRequested {
            call_id: "a".into(),
            tool: "pay".into(),
            args: serde_json::json!({}),
        });
        assert!(r.drain().is_empty());
    }

    #[test]
    fn drain_does_not_synthesize_for_a_deferred_call() {
        let mut r = SessionRecorder::new("root");
        r.observe(&tool_call_item("a"));
        r.observe(&tool_call_item("b"));
        r.observe(&AgentEvent::ToolOutputItem {
            item: Item::ToolResult {
                call_id: "a".into(),
                content: text("done"),
            },
        });
        r.observe(&run_deferred(vec![pending(
            "d1",
            "b",
            serde_json::json!({}),
        )]));
        let kinds: Vec<_> = r.drain().iter().map(SessionEvent::kind).collect();
        assert_eq!(
            kinds,
            [
                "tool_called",
                "tool_called",
                "tool_returned",
                "tool_deferred"
            ]
        );
    }

    /// A resume plan whose one open deferral is the call `call_id`.
    fn plan_for(call_id: &str) -> crate::ResumePlan {
        let events = vec![
            SessionEvent::user_message(text("hi")),
            SessionEvent::tool_called(call_id, "pay", serde_json::json!({})),
            SessionEvent::tool_deferred(&pending("d1", call_id, serde_json::json!({}))),
        ];
        let input = crate::AgentInput::new().with_resolution("d1", crate::ToolResolution::Approved);
        crate::ResumePlan::from_events(&events, &project(&events), "root", &input, 8).unwrap()
    }

    fn tool_output_item(call_id: &str, s: &str) -> AgentEvent {
        AgentEvent::ToolOutputItem {
            item: Item::ToolResult {
                call_id: call_id.into(),
                content: text(s),
            },
        }
    }

    const CANCEL: &str = CANCELLED_TOOL_CALL_TEXT;

    #[test]
    fn drain_puts_resume_results_before_the_input() {
        let mut r = SessionRecorder::new("root").with_resume_plan(&plan_for("b"));
        r.record_input(&[Item::UserMessage {
            content: text("next"),
        }]);
        r.observe(&tool_output_item("b", "paid"));
        r.observe(&AgentEvent::TurnStarted { turn: 0 });
        r.observe(&AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: text("done"),
                agent: None,
            },
        });
        let out = r.drain();
        let kinds: Vec<_> = out.iter().map(SessionEvent::kind).collect();
        assert_eq!(
            kinds,
            ["tool_returned", "user_message", "assistant_message"]
        );
        assert!(
            matches!(&out[0], SessionEvent::ToolReturned { call_id, content, .. } if call_id == "b" && content == &text("paid"))
        );
    }

    #[test]
    fn drain_closes_an_unanswered_planned_call_with_the_cancel_result() {
        let mut r = SessionRecorder::new("root").with_resume_plan(&plan_for("b"));
        r.record_input(&[Item::UserMessage {
            content: text("next"),
        }]);
        let out = r.drain();
        let kinds: Vec<_> = out.iter().map(SessionEvent::kind).collect();
        assert_eq!(kinds, ["tool_returned", "user_message"]);
        assert!(
            matches!(&out[0], SessionEvent::ToolReturned { call_id, content, .. } if call_id == "b" && content == &text(CANCEL))
        );
    }

    #[test]
    fn drain_records_nothing_for_a_rejected_resume() {
        let mut r = SessionRecorder::new("root").with_resume_plan(&plan_for("b"));
        r.record_input(&[Item::UserMessage {
            content: text("next"),
        }]);
        r.observe(&AgentEvent::RunStarted {
            agent: "seq".into(),
        });
        r.observe(&AgentEvent::RunFailed {
            error: "this agent cannot resume deferred calls".into(),
        });
        assert!(r.drain().is_empty());
    }

    #[test]
    fn drain_keeps_did_not_run_results_before_a_run_failed() {
        let mut r = SessionRecorder::new("root").with_resume_plan(&plan_for("b"));
        r.record_input(&[Item::UserMessage {
            content: text("next"),
        }]);
        r.observe(&tool_output_item("b", "did not run"));
        r.observe(&AgentEvent::RunFailed {
            error: "input guardrail tripwire".into(),
        });
        let kinds: Vec<_> = r.drain().iter().map(SessionEvent::kind).collect();
        assert_eq!(kinds, ["tool_returned", "user_message"]);
    }

    #[test]
    fn drain_matches_results_by_position_not_by_id() {
        let mut r = SessionRecorder::new("root").with_resume_plan(&plan_for("fc_0"));
        r.observe(&tool_output_item("fc_0", "paid"));
        r.observe(&AgentEvent::TurnStarted { turn: 0 });
        r.observe(&tool_call_item("fc_0"));
        let out = r.drain();
        let kinds: Vec<_> = out.iter().map(SessionEvent::kind).collect();
        assert_eq!(kinds, ["tool_returned", "tool_called", "tool_returned"]);
        assert!(
            matches!(out.last(), Some(SessionEvent::ToolReturned { call_id, content, .. }) if call_id == "fc_0" && content == &text(CANCEL))
        );
    }

    #[test]
    fn drain_without_a_plan_keeps_tool_results_in_place() {
        let mut r = SessionRecorder::new("root");
        r.record_input(&[Item::UserMessage {
            content: text("next"),
        }]);
        r.observe(&tool_call_item("a"));
        r.observe(&tool_output_item("a", "ok"));
        let kinds: Vec<_> = r.drain().iter().map(SessionEvent::kind).collect();
        assert_eq!(kinds, ["user_message", "tool_called", "tool_returned"]);
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
    fn observe_user_message_injected_records_user_message() {
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
        r.observe(&AgentEvent::UserMessageInjected {
            content: text("hi"),
        });
        let out = r.drain();
        assert_eq!(out.len(), 3);
        assert!(matches!(&out[0], SessionEvent::ToolCalled { .. }));
        assert!(matches!(&out[1], SessionEvent::ToolReturned { .. }));
        assert!(
            matches!(&out[2], SessionEvent::UserMessage { content, .. } if content == &text("hi"))
        );
        let snapshot = project(&out);
        assert!(
            matches!(snapshot.messages.last(), Some(Item::UserMessage { content }) if content == &text("hi"))
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
            (
                SessionEvent::ToolDeferred {
                    deferral_id: "d".into(),
                    call_id: "c".into(),
                    name: "n".into(),
                    args: serde_json::json!({}),
                    prompt: "p".into(),
                    handoff_path: vec![],
                    ts: epoch(),
                },
                "tool_deferred",
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

#[cfg(test)]
mod repair_tests {
    //! The read-time repair of handoff logs stored before SMA-810 (G4).

    use super::*;

    const TO_BUDGETING: &str = "transferred to budgeting";
    const NOT_PERFORMED: &str =
        "transfer not performed: this response already transferred to budgeting";

    fn text(s: &str) -> Vec<ContentPart> {
        vec![ContentPart::Text { text: s.to_owned() }]
    }
    fn user(s: &str) -> SessionEvent {
        SessionEvent::user_message(text(s))
    }
    fn am(s: &str, agent: &str) -> SessionEvent {
        SessionEvent::assistant_message(text(s), agent)
    }
    fn tc(id: &str, name: &str) -> SessionEvent {
        SessionEvent::tool_called(id, name, serde_json::json!({}))
    }
    fn tr(id: &str, s: &str) -> SessionEvent {
        SessionEvent::tool_returned(id, text(s))
    }
    fn cancel(id: &str) -> SessionEvent {
        tr(id, CANCELLED_TOOL_CALL_TEXT)
    }
    fn ho(from: &str, to: &str) -> SessionEvent {
        SessionEvent::handoff_occurred(from, to)
    }

    /// `(kind, call_id or name, text)` for each projected item.
    fn shape(events: &[SessionEvent]) -> Vec<(&'static str, String, String)> {
        fn joined(content: &[ContentPart]) -> String {
            content
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => text.as_str(),
                    _ => "<non-text>",
                })
                .collect()
        }
        project(events)
            .messages
            .iter()
            .map(|m| match m {
                Item::UserMessage { content } => ("user", String::new(), joined(content)),
                Item::AssistantMessage { content, .. } => ("am", String::new(), joined(content)),
                Item::ToolCall { call_id, name, .. } => ("tc", call_id.clone(), name.clone()),
                Item::ToolResult { call_id, content } => ("tr", call_id.clone(), joined(content)),
                Item::System { content } => ("system", String::new(), joined(content)),
            })
            .collect()
    }

    fn s(kind: &'static str, id: &str, t: &str) -> (&'static str, String, String) {
        (kind, id.to_owned(), t.to_owned())
    }

    #[test]
    fn legacy_transfer_result_moves_next_to_its_call() {
        let events = vec![
            user("hi"),
            tc("t", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            am("child", "budgeting"),
            cancel("t"),
        ];
        assert_eq!(
            shape(&events),
            [
                s("user", "", "hi"),
                s("tc", "t", "transfer_to_budgeting"),
                s("tr", "t", TO_BUDGETING),
                s("am", "", "child"),
            ]
        );
    }

    #[test]
    fn legacy_two_transfer_calls_move_in_call_order() {
        let events = vec![
            user("hi"),
            tc("t", "transfer_to_budgeting"),
            tc("u", "transfer_to_savings"),
            ho("triage", "budgeting"),
            am("child", "budgeting"),
            cancel("t"),
            cancel("u"),
        ];
        assert_eq!(
            shape(&events),
            [
                s("user", "", "hi"),
                s("tc", "t", "transfer_to_budgeting"),
                s("tc", "u", "transfer_to_savings"),
                s("tr", "t", TO_BUDGETING),
                s("tr", "u", NOT_PERFORMED),
                s("am", "", "child"),
            ]
        );
    }

    #[test]
    fn legacy_child_tool_rounds() {
        let events = vec![
            user("hi"),
            tc("t", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            tc("a", "lookup"),
            tr("a", "A"),
            tc("b", "lookup"),
            tr("b", "B"),
            am("done", "budgeting"),
            cancel("t"),
        ];
        assert_eq!(
            shape(&events),
            [
                s("user", "", "hi"),
                s("tc", "t", "transfer_to_budgeting"),
                s("tr", "t", TO_BUDGETING),
                s("tc", "a", "lookup"),
                s("tr", "a", "A"),
                s("tc", "b", "lookup"),
                s("tr", "b", "B"),
                s("am", "", "done"),
            ]
        );
    }

    #[test]
    fn legacy_repeated_id_pairs_each_result_with_its_call() {
        let events = vec![
            user("hi"),
            tc("fc_0", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            tc("fc_0", "lookup"),
            tr("fc_0", "found"),
            tc("c1", "pay"),
            cancel("c1"),
            cancel("fc_0"),
        ];
        assert_eq!(
            shape(&events),
            [
                s("user", "", "hi"),
                s("tc", "fc_0", "transfer_to_budgeting"),
                s("tr", "fc_0", TO_BUDGETING),
                s("tc", "fc_0", "lookup"),
                s("tr", "fc_0", "found"),
                s("tc", "c1", "pay"),
                s("tr", "c1", CANCELLED_TOOL_CALL_TEXT),
            ]
        );
    }

    #[test]
    fn legacy_repeated_id_three_levels() {
        // triage -> budgeting -> savings, every call is `fc_0`.
        let events = vec![
            user("hi"),
            tc("fc_0", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            tc("fc_0", "transfer_to_savings"),
            ho("budgeting", "savings"),
            tc("fc_0", "lookup"),
            tr("fc_0", "found"),
            am("done", "savings"),
            cancel("fc_0"),
            cancel("fc_0"),
        ];
        assert_eq!(
            shape(&events),
            [
                s("user", "", "hi"),
                s("tc", "fc_0", "transfer_to_budgeting"),
                s("tr", "fc_0", TO_BUDGETING),
                s("tc", "fc_0", "transfer_to_savings"),
                s("tr", "fc_0", "transferred to savings"),
                s("tc", "fc_0", "lookup"),
                s("tr", "fc_0", "found"),
                s("am", "", "done"),
            ]
        );
    }

    #[test]
    fn running_child_call_keeps_its_cancel() {
        let events = vec![
            tc("fc_0", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            tc("fc_0", "pay"),
            cancel("fc_0"),
            cancel("fc_0"),
        ];
        assert_eq!(
            shape(&events),
            [
                s("tc", "fc_0", "transfer_to_budgeting"),
                s("tr", "fc_0", TO_BUDGETING),
                s("tc", "fc_0", "pay"),
                s("tr", "fc_0", CANCELLED_TOOL_CALL_TEXT),
            ]
        );
    }

    #[test]
    fn failed_target_run_keeps_the_cancel_text() {
        let events = vec![
            user("hi"),
            tc("t", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            cancel("t"),
        ];
        assert_eq!(
            shape(&events),
            [
                s("user", "", "hi"),
                s("tc", "t", "transfer_to_budgeting"),
                s("tr", "t", CANCELLED_TOOL_CALL_TEXT),
            ]
        );
    }

    #[test]
    fn non_cancel_late_text_moves_and_keeps_its_text() {
        let events = vec![
            user("hi"),
            tc("t", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            am("child", "budgeting"),
            tr("t", "custom"),
        ];
        assert_eq!(
            shape(&events),
            [
                s("user", "", "hi"),
                s("tc", "t", "transfer_to_budgeting"),
                s("tr", "t", "custom"),
                s("am", "", "child"),
            ]
        );
    }

    #[test]
    fn unchanged_logs() {
        let no_handoff = vec![
            user("hi"),
            tc("a", "lookup"),
            am("text", "triage"),
            tr("a", "A"),
        ];
        assert_eq!(
            shape(&no_handoff),
            [
                s("user", "", "hi"),
                s("tc", "a", "lookup"),
                s("am", "", "text"),
                s("tr", "a", "A"),
            ]
        );

        let new_layout = vec![
            tc("t", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            tr("t", TO_BUDGETING),
            am("child", "budgeting"),
        ];
        assert_eq!(
            shape(&new_layout),
            [
                s("tc", "t", "transfer_to_budgeting"),
                s("tr", "t", TO_BUDGETING),
                s("am", "", "child"),
            ]
        );

        let parallel = vec![
            am("A", "a"),
            tc("fc_0", "search"),
            am("B", "b"),
            tc("fc_0", "lookup"),
            tr("fc_0", "search data"),
            tr("fc_0", "lookup data"),
        ];
        assert_eq!(
            shape(&parallel),
            [
                s("am", "", "A"),
                s("tc", "fc_0", "search"),
                s("am", "", "B"),
                s("tc", "fc_0", "lookup"),
                s("tr", "fc_0", "search data"),
                s("tr", "fc_0", "lookup data"),
            ]
        );
    }

    #[test]
    fn compaction_cases() {
        // The compaction removes the transfer call: the result stays in place.
        let removed = vec![
            user("hi"),
            tc("t", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            am("child", "budgeting"),
            SessionEvent::compacted("summary", 3),
            user("more"),
            cancel("t"),
        ];
        assert_eq!(
            shape(&removed),
            [
                s("user", "", "hi"),
                s("system", "", "summary"),
                s("user", "", "more"),
                s("tr", "t", CANCELLED_TOOL_CALL_TEXT),
            ]
        );

        // A compaction before the handoff: the handoff after the summary repairs.
        let before = vec![
            user("old"),
            am("old answer", "triage"),
            SessionEvent::compacted("summary", 2),
            user("hi"),
            tc("t", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            am("child", "budgeting"),
            cancel("t"),
        ];
        assert_eq!(
            shape(&before),
            [
                s("system", "", "summary"),
                s("user", "", "hi"),
                s("tc", "t", "transfer_to_budgeting"),
                s("tr", "t", TO_BUDGETING),
                s("am", "", "child"),
            ]
        );

        // The repair adds and removes no message.
        for events in [&removed, &before] {
            let (raw, _) = project_events(events);
            assert_eq!(project(events).messages.len(), raw.len());
        }
    }

    #[test]
    fn two_handoff_runs_in_one_log_each_repair_once() {
        let events = vec![
            // run 1, old layout
            user("hi"),
            tc("t", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            am("child 1", "budgeting"),
            cancel("t"),
            // run 2, old layout
            user("again"),
            tc("u", "transfer_to_savings"),
            ho("budgeting", "savings"),
            am("child 2", "savings"),
            cancel("u"),
            // run 3, new layout
            user("third"),
            tc("v", "transfer_to_budgeting"),
            ho("savings", "budgeting"),
            tr("v", TO_BUDGETING),
            am("child 3", "budgeting"),
        ];
        assert_eq!(
            shape(&events),
            [
                s("user", "", "hi"),
                s("tc", "t", "transfer_to_budgeting"),
                s("tr", "t", TO_BUDGETING),
                s("am", "", "child 1"),
                s("user", "", "again"),
                s("tc", "u", "transfer_to_savings"),
                s("tr", "u", "transferred to savings"),
                s("am", "", "child 2"),
                s("user", "", "third"),
                s("tc", "v", "transfer_to_budgeting"),
                s("tr", "v", TO_BUDGETING),
                s("am", "", "child 3"),
            ]
        );
        let (mut messages, sources) = project_events(&events);
        assert_eq!(
            repair_legacy_transfer_results(&events, &mut messages, &sources),
            2
        );
    }

    #[test]
    fn text_before_the_transfer_call_keeps_the_result_after_the_call() {
        let events = vec![
            user("hi"),
            am("let me transfer you", "triage"),
            tc("t", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            am("child", "budgeting"),
            cancel("t"),
        ];
        assert_eq!(
            shape(&events),
            [
                s("user", "", "hi"),
                s("am", "", "let me transfer you"),
                s("tc", "t", "transfer_to_budgeting"),
                s("tr", "t", TO_BUDGETING),
                s("am", "", "child"),
            ]
        );
    }

    /// The number of results that the repair moves in `events`.
    fn moves(events: &[SessionEvent]) -> usize {
        let (mut messages, sources) = project_events(events);
        repair_legacy_transfer_results(events, &mut messages, &sources)
    }

    #[test]
    fn new_layout_repeated_id_projects_unchanged() {
        let events = vec![
            user("hi"),
            tc("fc_0", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            tr("fc_0", TO_BUDGETING),
            tc("fc_0", "lookup"),
            tr("fc_0", "S data"),
        ];
        assert_eq!(moves(&events), 0);
        assert_eq!(
            shape(&events),
            [
                s("user", "", "hi"),
                s("tc", "fc_0", "transfer_to_budgeting"),
                s("tr", "fc_0", TO_BUDGETING),
                s("tc", "fc_0", "lookup"),
                s("tr", "fc_0", "S data"),
            ]
        );
    }

    #[test]
    fn result_directly_after_the_handoff_closes_its_transfer_call() {
        // An open non-transfer `fc_0` call (for example from a parallel
        // branch) does not take the result that follows the handoff.
        let events = vec![
            tc("fc_0", "lookup"),
            user("next"),
            tc("fc_0", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            tr("fc_0", "custom"),
            tr("fc_0", "found"),
        ];
        assert_eq!(moves(&events), 0);
        assert_eq!(
            shape(&events),
            [
                s("tc", "fc_0", "lookup"),
                s("user", "", "next"),
                s("tc", "fc_0", "transfer_to_budgeting"),
                s("tr", "fc_0", "custom"),
                s("tr", "fc_0", "found"),
            ]
        );
    }

    #[test]
    fn transfer_text_result_is_never_paired_with_a_non_transfer_call() {
        let events = vec![
            tc("fc_0", "transfer_to_budgeting"),
            ho("triage", "budgeting"),
            tc("fc_0", "lookup"),
            tr("fc_0", TO_BUDGETING),
            tr("fc_0", "S data"),
        ];
        let shape = shape(&events);
        // The transfer text stays with the transfer call, and "S data" stays
        // with the lookup call.
        assert_eq!(
            shape,
            [
                s("tc", "fc_0", "transfer_to_budgeting"),
                s("tr", "fc_0", TO_BUDGETING),
                s("tc", "fc_0", "lookup"),
                s("tr", "fc_0", "S data"),
            ]
        );
        assert_eq!(moves(&events), 1);
        for prefix in ["transfer not performed: x", "transfer failed: x"] {
            let events = vec![
                tc("fc_0", "transfer_to_budgeting"),
                ho("triage", "budgeting"),
                tc("fc_0", "lookup"),
                tr("fc_0", prefix),
                tr("fc_0", "S data"),
            ];
            assert_eq!(shape_of_results(&events), [prefix, "S data"]);
        }
    }

    /// The text of each projected tool result, in order.
    fn shape_of_results(events: &[SessionEvent]) -> Vec<String> {
        shape(events)
            .into_iter()
            .filter(|(kind, _, _)| *kind == "tr")
            .map(|(_, _, t)| t)
            .collect()
    }

    #[test]
    fn repair_is_a_no_op_on_a_new_layout_log() {
        let run = |n: &str| {
            vec![
                user(n),
                tc(&format!("t{n}"), "transfer_to_budgeting"),
                tc(&format!("u{n}"), "transfer_to_savings"),
                ho("triage", "budgeting"),
                tr(&format!("t{n}"), TO_BUDGETING),
                tr(&format!("u{n}"), NOT_PERFORMED),
                tc("fc_0", "lookup"),
                tr("fc_0", "found"),
                am("child", "budgeting"),
            ]
        };
        let mut events = run("1");
        let first = shape(&events);
        for n in ["2", "3"] {
            events.extend(run(n));
            let (mut messages, sources) = project_events(&events);
            assert_eq!(
                repair_legacy_transfer_results(&events, &mut messages, &sources),
                0
            );
            assert_eq!(shape(&events)[..first.len()], first[..]);
        }
    }
}
