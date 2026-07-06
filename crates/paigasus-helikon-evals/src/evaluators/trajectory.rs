//! Tool-use trajectory evaluator: compares the observed tool-call
//! sequence against `EvalCase::expected_tools`.

use async_trait::async_trait;
use paigasus_helikon_core::{AgentEvent, Item};

use crate::{CaseOutcome, EvalCase, EvalError, Evaluator, Score};

/// Compares the observed tool-call sequence in a run's events against
/// `EvalCase::expected_tools`.
///
/// Skips (rather than fails) cases without `expected_tools` set — this
/// evaluator only applies to cases that assert a tool-use contract.
/// Handoff tool calls (name starting with `transfer_to_`) are filtered
/// out of the observed sequence by default; use
/// [`ToolUseTrajectory::include_handoffs`] to keep them.
pub struct ToolUseTrajectory {
    mode: Mode,
    include_handoffs: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Exact,
    InOrder,
}

impl ToolUseTrajectory {
    /// The observed sequence must equal `expected_tools` exactly
    /// (position-for-position; length mismatches count as misses).
    pub fn exact() -> Self {
        Self {
            mode: Mode::Exact,
            include_handoffs: false,
        }
    }

    /// `expected_tools` must appear as an in-order subsequence of the
    /// observed sequence; extra tool calls between expected ones are
    /// allowed.
    pub fn in_order() -> Self {
        Self {
            mode: Mode::InOrder,
            include_handoffs: false,
        }
    }

    /// Keep `transfer_to_*` handoff tool calls in the observed sequence
    /// (filtered out by default).
    #[must_use]
    pub fn include_handoffs(mut self) -> Self {
        self.include_handoffs = true;
        self
    }
}

#[async_trait]
impl Evaluator for ToolUseTrajectory {
    fn name(&self) -> &str {
        "tool_trajectory"
    }

    async fn evaluate(&self, case: &EvalCase, outcome: &CaseOutcome) -> Result<Score, EvalError> {
        let Some(expected) = &case.expected_tools else {
            return Ok(Score::skipped("no `expected_tools` on case"));
        };

        let actual: Vec<String> = outcome
            .events
            .iter()
            .filter_map(|ev| match ev {
                AgentEvent::ToolCallItem {
                    item: Item::ToolCall { name, .. },
                } => Some(name.clone()),
                _ => None,
            })
            .filter(|n| self.include_handoffs || !n.starts_with("transfer_to_"))
            .collect();

        let (matched, denom) = match self.mode {
            Mode::Exact => {
                let matched = expected.iter().zip(&actual).filter(|(e, a)| e == a).count();
                (matched, expected.len().max(actual.len()))
            }
            Mode::InOrder => {
                let mut it = actual.iter();
                let matched = expected
                    .iter()
                    .filter(|e| it.by_ref().any(|a| a == *e))
                    .count();
                (matched, expected.len())
            }
        };

        let value = if denom == 0 {
            1.0
        } else {
            matched as f64 / denom as f64
        };

        if (value - 1.0).abs() < f64::EPSILON {
            Ok(Score::passed(1.0))
        } else {
            Ok(Score::failed(
                value,
                format!("expected {expected:?}, observed {actual:?}"),
            ))
        }
    }
}
