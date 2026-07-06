//! Error types for the Temporal-backed durable runtime.

use serde::{Deserialize, Serialize};

use crate::payloads::{DurableRunOutcome, RunStatusPayload};

/// Map a total [`DurableRunOutcome`] onto the [`crate::error`]-external
/// [`paigasus_helikon_core::Runner`] boundary types.
///
/// This is the durable-runner mirror of `TokioRunner`'s terminal-outcome
/// handling: the workflow always returns a `DurableRunOutcome` (finalize on
/// every exit path), and this function projects its four terminal states onto
/// the runner's `Result<RunResult, RunError>`:
///
/// - [`RunStatusPayload::Completed`] → `Ok(RunResult)` whose `final_output` is
///   the concatenated [`paigasus_helikon_core::ContentPart::Text`] parts of the
///   final output (the `FinalOutput::as_text` convention), carrying the run's
///   events and cumulative usage.
/// - [`RunStatusPayload::AgentFailed`] → `Err(RunError::Agent(..))`, the typed
///   [`paigasus_helikon_core::AgentError`] reconstructed from the wire payload.
/// - [`RunStatusPayload::Cancelled`] → `Err(RunError::Cancelled)`.
/// - [`RunStatusPayload::TimedOut`] → `Err(RunError::Timeout)`.
pub(crate) fn outcome_to_run_result(
    outcome: DurableRunOutcome,
) -> Result<paigasus_helikon_core::RunResult, paigasus_helikon_core::RunError> {
    use paigasus_helikon_core::ContentPart;

    let DurableRunOutcome {
        status,
        events,
        usage,
    } = outcome;

    match status {
        RunStatusPayload::Completed(final_output) => {
            let final_text = final_output
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            // `RunResult` is `#[non_exhaustive]`, so it cannot be built with a
            // struct literal from outside `paigasus-helikon-core`; construct
            // via `Default` (which yields `RunResult<String>`) then assign.
            let mut result = paigasus_helikon_core::RunResult::default();
            result.final_output = final_text;
            result.events = events;
            result.usage = usage;
            Ok(result)
        }
        RunStatusPayload::AgentFailed(kind) => Err(paigasus_helikon_core::RunError::Agent(
            kind.into_agent_error(),
        )),
        RunStatusPayload::Cancelled => Err(paigasus_helikon_core::RunError::Cancelled),
        RunStatusPayload::TimedOut => Err(paigasus_helikon_core::RunError::Timeout),
    }
}

/// Serializable error payload for crossing Temporal boundaries.
///
/// `AgentError` contains non-serializable variants (particularly `Other` with
/// `anyhow::Error`), so this type provides a wire-safe projection that can
/// be serialized and deserialized through Temporal activities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorKindPayload {
    /// Agent exhausted its turn budget before reaching a final output.
    MaxTurnsExceeded(u32),
    /// Structured output validation failed even after one repair attempt.
    InvalidStructuredOutput {
        /// Human-readable schema/validation errors.
        schema_errors: Vec<String>,
        /// The raw terminal assistant text that failed validation.
        final_text: String,
    },
    /// A downstream model call failed.
    Model {
        /// Error message from the model.
        message: String,
    },
    /// Handoff target is not supported (not yet used in Temporal workflow).
    HandoffUnsupported {
        /// The handoff target that was not supported.
        target: String,
    },
    /// Catch-all for all other agent errors.
    Other {
        /// Error message.
        message: String,
    },
}

impl ErrorKindPayload {
    /// Lossy projection from [`paigasus_helikon_core::AgentError`].
    ///
    /// Converts the typed error surface into a serializable form for
    /// transmission through Temporal. Some variants (especially `Other` with
    /// `anyhow::Error`) degrade to string messages, as `anyhow::Error` is
    /// not `Serialize`.
    ///
    /// # Mapping
    ///
    /// - `MaxTurnsExceeded(n)` → `Self::MaxTurnsExceeded(n)`
    /// - `InvalidStructuredOutput { schema_errors, final_text }` → `Self::InvalidStructuredOutput { .. }`
    /// - `Model(e)` → `Self::Model { message: e.to_string() }`
    /// - Everything else (Tool, Session, Guardrail, HookDenied, etc.) → `Self::Other { message }`
    pub fn from_agent_error(e: &paigasus_helikon_core::AgentError) -> Self {
        match e {
            paigasus_helikon_core::AgentError::MaxTurnsExceeded(n) => Self::MaxTurnsExceeded(*n),
            paigasus_helikon_core::AgentError::InvalidStructuredOutput {
                schema_errors,
                final_text,
            } => Self::InvalidStructuredOutput {
                schema_errors: schema_errors.clone(),
                final_text: final_text.clone(),
            },
            paigasus_helikon_core::AgentError::Model(e) => Self::Model {
                message: e.to_string(),
            },
            _ => Self::Other {
                message: e.to_string(),
            },
        }
    }

    /// Reconstruction into the typed error surface the Runner returns.
    ///
    /// Converts this serializable form back into an [`paigasus_helikon_core::AgentError`]
    /// for downstream error handling. Lossy variants (like `Other`) reconstruct
    /// with the stored message.
    pub fn into_agent_error(self) -> paigasus_helikon_core::AgentError {
        match self {
            Self::MaxTurnsExceeded(n) => paigasus_helikon_core::AgentError::MaxTurnsExceeded(n),
            Self::InvalidStructuredOutput {
                schema_errors,
                final_text,
            } => paigasus_helikon_core::AgentError::InvalidStructuredOutput {
                schema_errors,
                final_text,
            },
            Self::Model { message } => {
                paigasus_helikon_core::AgentError::Other(anyhow::anyhow!(message))
            }
            Self::HandoffUnsupported { target } => paigasus_helikon_core::AgentError::Other(
                anyhow::anyhow!("handoff unsupported: {}", target),
            ),
            Self::Other { message } => {
                paigasus_helikon_core::AgentError::Other(anyhow::anyhow!(message))
            }
        }
    }
}

#[cfg(test)]
mod outcome_mapping_tests {
    use super::*;
    use crate::payloads::{DurableRunOutcome, FinalOutputPayload};
    use paigasus_helikon_core::{AgentError, ContentPart, RunError, TokenUsage};

    fn usage(input_tokens: u64, output_tokens: u64) -> TokenUsage {
        // `TokenUsage` is `#[non_exhaustive]`: build via `Default`, assign
        // the `pub` fields.
        let mut u = TokenUsage::default();
        u.input_tokens = input_tokens;
        u.output_tokens = output_tokens;
        u.total_tokens = input_tokens + output_tokens;
        u
    }

    #[test]
    fn completed_maps_to_ok_with_concatenated_text_and_usage() {
        let outcome = DurableRunOutcome {
            status: RunStatusPayload::Completed(FinalOutputPayload {
                content: vec![
                    ContentPart::Text {
                        text: "Hello, ".to_owned(),
                    },
                    ContentPart::Text {
                        text: "world".to_owned(),
                    },
                ],
                usage: usage(3, 5),
            }),
            events: vec![paigasus_helikon_core::AgentEvent::RunCompleted { usage: usage(3, 5) }],
            usage: usage(3, 5),
        };

        let result = outcome_to_run_result(outcome).expect("Completed maps to Ok");
        assert_eq!(result.final_output, "Hello, world");
        assert_eq!(result.usage.input_tokens, 3);
        assert_eq!(result.usage.output_tokens, 5);
        assert_eq!(result.events.len(), 1);
    }

    #[test]
    fn agent_failed_max_turns_maps_to_typed_run_error() {
        let outcome = DurableRunOutcome {
            status: RunStatusPayload::AgentFailed(ErrorKindPayload::MaxTurnsExceeded(4)),
            events: vec![],
            usage: TokenUsage::default(),
        };

        match outcome_to_run_result(outcome) {
            Err(RunError::Agent(AgentError::MaxTurnsExceeded(4))) => {}
            other => panic!("expected RunError::Agent(MaxTurnsExceeded(4)), got {other:?}"),
        }
    }

    #[test]
    fn cancelled_maps_to_run_error_cancelled() {
        let outcome = DurableRunOutcome {
            status: RunStatusPayload::Cancelled,
            events: vec![],
            usage: TokenUsage::default(),
        };
        match outcome_to_run_result(outcome) {
            Err(RunError::Cancelled) => {}
            other => panic!("expected RunError::Cancelled, got {other:?}"),
        }
    }

    #[test]
    fn timed_out_maps_to_run_error_timeout() {
        let outcome = DurableRunOutcome {
            status: RunStatusPayload::TimedOut,
            events: vec![],
            usage: TokenUsage::default(),
        };
        match outcome_to_run_result(outcome) {
            Err(RunError::Timeout) => {}
            other => panic!("expected RunError::Timeout, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_turns_exceeded_roundtrip() {
        let original = paigasus_helikon_core::AgentError::MaxTurnsExceeded(16);
        let payload = ErrorKindPayload::from_agent_error(&original);
        let reconstructed = payload.into_agent_error();

        match (&original, &reconstructed) {
            (
                paigasus_helikon_core::AgentError::MaxTurnsExceeded(n1),
                paigasus_helikon_core::AgentError::MaxTurnsExceeded(n2),
            ) => {
                assert_eq!(n1, n2);
            }
            _ => panic!("Mismatch after roundtrip"),
        }
    }

    #[test]
    fn test_invalid_structured_output_roundtrip() {
        let original = paigasus_helikon_core::AgentError::InvalidStructuredOutput {
            schema_errors: vec!["error1".to_string(), "error2".to_string()],
            final_text: "invalid json".to_string(),
        };
        let payload = ErrorKindPayload::from_agent_error(&original);
        let reconstructed = payload.into_agent_error();

        match (&original, &reconstructed) {
            (
                paigasus_helikon_core::AgentError::InvalidStructuredOutput {
                    schema_errors: se1,
                    final_text: ft1,
                },
                paigasus_helikon_core::AgentError::InvalidStructuredOutput {
                    schema_errors: se2,
                    final_text: ft2,
                },
            ) => {
                assert_eq!(se1, se2);
                assert_eq!(ft1, ft2);
            }
            _ => panic!("Mismatch after roundtrip"),
        }
    }

    #[test]
    fn test_model_error_degrades_to_message() {
        use paigasus_helikon_core::ModelError;

        let model_err = ModelError::Transport("connection lost".to_string());
        let original = paigasus_helikon_core::AgentError::Model(model_err);
        let payload = ErrorKindPayload::from_agent_error(&original);

        match &payload {
            ErrorKindPayload::Model { message } => {
                assert!(message.contains("connection lost"));
            }
            _ => panic!("Expected Model variant"),
        }

        let reconstructed = payload.into_agent_error();
        match reconstructed {
            paigasus_helikon_core::AgentError::Other(_) => {
                // Expected: ModelError became Other(anyhow::Error)
                // Verify message survived roundtrip
                assert!(reconstructed.to_string().contains("connection lost"));
            }
            _ => panic!("Expected Other variant after roundtrip"),
        }
    }

    #[test]
    fn test_other_error_roundtrip() {
        let original = paigasus_helikon_core::AgentError::Other(anyhow::anyhow!("test error"));
        let payload = ErrorKindPayload::from_agent_error(&original);

        match &payload {
            ErrorKindPayload::Other { message } => {
                assert_eq!(message, "test error");
            }
            _ => panic!("Expected Other variant"),
        }

        let reconstructed = payload.into_agent_error();
        match reconstructed {
            paigasus_helikon_core::AgentError::Other(_) => {
                // Expected
                // Verify message survived roundtrip
                assert_eq!(reconstructed.to_string(), "test error");
            }
            _ => panic!("Expected Other variant after roundtrip"),
        }
    }

    #[test]
    fn test_error_payload_serialization() {
        let payload = ErrorKindPayload::MaxTurnsExceeded(42);
        let json = serde_json::to_string(&payload).expect("serialize");
        let deserialized: ErrorKindPayload = serde_json::from_str(&json).expect("deserialize");

        match deserialized {
            ErrorKindPayload::MaxTurnsExceeded(n) => assert_eq!(n, 42),
            _ => panic!("Deserialized to wrong variant"),
        }
    }
}
