//! Map Amazon Bedrock SDK errors to [`paigasus_helikon_core::ModelError`].
//!
//! Per ADR-10 (*No silent auto-retry inside the loop*), the runner never
//! retries — retries are an application-layer concern via `RetryingModel`.
//!
//! One send fallback exists, and it is not a retry in the ADR-10 sense:
//! when an Anthropic model rejects a forced tool choice before the stream
//! starts, the provider sends one native request, which is a different request,
//! on the structured-output path. Four facts make this different from a retry.
//! The fallback sends a *different* request, not the same request again.
//! It occurs at most once for each request. It is logged. It never follows
//! a transient error (throttling, a 5xx error or a transport error).
//!
//! The match on the error text is a weak contract. If Bedrock changes the
//! text, the result is the same `ValidationException` as before this
//! fallback existed. The static model-id rule still sends the known models
//! on the native path from the first request.
//!
//! The `classify` function is a pure, unit-testable classifier that operates
//! on string error codes and optional HTTP status codes, mirroring the style
//! of the Anthropic provider's `map_error_type`. `map_sdk_error` wraps it
//! by extracting the code/status from the real [`SdkError`] variants.

use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_runtime_api::http::Headers;
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

/// Returns `true` when a Bedrock error says that the model rejects a forced
/// tool choice (`toolChoice` `any` or `tool`).
///
/// The match is on the error code and on two exact texts: the text from the
/// model layer and the text from the Bedrock layer.
pub(crate) fn is_forced_choice_rejection(code: Option<&str>, message: &str) -> bool {
    code == Some("ValidationException")
        && (message.contains(r#"type "tool" and "any" are not supported"#)
            || message.contains("doesn't support the toolConfig.toolChoice."))
}

/// A failed send, with the flag that the fallback reads.
#[derive(Debug)]
pub(crate) struct SendFailure {
    /// `true` when the service rejected a forced tool choice.
    pub(crate) forced_choice_rejected: bool,
    /// The mapped error that goes to the caller when no fallback occurs.
    pub(crate) error: ModelError,
}

/// Parse the `Retry-After` header (integer seconds) from Bedrock response headers into milliseconds.
///
/// Returns `None` when the header is absent or its value is not a non-negative integer
/// (HTTP-date retry-after values are not supported — `None` is the safe fallback).
pub(crate) fn parse_retry_after_ms(headers: &Headers) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1000))
}

/// Map a Bedrock SDK [`SdkError`] to a [`ModelError`].
///
/// For `ServiceError`, the modeled error code and message are extracted via
/// [`ProvideErrorMetadata`] and delegated to [`classify`]. Transport-level
/// failures (`DispatchFailure`, `TimeoutError`, `ConstructionFailure`,
/// `ResponseError`) map to [`ModelError::Transport`].
///
/// This generic variant does **not** inspect HTTP headers (the response type `R`
/// may not be an HTTP response — e.g. the event-stream `recv()` path produces
/// `SdkError<_, RawMessage>`). Call [`map_sdk_http_error`] when `R` is known to
/// be [`aws_smithy_runtime_api::http::Response`] and you want `Retry-After`
/// header extraction.
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

/// Map a Bedrock SDK [`SdkError`] whose response type is the concrete HTTP
/// response to a [`ModelError`], extracting `Retry-After` header and HTTP
/// status from the raw response.
///
/// This is the preferred variant for the initial `fluent.send()` call on a
/// Bedrock operation, where the SDK returns
/// `SdkError<E, aws_smithy_runtime_api::client::orchestrator::HttpResponse>`.
pub(crate) fn map_sdk_http_error<E>(
    err: SdkError<E, aws_smithy_runtime_api::http::Response>,
) -> ModelError
where
    E: ProvideErrorMetadata + std::fmt::Debug,
{
    match &err {
        SdkError::ServiceError(svc) => {
            let code = svc.err().code();
            let message = svc.err().message().unwrap_or("");
            let raw = svc.raw();
            let status = Some(raw.status().as_u16());
            let retry_after_ms = parse_retry_after_ms(raw.headers());
            classify(code, status, message, retry_after_ms)
        }
        SdkError::DispatchFailure(_)
        | SdkError::TimeoutError(_)
        | SdkError::ConstructionFailure(_)
        | SdkError::ResponseError(_) => ModelError::Transport(format!("{err:?}")),
        // Non-exhaustive: treat any future variant as transport.
        _ => ModelError::Transport(format!("{err:?}")),
    }
}

/// Map a failed send to a [`SendFailure`].
///
/// The raw code and message of a `ServiceError` are read before the error is
/// mapped by [`map_sdk_http_error`]. All other variants are not a rejection
/// of a forced tool choice.
pub(crate) fn send_failure<E>(
    err: SdkError<E, aws_smithy_runtime_api::http::Response>,
) -> SendFailure
where
    E: ProvideErrorMetadata + std::fmt::Debug,
{
    let forced_choice_rejected = match &err {
        SdkError::ServiceError(svc) => {
            is_forced_choice_rejection(svc.err().code(), svc.err().message().unwrap_or(""))
        }
        _ => false,
    };
    SendFailure {
        forced_choice_rejected,
        error: map_sdk_http_error(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_retry_after_ms tests ────────────────────────────────────────────

    fn make_headers(pairs: &[(&str, &str)]) -> Headers {
        let mut h = Headers::new();
        for (k, v) in pairs {
            h.insert(k.to_string(), v.to_string());
        }
        h
    }

    #[test]
    fn parse_retry_after_integer_seconds_to_ms() {
        let h = make_headers(&[("retry-after", "30")]);
        assert_eq!(parse_retry_after_ms(&h), Some(30_000));
    }

    #[test]
    fn parse_retry_after_missing_returns_none() {
        let h = make_headers(&[]);
        assert_eq!(parse_retry_after_ms(&h), None);
    }

    #[test]
    fn parse_retry_after_http_date_returns_none() {
        // HTTP-date format is not supported; return None rather than panic.
        let h = make_headers(&[("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT")]);
        assert_eq!(parse_retry_after_ms(&h), None);
    }

    #[test]
    fn parse_retry_after_non_numeric_returns_none() {
        let h = make_headers(&[("retry-after", "soon")]);
        assert_eq!(parse_retry_after_ms(&h), None);
    }

    // ── classify tests ────────────────────────────────────────────────────────

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

    // ── forced-choice rejection classifier (SMA-839) ──────────────────────────

    const MODEL_LAYER: &str = "The model returned the following errors: tool_choice: type \"tool\" and \"any\" are not supported for this model.";
    const BEDROCK_LAYER: &str = "This model doesn't support the toolConfig.toolChoice.any field. Remove toolConfig.toolChoice.any and try again.";

    #[test]
    fn forced_choice_rejection_matches_both_patterns() {
        assert!(is_forced_choice_rejection(
            Some("ValidationException"),
            MODEL_LAYER
        ));
        assert!(is_forced_choice_rejection(
            Some("ValidationException"),
            BEDROCK_LAYER
        ));
    }

    #[test]
    fn forced_choice_rejection_ignores_near_misses() {
        for msg in [
            "Input is too long for requested model.",
            "output_config: Extra inputs are not permitted",
            "This model doesn't support the outputConfig field.",
            "The toolConfig field must be defined when using toolUse and toolResult content blocks.",
        ] {
            assert!(
                !is_forced_choice_rejection(Some("ValidationException"), msg),
                "{msg}"
            );
        }
        assert!(!is_forced_choice_rejection(
            Some("ThrottlingException"),
            MODEL_LAYER
        ));
        assert!(!is_forced_choice_rejection(None, MODEL_LAYER));
    }

    fn service_error(
        code: &str,
        message: &str,
        status: u16,
    ) -> SdkError<aws_smithy_types::error::ErrorMetadata, aws_smithy_runtime_api::http::Response>
    {
        let meta = aws_smithy_types::error::ErrorMetadata::builder()
            .code(code)
            .message(message)
            .build();
        let raw = aws_smithy_runtime_api::http::Response::new(
            aws_smithy_runtime_api::http::StatusCode::try_from(status).expect("status"),
            aws_smithy_types::body::SdkBody::empty(),
        );
        SdkError::service_error(meta, raw)
    }

    #[test]
    fn send_failure_flags_a_forced_choice_rejection() {
        let f = send_failure(service_error("ValidationException", BEDROCK_LAYER, 400));
        assert!(f.forced_choice_rejected);
        assert!(matches!(f.error, ModelError::Other(_)));
    }

    #[test]
    fn send_failure_keeps_other_errors_unflagged() {
        let f = send_failure(service_error("ThrottlingException", "slow down", 429));
        assert!(!f.forced_choice_rejected);
        assert!(matches!(f.error, ModelError::RateLimited { .. }));
        let f = send_failure(service_error(
            "ValidationException",
            "Input is too long for requested model.",
            400,
        ));
        assert!(!f.forced_choice_rejected);
        assert!(matches!(f.error, ModelError::ContextLengthExceeded));
    }
}
