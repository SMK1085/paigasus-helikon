//! Map Anthropic HTTP and in-stream errors to [`paigasus_helikon_core::ModelError`].
//!
//! Per ADR-10 ("no silent auto-retry in the loop"), the runner never
//! retries; the application configures retries by wrapping the model in
//! `RetryingModel` (with a `RetryPolicy`) from
//! `paigasus-helikon-runtime-tokio`. The mapping, first match wins:
//!
//! - `overloaded_error` gives `Unavailable`. `rate_limit_error` gives
//!   `RateLimited`. So does a 429 with `billing_error`.
//! - `invalid_request_error` with `prompt is too long` gives
//!   `ContextLengthExceeded`.
//! - HTTP 400, 401, 403, 404 and 413 give `Rejected` with the status, the
//!   `error.type` (empty when the body has none) and the message. The runtime
//!   does not retry `Rejected`.
//! - `authentication_error` and `permission_error` on the in-stream path (no
//!   status) give `Refused`.
//! - HTTP 500 to 504 and 529 give `Unavailable`. Any other status gives
//!   `Other`. With no status, the result is `Transport`.
//!
//! The same helper is invoked from both the HTTP-response path and the
//! in-stream `error` SSE event path so behavior is consistent.

use paigasus_helikon_core::ModelError;

/// `error.type` (with optional HTTP status) → `ModelError`.
///
/// Stream path passes `status: None` and `retry_after_ms: None`. HTTP path
/// supplies both.
pub(crate) fn map_error_type(
    status: Option<u16>,
    error_type: &str,
    message: &str,
    retry_after_ms: Option<u64>,
) -> ModelError {
    match (status, error_type) {
        (_, "overloaded_error") => ModelError::Unavailable,
        (_, "rate_limit_error") => ModelError::RateLimited { retry_after_ms },
        (Some(429), "billing_error") => ModelError::RateLimited { retry_after_ms },
        (_, "invalid_request_error") if message.contains("prompt is too long") => {
            ModelError::ContextLengthExceeded
        }
        (Some(status @ (400 | 401 | 403 | 404 | 413)), _) => ModelError::Rejected {
            status,
            error_type: error_type.to_owned(),
            message: message.to_owned(),
        },
        (None, "authentication_error" | "permission_error") => ModelError::Refused {
            reason: message.to_owned(),
        },
        (Some(500..=504 | 529), _) => ModelError::Unavailable,
        (Some(_), _) => ModelError::Other(anyhow::anyhow!("anthropic {error_type}: {message}")),
        (None, _) => ModelError::Transport(message.to_owned()),
    }
}

/// Parse the `retry-after` header (seconds, integer) into milliseconds.
pub(crate) fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_overloaded_maps_to_unavailable() {
        assert!(matches!(
            map_error_type(Some(529), "overloaded_error", "busy", None),
            ModelError::Unavailable,
        ));
    }

    #[test]
    fn stream_overloaded_maps_to_unavailable_not_transport() {
        assert!(matches!(
            map_error_type(None, "overloaded_error", "busy", None),
            ModelError::Unavailable,
        ));
    }

    #[test]
    fn http_429_maps_to_rate_limited_with_retry_after() {
        match map_error_type(Some(429), "rate_limit_error", "slow", Some(5000)) {
            ModelError::RateLimited { retry_after_ms } => {
                assert_eq!(retry_after_ms, Some(5000));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    fn assert_rejected(got: ModelError, want_status: u16, want_type: &str, want_message: &str) {
        match got {
            ModelError::Rejected {
                status,
                error_type,
                message,
            } => {
                assert_eq!(status, want_status);
                assert_eq!(error_type, want_type);
                assert_eq!(message, want_message);
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn http_400_maps_to_rejected() {
        assert_rejected(
            map_error_type(Some(400), "invalid_request_error", "missing field", None),
            400,
            "invalid_request_error",
            "missing field",
        );
    }

    #[test]
    fn http_401_maps_to_rejected() {
        assert_rejected(
            map_error_type(Some(401), "authentication_error", "bad key", None),
            401,
            "authentication_error",
            "bad key",
        );
    }

    #[test]
    fn http_403_maps_to_rejected() {
        assert_rejected(
            map_error_type(Some(403), "permission_error", "no access", None),
            403,
            "permission_error",
            "no access",
        );
    }

    #[test]
    fn http_404_maps_to_rejected() {
        assert_rejected(
            map_error_type(Some(404), "not_found_error", "no model", None),
            404,
            "not_found_error",
            "no model",
        );
    }

    #[test]
    fn http_413_maps_to_rejected() {
        assert_rejected(
            map_error_type(Some(413), "request_too_large", "too big", None),
            413,
            "request_too_large",
            "too big",
        );
    }

    #[test]
    fn http_401_without_error_body_maps_to_rejected() {
        assert_rejected(
            map_error_type(Some(401), "", "<html>denied</html>", None),
            401,
            "",
            "<html>denied</html>",
        );
    }

    #[test]
    fn http_429_billing_error_maps_to_rate_limited_with_retry_after() {
        match map_error_type(Some(429), "billing_error", "budget", Some(3000)) {
            ModelError::RateLimited { retry_after_ms } => {
                assert_eq!(retry_after_ms, Some(3000));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn stream_auth_error_stays_refused() {
        match map_error_type(None, "authentication_error", "bad key", None) {
            ModelError::Refused { reason } => assert_eq!(reason, "bad key"),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[test]
    fn http_422_maps_to_other() {
        assert!(matches!(
            map_error_type(Some(422), "invalid_request_error", "x", None),
            ModelError::Other(_),
        ));
    }

    /// A 402, 409 or 422 with an auth error type is `Other` now. It was
    /// `Refused` before the `Rejected` change (ruling F6).
    #[test]
    fn http_402_409_422_with_auth_error_type_map_to_other() {
        for status in [402, 409, 422] {
            for error_type in ["authentication_error", "permission_error"] {
                assert!(
                    matches!(
                        map_error_type(Some(status), error_type, "x", None),
                        ModelError::Other(_),
                    ),
                    "status {status}, type {error_type}",
                );
            }
        }
    }

    #[test]
    fn http_504_timeout_error_maps_to_unavailable() {
        assert!(matches!(
            map_error_type(Some(504), "timeout_error", "slow", None),
            ModelError::Unavailable,
        ));
    }

    #[test]
    fn prompt_too_long_maps_to_context_length_exceeded() {
        assert!(matches!(
            map_error_type(
                Some(400),
                "invalid_request_error",
                "prompt is too long: 200k",
                None
            ),
            ModelError::ContextLengthExceeded,
        ));
    }

    #[test]
    fn http_500_falls_through_to_unavailable() {
        assert!(matches!(
            map_error_type(Some(500), "api_error", "internal", None),
            ModelError::Unavailable,
        ));
    }

    #[test]
    fn stream_unknown_type_falls_to_transport() {
        match map_error_type(None, "mystery_error", "boom", None) {
            ModelError::Transport(s) => assert_eq!(s, "boom"),
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[test]
    fn parse_retry_after_handles_integer_seconds() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&h), Some(3000));
    }

    #[test]
    fn parse_retry_after_missing_returns_none() {
        let h = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after_ms(&h), None);
    }
}
