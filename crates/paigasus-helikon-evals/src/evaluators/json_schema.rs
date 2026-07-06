//! JSON Schema conformance evaluator: validates a run's final output
//! against a fixed schema.

use async_trait::async_trait;
use serde_json::Value;

use crate::{CaseOutcome, EvalCase, EvalError, Evaluator, Score};

/// Validates a run's final output against a JSON Schema, independent of
/// anything on the case itself (never skips).
pub struct JsonSchemaConformance {
    validator: jsonschema::Validator,
}

impl JsonSchemaConformance {
    /// Compile `schema`. Fails with [`EvalError::InvalidSchema`] if the
    /// schema itself is invalid.
    pub fn new(schema: Value) -> Result<Self, EvalError> {
        let validator = jsonschema::validator_for(&schema)
            .map_err(|e| EvalError::InvalidSchema(e.to_string()))?;
        Ok(Self { validator })
    }
}

#[async_trait]
impl Evaluator for JsonSchemaConformance {
    fn name(&self) -> &str {
        "json_schema"
    }

    async fn evaluate(&self, _case: &EvalCase, outcome: &CaseOutcome) -> Result<Score, EvalError> {
        let value: Value = match serde_json::from_str(outcome.final_output.trim()) {
            Ok(v) => v,
            Err(e) => {
                return Ok(Score::failed(
                    0.0,
                    format!("final output is not valid JSON: {e}"),
                ));
            }
        };

        let violations: Vec<String> = self
            .validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();

        Ok(if violations.is_empty() {
            Score::passed(1.0)
        } else {
            Score::failed(0.0, violations.join("; "))
        })
    }
}
