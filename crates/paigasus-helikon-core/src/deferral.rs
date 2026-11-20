//! Deferred tool calls: the types a run uses to stop on a tool call that needs
//! a later decision, and the types a host uses to resume the run.

use std::fmt;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// A tool call that a run deferred instead of running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PendingToolCall {
    deferral_id: String,
    call_id: String,
    tool: String,
    args: serde_json::Value,
    prompt: String,
    handoff_path: Vec<String>,
}

impl PendingToolCall {
    /// Make a pending tool call.
    pub fn new(
        deferral_id: impl Into<String>,
        call_id: impl Into<String>,
        tool: impl Into<String>,
        args: serde_json::Value,
        prompt: impl Into<String>,
        handoff_path: Vec<String>,
    ) -> Self {
        Self {
            deferral_id: deferral_id.into(),
            call_id: call_id.into(),
            tool: tool.into(),
            args,
            prompt: prompt.into(),
            handoff_path,
        }
    }

    /// The id the host uses to resolve this deferral.
    pub fn deferral_id(&self) -> &str {
        &self.deferral_id
    }

    /// The model-assigned id of the tool call.
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// The name of the tool.
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// The arguments of the tool call.
    pub fn args(&self) -> &serde_json::Value {
        &self.args
    }

    /// The prompt for the approver.
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// The names of the agents from the root agent to the deferring agent.
    pub fn handoff_path(&self) -> &[String] {
        &self.handoff_path
    }

    /// The name of the agent that deferred the call (the last name in the path).
    pub fn agent(&self) -> Option<&str> {
        self.handoff_path.last().map(String::as_str)
    }

    /// The redacted event copy of this call. The redaction applies to the
    /// arguments and to the prompt, because a policy can make the prompt from
    /// the arguments.
    pub(crate) fn redacted(&self, secrets: &crate::redaction::SecretSet) -> Self {
        Self {
            args: crate::redaction::redact(&self.args, secrets),
            prompt: crate::redaction::redact_str(&self.prompt, secrets),
            ..self.clone()
        }
    }
}

/// Make a new deferral id.
pub(crate) fn new_deferral_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The host decision for one deferred tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolResolution {
    /// Run the tool call.
    Approved,
    /// Do not run the tool call.
    Denied {
        /// The reason the model sees in the tool result.
        reason: String,
    },
}

/// Why a run stopped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopReason {
    /// The run reached a final answer.
    #[default]
    Completed,
    /// The run stopped on tool calls that need a decision.
    Deferred {
        /// The deferred tool calls.
        calls: Vec<PendingToolCall>,
    },
}

/// A resume request that the runner rejects as invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InvalidResume {
    /// The session has no open deferred tool calls.
    #[error("the session has no open deferred tool calls")]
    NoOpenDeferrals,
    /// Some open deferred tool calls have no resolution.
    #[error("open deferred tool calls have no resolution: {missing:?}")]
    UnresolvedDeferrals {
        /// The deferral ids with no resolution.
        missing: Vec<String>,
    },
    /// A resolution names a deferral that is not open.
    #[error("no open deferred tool call has the id {deferral_id}")]
    UnknownDeferral {
        /// The unknown deferral id.
        deferral_id: String,
    },
    /// Two resolutions name the same deferral.
    #[error("two resolutions name the deferral {deferral_id}")]
    DuplicateResolution {
        /// The deferral id that two resolutions name.
        deferral_id: String,
    },
    /// The session changed after the deferral was recorded.
    #[error("the session changed after the deferred tool calls were recorded")]
    SessionModified,
    /// The session history has no open tool call for a deferral.
    ///
    /// A compactor removed the deferred `ToolCall` item. No resume can fix
    /// such a session.
    #[error("the session history has no open tool call {call_id} for a deferral")]
    CallNotInHistory {
        /// The call id that the history lacks.
        call_id: String,
    },
    /// The resume input contains tool call or tool result items, at the top
    /// level or nested in a message as `ContentPart::ToolUse` or
    /// `ContentPart::ToolResult`.
    #[error("the resume input contains tool call or tool result items")]
    ToolItemsInInput,
    /// The handoff path is longer than the agent depth limit allows.
    #[error(
        "the handoff path has {len} agents, more than max_agent_depth + 1 (max_agent_depth = {max_agent_depth})"
    )]
    PathTooDeep {
        /// The number of agents in the path.
        len: usize,
        /// The configured agent depth limit.
        max_agent_depth: u32,
    },
    /// The runner cannot resume deferred tool calls.
    #[error("this runner cannot resume deferred tool calls")]
    ResumeUnsupported,
}

/// A shared slot where a run records the tool calls it deferred.
#[derive(Clone, Default)]
pub struct DeferralSlot(Arc<Mutex<Vec<PendingToolCall>>>);

impl DeferralSlot {
    /// Make an empty slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// A copy of the recorded calls.
    pub fn calls(&self) -> Vec<PendingToolCall> {
        self.0.lock().unwrap().clone()
    }

    /// Remove and return the call with this deferral id.
    pub(crate) fn take(&self, deferral_id: &str) -> Option<PendingToolCall> {
        let mut calls = self.0.lock().unwrap();
        let idx = calls.iter().position(|c| c.deferral_id() == deferral_id)?;
        Some(calls.remove(idx))
    }

    /// Append calls to the slot.
    pub(crate) fn record(&self, calls: Vec<PendingToolCall>) {
        self.0.lock().unwrap().extend(calls);
    }
}

impl fmt::Debug for DeferralSlot {
    // The slot holds raw tool arguments, so the output shows only the count.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeferralSlot")
            .field("calls", &self.0.lock().unwrap().len())
            .finish()
    }
}

/// Find the deferrals in a session log that no result has closed.
///
/// A [`SessionEvent::ToolDeferred`](crate::SessionEvent::ToolDeferred) at
/// position `i` is open when no
/// [`SessionEvent::ToolReturned`](crate::SessionEvent::ToolReturned) with the same `call_id` follows at a position
/// after `i`. The calls come in log order and carry the raw arguments from the
/// event fields.
///
/// While a deferral is open, the log holds a `ToolCall` without a
/// `ToolResult`. A `snapshot()` of that session is therefore not valid
/// provider input.
pub fn open_deferrals(events: &[crate::SessionEvent]) -> Vec<PendingToolCall> {
    open_deferral_positions(events)
        .into_iter()
        .map(|(_, call)| call)
        .collect()
}

/// [`open_deferrals`] with the log index of each `ToolDeferred` event.
fn open_deferral_positions(events: &[crate::SessionEvent]) -> Vec<(usize, PendingToolCall)> {
    use crate::SessionEvent;
    let mut open = Vec::new();
    for (i, event) in events.iter().enumerate() {
        let SessionEvent::ToolDeferred {
            deferral_id,
            call_id,
            name,
            args,
            prompt,
            handoff_path,
            ..
        } = event
        else {
            continue;
        };
        let closed = events[i + 1..]
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolReturned { call_id: c, .. } if c == call_id));
        if !closed {
            open.push((
                i,
                PendingToolCall::new(
                    deferral_id.clone(),
                    call_id.clone(),
                    name.clone(),
                    args.clone(),
                    prompt.clone(),
                    handoff_path.clone(),
                ),
            ));
        }
    }
    open
}

/// One open deferral of a [`ResumePlan`], with the host decision for it.
#[derive(Clone)]
pub(crate) struct PlannedCall {
    pub(crate) pending: PendingToolCall,
    pub(crate) resolution: ToolResolution,
}

impl fmt::Debug for PlannedCall {
    // The pending call holds raw tool arguments, so the output omits them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlannedCall")
            .field("deferral_id", &self.pending.deferral_id())
            .field("call_id", &self.pending.call_id())
            .field("tool", &self.pending.tool())
            .field("resolution", &self.resolution)
            .finish_non_exhaustive()
    }
}

/// A validated resume of a session with open deferred tool calls.
///
/// The plan holds each open deferral of the session, in log order, with the
/// host decision for it. It also holds the handoff path of the deferring
/// agent and the conversation that each agent on that path saw.
///
/// Only [`ResumePlan::from_events`] makes a plan. A deferral-aware runner
/// makes it and gives it to the agent with
/// [`AgentInput::with_resume_plan`](crate::AgentInput::with_resume_plan).
#[derive(Clone)]
pub struct ResumePlan {
    /// The open deferrals in log order, each with its resolution.
    calls: Vec<PlannedCall>,
    /// The shared handoff path of the open deferrals.
    path: Vec<String>,
    /// `histories[i]` is the conversation of `path[i]`; `[0]` is the snapshot.
    histories: Vec<Vec<crate::Item>>,
    /// `transfers[i]` is the transfer call id from `path[i]` to `path[i + 1]`.
    transfers: Vec<Option<String>>,
    /// The path entry of the agent that receives this plan.
    level: usize,
}

impl fmt::Debug for ResumePlan {
    // The calls and the histories hold raw tool arguments, so the output
    // shows only ids, names and lengths.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResumePlan")
            .field("calls", &self.calls)
            .field("path", &self.path)
            .field(
                "history_lens",
                &self.histories.iter().map(Vec::len).collect::<Vec<_>>(),
            )
            .field("transfers", &self.transfers)
            .field("level", &self.level)
            .finish()
    }
}

impl ResumePlan {
    /// Validate a resume request against the session log and make a plan.
    ///
    /// `events` is the full session log and `snapshot` is the session
    /// snapshot (a custom session can override the snapshot, so the runner
    /// reads both). `root_agent_name` is the name of the agent that the
    /// runner runs. `input` carries the host resolutions and the new input
    /// messages.
    ///
    /// The checks run in this order: tool items in the input, duplicate
    /// resolutions, no open deferrals, unknown deferral ids, unresolved
    /// deferrals, a session changed after the deferral, a deferred call that
    /// is not in the history, and a handoff path that is too deep.
    ///
    /// A `root_agent_name` that is not the first name of the handoff path is
    /// not an error. The agent then denies every call.
    pub fn from_events(
        events: &[crate::SessionEvent],
        snapshot: &crate::ConversationSnapshot,
        root_agent_name: &str,
        input: &crate::AgentInput,
        max_agent_depth: u32,
    ) -> Result<ResumePlan, InvalidResume> {
        use crate::{Item, SessionEvent};

        // A nested `ContentPart::ToolUse` or `ContentPart::ToolResult` is
        // equal to a top-level tool item, so it is rejected too.
        let has_tool_part = |content: &[crate::ContentPart]| {
            content.iter().any(|p| {
                matches!(
                    p,
                    crate::ContentPart::ToolUse { .. } | crate::ContentPart::ToolResult { .. }
                )
            })
        };
        if input.messages.iter().any(|i| match i {
            Item::ToolCall { .. } | Item::ToolResult { .. } => true,
            Item::UserMessage { content }
            | Item::AssistantMessage { content, .. }
            | Item::System { content } => has_tool_part(content),
        }) {
            return Err(InvalidResume::ToolItemsInInput);
        }

        let resolutions = input.resolutions();
        for (n, (id, _)) in resolutions.iter().enumerate() {
            if resolutions[..n].iter().any(|(other, _)| other == id) {
                return Err(InvalidResume::DuplicateResolution {
                    deferral_id: id.clone(),
                });
            }
        }

        let open = open_deferral_positions(events);
        let Some(&(first, _)) = open.first() else {
            return Err(InvalidResume::NoOpenDeferrals);
        };

        for (id, _) in resolutions {
            if !open.iter().any(|(_, c)| c.deferral_id() == id) {
                return Err(InvalidResume::UnknownDeferral {
                    deferral_id: id.clone(),
                });
            }
        }

        let missing: Vec<String> = open
            .iter()
            .filter(|(_, c)| !resolutions.iter().any(|(id, _)| id == c.deferral_id()))
            .map(|(_, c)| c.deferral_id().to_owned())
            .collect();
        if !missing.is_empty() {
            return Err(InvalidResume::UnresolvedDeferrals { missing });
        }

        if events[first + 1..].iter().any(|e| {
            matches!(
                e,
                SessionEvent::UserMessage { .. }
                    | SessionEvent::AssistantMessage { .. }
                    | SessionEvent::ToolCalled { .. }
            )
        }) {
            return Err(InvalidResume::SessionModified);
        }
        let path = open[0].1.handoff_path().to_vec();
        if path.is_empty() || open.iter().any(|(_, c)| c.handoff_path() != path) {
            return Err(InvalidResume::SessionModified);
        }
        let handoffs = handoff_indices(events, &path, first)?;

        for (_, call) in &open {
            let messages = &snapshot.messages;
            let found = messages.iter().rposition(
                |i| matches!(i, Item::ToolCall { call_id, .. } if call_id == call.call_id()),
            );
            let valid = found.is_some_and(|p| {
                let same_name =
                    matches!(&messages[p], Item::ToolCall { name, .. } if name == call.tool());
                let answered = messages[p + 1..].iter().any(
                    |i| matches!(i, Item::ToolResult { call_id, .. } if call_id == call.call_id()),
                );
                same_name && !answered
            });
            if !valid {
                return Err(InvalidResume::CallNotInHistory {
                    call_id: call.call_id().to_owned(),
                });
            }
        }

        // `path` is nonempty here (checked above). Compare the depth, not
        // `len > max + 1`: the addition can overflow `usize` on a 32-bit target.
        if path.len() - 1 > max_agent_depth as usize {
            return Err(InvalidResume::PathTooDeep {
                len: path.len(),
                max_agent_depth,
            });
        }

        let mut histories = vec![snapshot.messages.clone()];
        if root_agent_name == path[0] {
            for level in 1..path.len() {
                histories.push(agent_view(events, &handoffs, level, events.len()));
            }
        }
        let transfers = (1..path.len())
            .map(|level| transfer_call_before(events, handoffs[level]))
            .collect();

        let calls = open
            .into_iter()
            .map(|(_, pending)| {
                let resolution = resolutions
                    .iter()
                    .find(|(id, _)| id == pending.deferral_id())
                    .map(|(_, r)| r.clone())
                    .expect("every open deferral has a resolution");
                PlannedCall {
                    pending,
                    resolution,
                }
            })
            .collect();

        Ok(ResumePlan {
            calls,
            path,
            histories,
            transfers,
            level: 0,
        })
    }

    /// The open deferrals in log order, each with its resolution.
    pub(crate) fn calls(&self) -> &[PlannedCall] {
        &self.calls
    }

    /// The call ids of the open deferrals, in log order.
    pub(crate) fn call_ids(&self) -> Vec<String> {
        self.calls
            .iter()
            .map(|c| c.pending.call_id().to_owned())
            .collect()
    }

    /// The name of the agent that must receive this plan.
    pub(crate) fn expected_agent(&self) -> &str {
        &self.path[self.level]
    }

    /// The name of the handoff target after the receiving agent, if any.
    pub(crate) fn next_agent(&self) -> Option<&str> {
        self.path.get(self.level + 1).map(String::as_str)
    }

    /// The transfer call id from the receiving agent to [`Self::next_agent`].
    pub(crate) fn transfer_call_to_next(&self) -> Option<&str> {
        self.transfers.get(self.level)?.as_deref()
    }

    /// The conversation that the receiving agent saw when the call was
    /// deferred. At the root level this is the session snapshot, also for a
    /// renamed root agent. Empty for a deeper level when the plan has no
    /// history for it (the plan of a renamed root keeps only the snapshot).
    pub(crate) fn history(&self) -> &[crate::Item] {
        self.histories.get(self.level).map_or(&[], Vec::as_slice)
    }

    /// The plan for the next agent on the handoff path.
    pub(crate) fn descend(&self) -> ResumePlan {
        ResumePlan {
            level: self.level + 1,
            ..self.clone()
        }
    }

    /// A copy of this plan where every resolution is `Denied { reason }`.
    pub(crate) fn with_all_denied(&self, reason: &str) -> ResumePlan {
        let mut plan = self.clone();
        for call in &mut plan.calls {
            call.resolution = ToolResolution::Denied {
                reason: reason.to_owned(),
            };
        }
        plan
    }
}

/// The log index `k_i` of the `HandoffOccurred` event into `path[i]` for each
/// level `i` (`k_0 = 0`), all before `deferred_at`.
///
/// The search runs from the deepest level back to the root: `k_i` is the last
/// matching handoff before `k_{i+1}`. So a path that visits an agent two times
/// finds the handoffs of the latest chain.
fn handoff_indices(
    events: &[crate::SessionEvent],
    path: &[String],
    deferred_at: usize,
) -> Result<Vec<usize>, InvalidResume> {
    use crate::SessionEvent;
    let mut indices = vec![0; path.len()];
    let mut upper = deferred_at;
    for level in (1..path.len()).rev() {
        let k = events[..upper]
            .iter()
            .rposition(|e| {
                matches!(e, SessionEvent::HandoffOccurred { from, to, .. }
                    if *from == path[level - 1] && *to == path[level])
            })
            .ok_or(InvalidResume::SessionModified)?;
        indices[level] = k;
        upper = k;
    }
    Ok(indices)
}

/// `view(level, end)`: the conversation of `path[level]` over `events[..end]`.
///
/// `view(0, end)` is the projection of `events[..end]`. A child view threads
/// the parent view at its handoff (no tool items, plus a transfer note) and
/// then adds the projection of the events after the handoff.
///
/// A child view pairs each tool result with one open call of the view: the
/// result closes the earliest open call with its call id. The view drops a
/// result that closes no open call. The session recorder closes the
/// transfer calls of a run with a synthetic result after the handoff, and
/// the child view has no transfer call. A child call can use the call id of
/// a transfer call, so a match by call id alone can keep two results for one
/// call.
fn agent_view(
    events: &[crate::SessionEvent],
    handoffs: &[usize],
    level: usize,
    end: usize,
) -> Vec<crate::Item> {
    use crate::Item;
    if level == 0 {
        return crate::project(&events[..end]).messages;
    }
    let k = handoffs[level];
    let mut view =
        crate::loop_state::thread_handoff_transcript(&agent_view(events, handoffs, level - 1, k));
    view.extend(crate::project(&events[k + 1..end]).messages);
    // The call ids of the open calls, in view order.
    let mut open: Vec<String> = Vec::new();
    view.retain(|item| match item {
        Item::ToolCall { call_id, .. } => {
            open.push(call_id.clone());
            true
        }
        Item::ToolResult { call_id, .. } => match open.iter().position(|c| c == call_id) {
            Some(p) => {
                open.remove(p);
                true
            }
            None => false,
        },
        _ => true,
    });
    view
}

/// The transfer call id of the handoff at `handoff_at`.
///
/// The loop records the transfer `ToolCalled` events of a handoff response
/// directly before the `HandoffOccurred` event, with the assistant text of
/// that response. The first transfer call names the target. This finds the
/// call by its position, not by its tool name, because a handoff can use a
/// custom tool name.
fn transfer_call_before(events: &[crate::SessionEvent], handoff_at: usize) -> Option<String> {
    use crate::SessionEvent;
    events[..handoff_at]
        .iter()
        .rev()
        .take_while(|e| {
            matches!(
                e,
                SessionEvent::ToolCalled { .. } | SessionEvent::AssistantMessage { .. }
            )
        })
        .filter_map(|e| match e {
            SessionEvent::ToolCalled { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .last()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_tool_call_agent_is_last_path_name() {
        let c = PendingToolCall::new(
            "d",
            "c",
            "t",
            serde_json::json!({}),
            "p",
            vec!["a".into(), "b".into()],
        );
        assert_eq!(c.agent(), Some("b"));
        let empty = PendingToolCall::new("d", "c", "t", serde_json::json!({}), "p", vec![]);
        assert_eq!(empty.agent(), None);
    }

    #[test]
    fn stop_reason_default_is_completed() {
        assert_eq!(StopReason::default(), StopReason::Completed);
    }

    #[test]
    fn new_deferral_id_is_a_v4_uuid() {
        let id = new_deferral_id();
        assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
        assert_ne!(id, new_deferral_id());
    }

    #[test]
    fn deferral_slot_shares_records_across_clones() {
        let a = DeferralSlot::new();
        let b = a.clone();
        b.record(vec![PendingToolCall::new(
            "d",
            "c",
            "t",
            serde_json::json!({}),
            "p",
            vec![],
        )]);
        assert_eq!(a.calls().len(), 1);
        assert!(!format!("{a:?}").contains("\"t\""));
    }

    use crate::{project, AgentInput, ContentPart, Item, SessionEvent};

    fn text(s: &str) -> Vec<ContentPart> {
        vec![ContentPart::Text { text: s.to_owned() }]
    }

    fn deferred(deferral_id: &str, call_id: &str, path: &[&str]) -> SessionEvent {
        SessionEvent::tool_deferred(&PendingToolCall::new(
            deferral_id,
            call_id,
            "pay",
            serde_json::json!({"amount": 5}),
            "ok?",
            path.iter().map(|s| (*s).to_owned()).collect(),
        ))
    }

    /// user "hi"; assistant (root); calls c1 and c2; both deferred.
    fn two_deferrals() -> Vec<SessionEvent> {
        vec![
            SessionEvent::user_message(text("hi")),
            SessionEvent::assistant_message(text("paying"), "root"),
            SessionEvent::tool_called("c1", "pay", serde_json::json!({"amount": 5})),
            SessionEvent::tool_called("c2", "pay", serde_json::json!({"amount": 5})),
            deferred("d1", "c1", &["root"]),
            deferred("d2", "c2", &["root"]),
        ]
    }

    fn approve_both() -> AgentInput {
        AgentInput::new()
            .with_resolution("d1", ToolResolution::Approved)
            .with_resolution("d2", ToolResolution::Approved)
    }

    fn plan(events: &[SessionEvent], input: &AgentInput) -> Result<ResumePlan, InvalidResume> {
        ResumePlan::from_events(events, &project(events), "root", input, 8)
    }

    #[test]
    fn from_events_rejects_tool_items_in_input() {
        let mut input = approve_both();
        input.messages.push(Item::ToolResult {
            call_id: "c1".into(),
            content: text("x"),
        });
        assert_eq!(
            plan(&two_deferrals(), &input).unwrap_err(),
            InvalidResume::ToolItemsInInput
        );
    }

    #[test]
    fn from_events_rejects_duplicate_resolution() {
        let input = approve_both().with_resolution("d1", ToolResolution::Approved);
        assert_eq!(
            plan(&two_deferrals(), &input).unwrap_err(),
            InvalidResume::DuplicateResolution {
                deferral_id: "d1".into()
            }
        );
    }

    #[test]
    fn from_events_rejects_resolutions_without_open_deferrals() {
        let events = vec![SessionEvent::user_message(text("hi"))];
        assert_eq!(
            plan(&events, &approve_both()).unwrap_err(),
            InvalidResume::NoOpenDeferrals
        );
    }

    #[test]
    fn from_events_rejects_empty_input_without_open_deferrals() {
        let events = vec![SessionEvent::user_message(text("hi"))];
        assert_eq!(
            plan(&events, &AgentInput::new()).unwrap_err(),
            InvalidResume::NoOpenDeferrals
        );
    }

    #[test]
    fn from_events_rejects_unknown_deferral() {
        let input = approve_both().with_resolution("zz", ToolResolution::Approved);
        assert_eq!(
            plan(&two_deferrals(), &input).unwrap_err(),
            InvalidResume::UnknownDeferral {
                deferral_id: "zz".into()
            }
        );
    }

    #[test]
    fn from_events_rejects_missing_resolution() {
        let input = AgentInput::new().with_resolution("d1", ToolResolution::Approved);
        assert_eq!(
            plan(&two_deferrals(), &input).unwrap_err(),
            InvalidResume::UnresolvedDeferrals {
                missing: vec!["d2".into()]
            }
        );
    }

    #[test]
    fn from_events_rejects_empty_input_with_open_deferrals() {
        assert_eq!(
            plan(&two_deferrals(), &AgentInput::new()).unwrap_err(),
            InvalidResume::UnresolvedDeferrals {
                missing: vec!["d1".into(), "d2".into()]
            }
        );
    }

    #[test]
    fn from_events_rejects_session_modified() {
        let mut events = two_deferrals();
        events.push(SessionEvent::user_message(text("later")));
        assert_eq!(
            plan(&events, &approve_both()).unwrap_err(),
            InvalidResume::SessionModified
        );
    }

    #[test]
    fn from_events_rejects_different_handoff_paths() {
        let mut events = two_deferrals();
        events[5] = deferred("d2", "c2", &["other"]);
        assert_eq!(
            plan(&events, &approve_both()).unwrap_err(),
            InvalidResume::SessionModified
        );
    }

    #[test]
    fn from_events_rejects_call_not_in_history() {
        let events = two_deferrals();
        let mut snapshot = project(&events);
        snapshot
            .messages
            .retain(|i| !matches!(i, Item::ToolCall { call_id, .. } if call_id == "c2"));
        assert_eq!(
            ResumePlan::from_events(&events, &snapshot, "root", &approve_both(), 8).unwrap_err(),
            InvalidResume::CallNotInHistory {
                call_id: "c2".into()
            }
        );
    }

    /// root → mid → leaf; the leaf defers c1.
    fn three_level() -> Vec<SessionEvent> {
        vec![
            SessionEvent::user_message(text("hi")),
            SessionEvent::tool_called("t1", "transfer_to_mid", serde_json::json!({})),
            SessionEvent::handoff_occurred("root", "mid"),
            SessionEvent::tool_called("t2", "go_leaf", serde_json::json!({})),
            SessionEvent::handoff_occurred("mid", "leaf"),
            SessionEvent::tool_called("c1", "pay", serde_json::json!({"amount": 5})),
            deferred("d1", "c1", &["root", "mid", "leaf"]),
        ]
    }

    #[test]
    fn from_events_rejects_path_too_deep() {
        let events = three_level();
        let input = AgentInput::new().with_resolution("d1", ToolResolution::Approved);
        assert_eq!(
            ResumePlan::from_events(&events, &project(&events), "root", &input, 1).unwrap_err(),
            InvalidResume::PathTooDeep {
                len: 3,
                max_agent_depth: 1
            }
        );
        assert!(ResumePlan::from_events(&events, &project(&events), "root", &input, 2).is_ok());
    }

    #[test]
    fn from_events_rejects_a_missing_handoff_event() {
        let mut events = three_level();
        events.remove(4);
        let input = AgentInput::new().with_resolution("d1", ToolResolution::Approved);
        assert_eq!(
            plan(&events, &input).unwrap_err(),
            InvalidResume::SessionModified
        );
    }

    #[test]
    fn from_events_accepts_a_renamed_root() {
        let events = two_deferrals();
        let p = ResumePlan::from_events(&events, &project(&events), "other", &approve_both(), 8)
            .unwrap();
        assert_eq!(p.expected_agent(), "root");
        assert_eq!(p.call_ids(), ["c1", "c2"]);
    }

    #[test]
    fn from_events_keeps_log_order_and_resolutions() {
        let input = AgentInput::new()
            .with_resolution(
                "d2",
                ToolResolution::Denied {
                    reason: "no".into(),
                },
            )
            .with_resolution("d1", ToolResolution::Approved);
        let p = plan(&two_deferrals(), &input).unwrap();
        assert_eq!(p.call_ids(), ["c1", "c2"]);
        assert_eq!(p.calls()[0].resolution, ToolResolution::Approved);
        assert_eq!(
            p.calls()[1].resolution,
            ToolResolution::Denied {
                reason: "no".into()
            }
        );
        assert_eq!(p.next_agent(), None);
        assert_eq!(p.transfer_call_to_next(), None);
        assert_eq!(
            format!("{:?}", p.history()),
            format!("{:?}", project(&two_deferrals()).messages)
        );
        let denied = p.with_all_denied("gone");
        assert!(denied.calls().iter().all(|c| c.resolution
            == ToolResolution::Denied {
                reason: "gone".into()
            }));
    }

    fn handoff_fixture() -> Vec<SessionEvent> {
        vec![
            SessionEvent::user_message(text("hi")),
            SessionEvent::assistant_message(text("routing"), "triage"),
            SessionEvent::tool_called("t1", "transfer_to_budgeting", serde_json::json!({})),
            SessionEvent::handoff_occurred("triage", "budgeting"),
            SessionEvent::assistant_message(text("checking"), "budgeting"),
            SessionEvent::tool_called("c1", "pay", serde_json::json!({"amount": 5})),
            deferred("d1", "c1", &["triage", "budgeting"]),
        ]
    }

    #[test]
    fn from_events_builds_the_child_history_without_transfer_or_root_tool_calls() {
        let events = handoff_fixture();
        let input = AgentInput::new().with_resolution("d1", ToolResolution::Approved);
        let p = ResumePlan::from_events(&events, &project(&events), "triage", &input, 8).unwrap();
        assert_eq!(p.expected_agent(), "triage");
        assert_eq!(p.next_agent(), Some("budgeting"));
        assert_eq!(
            format!("{:?}", p.history()),
            format!("{:?}", project(&events).messages)
        );
        let child = p.descend();
        assert_eq!(child.expected_agent(), "budgeting");
        assert_eq!(child.next_agent(), None);
        let history = child.history();
        let note = "You are now handling a transferred conversation. \
                    Continue assisting the user.";
        assert_eq!(history.len(), 5, "{history:?}");
        assert!(matches!(&history[0], Item::UserMessage { content } if content == &text("hi")));
        assert!(
            matches!(&history[1], Item::AssistantMessage { content, .. } if content == &text("routing"))
        );
        assert!(matches!(&history[2], Item::UserMessage { content } if content == &text(note)));
        assert!(
            matches!(&history[3], Item::AssistantMessage { content, .. } if content == &text("checking"))
        );
        assert!(matches!(&history[4], Item::ToolCall { call_id, .. } if call_id == "c1"));
        assert!(!history
            .iter()
            .any(|i| matches!(i, Item::ToolCall { call_id, .. } if call_id == "t1")));
    }

    #[test]
    fn child_history_drops_the_recorder_result_of_a_transfer_call() {
        // The session recorder closes each transfer call at the end of the
        // run with a synthetic result.
        let mut events = three_level();
        for id in ["t1", "t2"] {
            events.push(SessionEvent::tool_returned(id, text("cancelled")));
        }
        let input = AgentInput::new().with_resolution("d1", ToolResolution::Approved);
        let p = plan(&events, &input).unwrap();
        let mid = p.descend();
        let leaf = mid.descend();
        let result_ids = |h: &[Item]| -> Vec<String> {
            h.iter()
                .filter_map(|i| match i {
                    Item::ToolResult { call_id, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(result_ids(p.history()), ["t1", "t2"]);
        assert_eq!(result_ids(mid.history()), ["t2"]);
        assert!(
            result_ids(leaf.history()).is_empty(),
            "{:?}",
            leaf.history()
        );
        assert!(
            matches!(leaf.history().last(), Some(Item::ToolCall { call_id, .. }) if call_id == "c1")
        );
    }

    #[test]
    fn child_history_pairs_each_result_with_one_open_call() {
        // The child's allowed call reuses the transfer call id `fc_0`. The
        // log then has two `fc_0` results: the real one and the recorder's
        // synthetic one. The child view keeps one result per call.
        let events = vec![
            SessionEvent::user_message(text("hi")),
            SessionEvent::tool_called("fc_0", "transfer_to_budgeting", serde_json::json!({})),
            SessionEvent::handoff_occurred("triage", "budgeting"),
            SessionEvent::tool_called("fc_0", "lookup", serde_json::json!({})),
            SessionEvent::tool_returned("fc_0", text("found")),
            SessionEvent::tool_called("c1", "pay", serde_json::json!({"amount": 5})),
            deferred("d1", "c1", &["triage", "budgeting"]),
            SessionEvent::tool_returned("fc_0", text("cancelled")),
        ];
        let input = AgentInput::new().with_resolution("d1", ToolResolution::Approved);
        let p = ResumePlan::from_events(&events, &project(&events), "triage", &input, 8).unwrap();
        let child = p.descend();
        let history = child.history();
        let fc_0 = |want_call: bool| {
            history
                .iter()
                .filter(|i| match i {
                    Item::ToolCall { call_id, .. } => want_call && call_id == "fc_0",
                    Item::ToolResult { call_id, .. } => !want_call && call_id == "fc_0",
                    _ => false,
                })
                .count()
        };
        assert_eq!(fc_0(true), 1, "{history:?}");
        assert_eq!(fc_0(false), 1, "{history:?}");
        assert!(
            history.iter().any(
                |i| matches!(i, Item::ToolResult { call_id, content } if call_id == "fc_0"
                    && content == &text("found"))
            ),
            "{history:?}"
        );
    }

    #[test]
    fn transfer_call_to_next_is_the_handoff_call_id() {
        let events = handoff_fixture();
        let input = AgentInput::new().with_resolution("d1", ToolResolution::Approved);
        let p = ResumePlan::from_events(&events, &project(&events), "triage", &input, 8).unwrap();
        assert_eq!(p.transfer_call_to_next(), Some("t1"));
        assert_eq!(p.descend().transfer_call_to_next(), None);

        let events = three_level();
        let p = plan(&events, &input).unwrap();
        assert_eq!(p.transfer_call_to_next(), Some("t1"));
        assert_eq!(p.descend().transfer_call_to_next(), Some("t2"));
        let leaf = p.descend().descend();
        assert_eq!(leaf.expected_agent(), "leaf");
        assert!(
            matches!(leaf.history().last(), Some(Item::ToolCall { call_id, .. }) if call_id == "c1")
        );
    }

    #[test]
    fn resume_plan_debug_hides_args() {
        let p = plan(&two_deferrals(), &approve_both()).unwrap();
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("amount"), "{dbg}");
        assert!(dbg.contains("c1"), "{dbg}");
    }
}
