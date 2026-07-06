//! Exact-match evaluator: string or structural-JSON equality against a
//! case's `expected` value.

use async_trait::async_trait;
use serde_json::Value;

use crate::{CaseOutcome, EvalCase, EvalError, Evaluator, Score};

/// Compares a run's final output against `EvalCase::expected`.
///
/// - `expected: None` → the case is skipped (not applicable).
/// - `expected` is a JSON string → the final output is compared to it as
///   trimmed text (optionally case-insensitively, via
///   [`ExactMatch::case_insensitive`]).
/// - `expected` is any other JSON value → the final output is parsed as
///   JSON and compared structurally.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExactMatch {
    case_insensitive: bool,
}

impl ExactMatch {
    /// A new exact-match evaluator with case-sensitive string comparison.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold string comparisons to lowercase before comparing.
    #[must_use]
    pub fn case_insensitive(mut self) -> Self {
        self.case_insensitive = true;
        self
    }
}

#[async_trait]
impl Evaluator for ExactMatch {
    fn name(&self) -> &str {
        "exact_match"
    }

    async fn evaluate(&self, case: &EvalCase, outcome: &CaseOutcome) -> Result<Score, EvalError> {
        let Some(expected) = &case.expected else {
            return Ok(Score::skipped("no `expected` on case"));
        };

        if let Value::String(expected_str) = expected {
            let actual = outcome.final_output.trim();
            let expected_trimmed = expected_str.trim();
            let matched = if self.case_insensitive {
                actual.to_lowercase() == expected_trimmed.to_lowercase()
            } else {
                actual == expected_trimmed
            };
            return Ok(if matched {
                Score::passed(1.0)
            } else {
                Score::failed(
                    0.0,
                    format!("expected {expected_trimmed:?}, got {actual:?}"),
                )
            });
        }

        let actual: Value = match serde_json::from_str(outcome.final_output.trim()) {
            Ok(v) => v,
            Err(e) => {
                return Ok(Score::failed(
                    0.0,
                    format!("final output is not valid JSON: {e}"),
                ));
            }
        };

        Ok(if &actual == expected {
            Score::passed(1.0)
        } else {
            Score::failed(0.0, format!("expected {expected}, got {actual}"))
        })
    }
}
