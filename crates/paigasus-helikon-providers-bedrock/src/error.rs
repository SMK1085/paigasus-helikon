//! Map Amazon Bedrock SDK errors to [`paigasus_helikon_core::ModelError`].
//!
//! Per ADR-10 (*No silent auto-retry inside the loop*), the runner never
//! retries — retries are an application-layer concern via `RetryingModel`.
//!
//! The `classify` function is a pure, unit-testable classifier that operates
//! on string error codes and optional HTTP status codes, mirroring the style
//! of the Anthropic provider's `map_error_type`. `map_sdk_error` wraps it
//! by extracting the code/status from the real [`SdkError`] variants.

use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use paigasus_helikon_core::ModelError;

/// Classify a Bedrock error into a [`ModelError`] from raw components.
///
/// This function is intentionally pure so it can be unit-tested without
/// constructing live `SdkError` values.
///
/// | `code`                       | Result                             |
/// |------------------------------|------------------------------------|
/// | `ThrottlingException`        | `RateLimited { retry_after_ms }`   |
/// | `ServiceUnavailableException`, `ModelNotReadyException`, `InternalServerException`, `ModelStreamErrorException`, `ModelTimeoutException` | `Unavailable` |
/// | `AccessDeniedException`      | `Refused { reason: message }`      |
/// | `ValidationException` + message contains "too long"/"maximum context" | `ContextLengthExceeded` |
/// | `ValidationException` (other)| `Other`                            |
/// | unknown / `None`             | `Other`                            |
// Used by model.rs (Task 12) — allow dead_code until that task lands.
#[allow(dead_code)]
pub(crate) fn classify(
    code: Option<&str>,
    _status: Option<u16>,
    message: &str,
    retry_after_ms: Option<u64>,
) -> ModelError {
    match code {
        Some("ThrottlingException") => ModelError::RateLimited { retry_after_ms },
        Some(
            "ServiceUnavailableException"
            | "ModelNotReadyException"
            | "InternalServerException"
            | "ModelStreamErrorException"
            | "ModelTimeoutException",
        ) => ModelError::Unavailable,
        Some("AccessDeniedException") => ModelError::Refused {
            reason: message.to_owned(),
        },
        Some("ValidationException") => {
            let msg_lower = message.to_ascii_lowercase();
            if msg_lower.contains("too long") || msg_lower.contains("maximum context") {
                ModelError::ContextLengthExceeded
            } else {
                ModelError::Other(anyhow::anyhow!("bedrock ValidationException: {message}"))
            }
        }
        _ => ModelError::Other(anyhow::anyhow!(
            "bedrock error {}: {message}",
            code.unwrap_or("unknown")
        )),
    }
}

/// Map a Bedrock SDK [`SdkError`] to a [`ModelError`].
///
/// For `ServiceError`, the modeled error code and message are extracted via
/// [`ProvideErrorMetadata`] and delegated to [`classify`]. Transport-level
/// failures (`DispatchFailure`, `TimeoutError`, `ConstructionFailure`,
/// `ResponseError`) map to [`ModelError::Transport`].
// Used by model.rs (Task 12) — allow dead_code until that task lands.
#[allow(dead_code)]
pub(crate) fn map_sdk_error<E, R>(err: SdkError<E, R>) -> ModelError
where
    E: ProvideErrorMetadata + std::fmt::Debug,
    R: std::fmt::Debug,
{
    match &err {
        SdkError::ServiceError(svc) => {
            let code = svc.err().code();
            let message = svc.err().message().unwrap_or("");
            classify(code, None, message, None)
        }
        SdkError::DispatchFailure(_)
        | SdkError::TimeoutError(_)
        | SdkError::ConstructionFailure(_)
        | SdkError::ResponseError(_) => ModelError::Transport(format!("{err:?}")),
        // Non-exhaustive: treat any future variant as transport.
        _ => ModelError::Transport(format!("{err:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttling_maps_to_rate_limited() {
        match classify(
            Some("ThrottlingException"),
            Some(429),
            "slow down",
            Some(2000),
        ) {
            ModelError::RateLimited { retry_after_ms } => {
                assert_eq!(retry_after_ms, Some(2000));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn throttling_without_retry_after() {
        assert!(matches!(
            classify(Some("ThrottlingException"), None, "slow", None),
            ModelError::RateLimited {
                retry_after_ms: None
            }
        ));
    }

    #[test]
    fn service_unavailable_maps_to_unavailable() {
        assert!(matches!(
            classify(Some("ServiceUnavailableException"), Some(503), "down", None),
            ModelError::Unavailable
        ));
    }

    #[test]
    fn model_not_ready_maps_to_unavailable() {
        assert!(matches!(
            classify(Some("ModelNotReadyException"), None, "not ready", None),
            ModelError::Unavailable
        ));
    }

    #[test]
    fn internal_server_exception_maps_to_unavailable() {
        assert!(matches!(
            classify(Some("InternalServerException"), Some(500), "boom", None),
            ModelError::Unavailable
        ));
    }

    #[test]
    fn model_stream_error_maps_to_unavailable() {
        assert!(matches!(
            classify(
                Some("ModelStreamErrorException"),
                None,
                "stream error",
                None
            ),
            ModelError::Unavailable
        ));
    }

    #[test]
    fn model_timeout_maps_to_unavailable() {
        assert!(matches!(
            classify(Some("ModelTimeoutException"), None, "timeout", None),
            ModelError::Unavailable
        ));
    }

    #[test]
    fn access_denied_maps_to_refused() {
        match classify(Some("AccessDeniedException"), Some(403), "no access", None) {
            ModelError::Refused { reason } => assert_eq!(reason, "no access"),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[test]
    fn validation_exception_too_long_maps_to_context_length() {
        assert!(matches!(
            classify(
                Some("ValidationException"),
                Some(400),
                "prompt is too long: 200k tokens",
                None
            ),
            ModelError::ContextLengthExceeded
        ));
    }

    #[test]
    fn validation_exception_maximum_context_maps_to_context_length() {
        assert!(matches!(
            classify(
                Some("ValidationException"),
                Some(400),
                "Input length exceeds maximum context length",
                None
            ),
            ModelError::ContextLengthExceeded
        ));
    }

    #[test]
    fn validation_exception_other_maps_to_other() {
        assert!(matches!(
            classify(
                Some("ValidationException"),
                Some(400),
                "missing field",
                None
            ),
            ModelError::Other(_)
        ));
    }

    #[test]
    fn unknown_code_maps_to_other() {
        assert!(matches!(
            classify(Some("UnknownFutureException"), None, "idk", None),
            ModelError::Other(_)
        ));
    }

    #[test]
    fn none_code_maps_to_other() {
        assert!(matches!(
            classify(None, None, "no code", None),
            ModelError::Other(_)
        ));
    }
}
