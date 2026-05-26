//! Map Anthropic HTTP and in-stream errors to [`paigasus_helikon_core::ModelError`].
//!
//! Per ADR-10 ("no silent auto-retry in the loop"), the runner never
//! retries; the application configures retries via
//! `RunConfig::retry_policy`. Auth failures (401/403) map to `Refused`;
//! 429 maps to `RateLimited`; 5xx and 529 map to `Unavailable`. The same
//! helper is invoked from both the HTTP-response path and the in-stream
//! `error` SSE event path so behavior is consistent.

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
        (_, "authentication_error") | (_, "permission_error") => ModelError::Refused {
            reason: message.to_owned(),
        },
        (_, "invalid_request_error") if message.contains("prompt is too long") => {
            ModelError::ContextLengthExceeded
        }
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

    #[test]
    fn auth_error_maps_to_refused() {
        match map_error_type(Some(401), "authentication_error", "bad key", None) {
            ModelError::Refused { reason } => assert_eq!(reason, "bad key"),
            other => panic!("expected Refused, got {other:?}"),
        }
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
    fn http_400_other_maps_to_other() {
        assert!(matches!(
            map_error_type(Some(400), "invalid_request_error", "missing field", None),
            ModelError::Other(_),
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
