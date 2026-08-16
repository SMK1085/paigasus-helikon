//! Map LiteLLM HTTP errors onto core [`ModelError`] variants.
//!
//! The envelope is `{"error": {"message", "type", "param", "code"}}`, but two
//! of those fields do not mean what the OpenAI-shaped name suggests, measured
//! against LiteLLM 1.97.0:
//!
//! - **`code` is the HTTP status restated as a string** (`"400"`, `"429"`,
//!   `"500"`) — never a semantic identifier. Matching it against
//!   `"context_length_exceeded"` would never fire.
//! - **`type` is frequently `null`, and sometimes the literal string
//!   `"None"`.**
//!
//! The dependable signal is the LiteLLM exception class name, which is
//! prefixed onto `message` (`"litellm.ContextWindowExceededError: …"`).

use paigasus_helikon_core::ModelError;

/// Treat LiteLLM's stringified `"None"` as absent.
pub(crate) fn normalise_type(raw: Option<&str>) -> Option<&str> {
    raw.filter(|t| !t.is_empty() && *t != "None" && *t != "null")
}

/// Does this message indicate a context-window overflow?
fn is_context_overflow(message: &str) -> bool {
    if message.contains("ContextWindowExceededError") {
        return true;
    }
    let lc = message.to_ascii_lowercase();
    lc.contains("context_window_exceeded")
        || lc.contains("context_length_exceeded")
        || (lc.contains("maximum context length") || lc.contains("context window"))
}

/// Does this error indicate budget exhaustion rather than throttling?
fn is_budget_exhaustion(err_type: Option<&str>, message: &str) -> bool {
    let t = err_type.unwrap_or("").to_ascii_lowercase();
    if t.contains("budget") {
        return true;
    }
    let lc = message.to_ascii_lowercase();
    lc.contains("budgetexceeded") || lc.contains("budget has been exceeded")
}

/// Classify a LiteLLM error response.
///
/// Rules are evaluated top-down; the context-overflow check is deliberately
/// **first** and not gated on status, because its measured status is 400 and
/// keying it off a status would collide with every other 400.
pub(crate) fn classify(
    status: u16,
    code: Option<&str>,
    err_type: Option<&str>,
    message: &str,
    retry_after_ms: Option<u64>,
) -> ModelError {
    let err_type = normalise_type(err_type);

    if is_context_overflow(message) {
        return ModelError::ContextLengthExceeded;
    }

    match status {
        429 if is_budget_exhaustion(err_type, message) => ModelError::Refused {
            reason: message.to_owned(),
        },
        429 => ModelError::RateLimited { retry_after_ms },
        500 | 502 | 503 | 504 => ModelError::Unavailable,
        401 | 403 => ModelError::Refused {
            reason: message.to_owned(),
        },
        _ if matches!(
            err_type,
            Some("content_policy_violation") | Some("no_db_connection")
        ) =>
        {
            ModelError::Refused {
                reason: message.to_owned(),
            }
        }
        _ => ModelError::Other(anyhow::anyhow!(
            "litellm http {status}{}: {message}",
            code.map(|c| format!(" (code {c})")).unwrap_or_default()
        )),
    }
}

/// Parse `Retry-After` as whole seconds.
///
/// The HTTP-date form yields `None` — `RateLimited::retry_after_ms` is already
/// `Option`, so callers must handle absence regardless.
pub(crate) fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|s| s.saturating_mul(1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_string_and_null_types_both_normalise_to_none() {
        assert_eq!(normalise_type(Some("None")), None);
        assert_eq!(normalise_type(None), None);
        assert_eq!(
            normalise_type(Some("throttling_error")),
            Some("throttling_error")
        );
    }

    #[test]
    fn context_window_exceeded_is_detected_by_class_name() {
        // Captured from LiteLLM 1.97.0: status 400, code "400", type null.
        let m = "litellm.ContextWindowExceededError: litellm.BadRequestError: \
                 this is a mock context window exceeded error";
        assert!(matches!(
            classify(400, Some("400"), None, m, None),
            ModelError::ContextLengthExceeded
        ));
    }

    #[test]
    fn context_length_exceeded_prose_fallback_still_works() {
        let m = "This model's maximum context length is 8192 tokens";
        assert!(matches!(
            classify(400, Some("400"), None, m, None),
            ModelError::ContextLengthExceeded
        ));
    }

    #[test]
    fn context_overflow_check_is_not_gated_on_status() {
        // Not a realistic wire shape — the proxy was never observed to send
        // this class name on a 500. This test exists purely to pin the
        // ordering invariant: the context-overflow check runs before the
        // status match, so it must fire even on a status (500) that the
        // match would otherwise claim for `Unavailable`. A regression that
        // moved the check inside the match as `400 if is_context_overflow(..)`
        // would pass every other test here unchanged, since 400 is the only
        // status exercised elsewhere.
        let m = "litellm.ContextWindowExceededError: litellm.BadRequestError: \
                 this is a mock context window exceeded error";
        assert!(matches!(
            classify(500, Some("500"), None, m, None),
            ModelError::ContextLengthExceeded
        ));
    }

    #[test]
    fn rate_limit_maps_with_retry_after() {
        let m = "litellm.RateLimitError: this is a mock rate limit error";
        match classify(429, Some("429"), Some("throttling_error"), m, Some(1500)) {
            ModelError::RateLimited { retry_after_ms } => assert_eq!(retry_after_ms, Some(1500)),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn budget_exhaustion_429_is_refused_not_rate_limited() {
        // A retry loop would burn its whole budget against a limit that will
        // never clear.
        let m = "litellm.BudgetExceededError: Budget has been exceeded for key";
        assert!(matches!(
            classify(429, Some("429"), Some("budget_exceeded"), m, Some(1000)),
            ModelError::Refused { .. }
        ));
    }

    #[test]
    fn five_hundred_is_unavailable_not_other() {
        // `Other` is not retryable under runtime-tokio's RetryPolicy, and 500
        // is the single most common transient proxy failure.
        let m = "litellm.InternalServerError: this is a mock internal server error";
        for status in [500, 502, 503, 504] {
            assert!(
                matches!(
                    classify(status, Some("500"), None, m, None),
                    ModelError::Unavailable
                ),
                "status {status} must map to Unavailable"
            );
        }
    }

    #[test]
    fn auth_failures_are_refused() {
        assert!(matches!(
            classify(401, Some("401"), None, "invalid key", None),
            ModelError::Refused { .. }
        ));
        assert!(matches!(
            classify(403, Some("403"), None, "forbidden", None),
            ModelError::Refused { .. }
        ));
    }

    #[test]
    fn db_less_bad_key_400_is_refused_not_other() {
        // Measured: a wrong virtual key on a DB-less deployment returns 400
        // with type `no_db_connection`, not 401.
        assert!(matches!(
            classify(
                400,
                Some("400"),
                Some("no_db_connection"),
                "No connected db.",
                None
            ),
            ModelError::Refused { .. }
        ));
    }

    #[test]
    fn content_policy_is_refused() {
        assert!(matches!(
            classify(
                400,
                Some("400"),
                Some("content_policy_violation"),
                "blocked",
                None
            ),
            ModelError::Refused { .. }
        ));
    }

    #[test]
    fn unknown_model_400_falls_through_to_other() {
        let m = "/chat/completions: Invalid model name passed in model=does-not-exist";
        match classify(400, Some("400"), Some("None"), m, None) {
            ModelError::Other(e) => assert!(e.to_string().contains("does-not-exist")),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn retry_after_seconds_header_parses() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("retry-after", "3".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&h), Some(3000));
    }

    #[test]
    fn retry_after_http_date_yields_none() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(parse_retry_after_ms(&h), None);
    }

    #[test]
    fn absent_retry_after_yields_none() {
        assert_eq!(
            parse_retry_after_ms(&reqwest::header::HeaderMap::new()),
            None
        );
    }
}
