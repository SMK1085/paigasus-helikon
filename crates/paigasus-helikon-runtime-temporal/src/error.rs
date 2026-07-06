//! Error types for the Temporal-backed durable runtime.

use serde::{Deserialize, Serialize};

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
