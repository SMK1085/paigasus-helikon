//! Wire-format payload types exchanged between the Temporal workflow and its activities.

use paigasus_helikon_core::{AgentEvent, ContentPart, Item, ModelTurn, TokenUsage};
use serde::{Deserialize, Serialize};

use crate::error::ErrorKindPayload;

/// Driver configuration for the Temporal runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverConfig {
    /// Maximum number of turns the agent may execute before hitting the budget.
    pub max_turns: u32,
    /// Limit on concurrent tool calls; None = no limit.
    pub parallel_tool_call_limit: Option<usize>,
}

/// Input envelope for the Temporal workflow.
///
/// Seeds the durable run with the agent name, conversation history,
/// configuration, and timeout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowInput {
    /// Agent name to invoke.
    pub agent_name: String,
    /// Session snapshot plus new-turn messages.
    ///
    /// The workflow seeds the system message from the render_instructions
    /// activity result; this field contains user messages and prior turn
    /// items but NOT a system item.
    pub conversation: Vec<Item>,
    /// Runtime configuration (turn budget, tool call limits).
    pub config: DriverConfig,
    /// Run timeout as milliseconds; None = no deadline.
    pub timeout_ms: Option<u64>,
}

/// A reassembled model turn, wrapped for Temporal serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTurnResult(
    /// The underlying [`ModelTurn`] produced by the model.
    pub ModelTurn,
);

/// The final successful output of an agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalOutputPayload {
    /// Content items the agent produced (text, tool calls, etc.).
    pub content: Vec<ContentPart>,
    /// Cumulative token usage for the run.
    pub usage: TokenUsage,
}

/// The terminal status of a durable run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunStatusPayload {
    /// Run succeeded with a final output.
    Completed(FinalOutputPayload),
    /// Run failed with an agent error.
    AgentFailed(ErrorKindPayload),
    /// Run was externally cancelled.
    Cancelled,
    /// Run exceeded its timeout deadline.
    TimedOut,
}

/// The complete outcome of a durable agent run.
///
/// Combines the terminal status, event stream history, and cumulative usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableRunOutcome {
    /// The terminal run status.
    pub status: RunStatusPayload,
    /// All agent events emitted during the run.
    pub events: Vec<AgentEvent>,
    /// Cumulative token usage across the entire run.
    pub usage: TokenUsage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_durable_run_outcome_completed_roundtrip() {
        let outcome = DurableRunOutcome {
            status: RunStatusPayload::Completed(FinalOutputPayload {
                content: vec![ContentPart::Text {
                    text: "Hello".to_string(),
                }],
                usage: TokenUsage::default(),
            }),
            events: vec![],
            usage: TokenUsage::default(),
        };

        let json = serde_json::to_string(&outcome).expect("serialize");
        let deserialized: DurableRunOutcome = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            serde_json::to_value(&deserialized).unwrap()
        );
    }

    #[test]
    fn test_durable_run_outcome_cancelled_roundtrip() {
        let outcome = DurableRunOutcome {
            status: RunStatusPayload::Cancelled,
            events: vec![],
            usage: TokenUsage::default(),
        };

        let json = serde_json::to_string(&outcome).expect("serialize");
        let deserialized: DurableRunOutcome = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            serde_json::to_value(&deserialized).unwrap()
        );
    }

    #[test]
    fn test_durable_run_outcome_timedout_roundtrip() {
        let outcome = DurableRunOutcome {
            status: RunStatusPayload::TimedOut,
            events: vec![],
            usage: TokenUsage::default(),
        };

        let json = serde_json::to_string(&outcome).expect("serialize");
        let deserialized: DurableRunOutcome = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            serde_json::to_value(&deserialized).unwrap()
        );
    }

    #[test]
    fn test_durable_run_outcome_agent_failed_roundtrip() {
        let outcome = DurableRunOutcome {
            status: RunStatusPayload::AgentFailed(ErrorKindPayload::MaxTurnsExceeded(16)),
            events: vec![],
            usage: TokenUsage::default(),
        };

        let json = serde_json::to_string(&outcome).expect("serialize");
        let deserialized: DurableRunOutcome = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            serde_json::to_value(&deserialized).unwrap()
        );
    }
}
