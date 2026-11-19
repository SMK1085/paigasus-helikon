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

    /// A copy of this call with other arguments (the redacted event copy).
    #[expect(dead_code, reason = "used from Task 4")]
    pub(crate) fn with_args(&self, args: serde_json::Value) -> Self {
        Self {
            args,
            ..self.clone()
        }
    }
}

/// Make a new deferral id.
#[cfg_attr(not(test), expect(dead_code, reason = "used from Task 4"))]
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
    #[error("the session history has no open tool call {call_id} for a deferral")]
    CallNotInHistory {
        /// The call id that the history lacks.
        call_id: String,
    },
    /// The resume input contains tool call or tool result items.
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

    /// Append calls to the slot.
    #[cfg_attr(not(test), expect(dead_code, reason = "used from Task 4"))]
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
}
