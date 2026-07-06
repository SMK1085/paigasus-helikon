//! The `Evaluator` trait and score types.

use async_trait::async_trait;
use paigasus_helikon_core::{AgentEvent, TokenUsage};

use crate::{EvalCase, EvalError};

/// What one case's agent run produced.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CaseOutcome {
    /// The run's final output text.
    pub final_output: String,
    /// The full event trajectory.
    pub events: Vec<AgentEvent>,
    /// Run-level token usage.
    pub usage: TokenUsage,
}

/// Pass/fail/skip classification of one score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreOutcome {
    /// The evaluator's criterion held.
    Passed,
    /// The criterion failed.
    Failed,
    /// The evaluator wasn't applicable to this case.
    Skipped,
}

/// One evaluator's verdict on one case.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Score {
    /// Score value in `[0, 1]`.
    pub value: f64,
    /// Pass/fail/skip classification.
    pub outcome: ScoreOutcome,
    /// Human-readable explanation (violations, diffs, skip reason).
    pub detail: Option<String>,
}

impl Score {
    /// A passing score.
    pub fn passed(value: f64) -> Self {
        Self {
            value,
            outcome: ScoreOutcome::Passed,
            detail: None,
        }
    }
    /// A failing score with an explanation.
    pub fn failed(value: f64, detail: impl Into<String>) -> Self {
        Self {
            value,
            outcome: ScoreOutcome::Failed,
            detail: Some(detail.into()),
        }
    }
    /// A skipped (not-applicable) score.
    pub fn skipped(reason: impl Into<String>) -> Self {
        Self {
            value: 0.0,
            outcome: ScoreOutcome::Skipped,
            detail: Some(reason.into()),
        }
    }
}

/// Scores one case's outcome. Implementations must be side-effect free.
#[async_trait]
pub trait Evaluator: Send + Sync {
    /// Stable evaluator name (used in reports and trace sinks).
    fn name(&self) -> &str;
    /// Score `outcome` for `case`.
    async fn evaluate(&self, case: &EvalCase, outcome: &CaseOutcome) -> Result<Score, EvalError>;
}
