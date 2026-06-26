//! Map Google API HTTP errors onto core `ModelError` variants.

use paigasus_helikon_core::ModelError;

/// Classify a Google API error response into a core [`ModelError`].
///
/// `status` is the HTTP status; `status_field` is the JSON `error.status`
/// string (e.g. `RESOURCE_EXHAUSTED`); `message` is `error.message`.
pub(crate) fn classify(
    status: u16,
    status_field: Option<&str>,
    message: &str,
    retry_after_ms: Option<u64>,
) -> ModelError {
    match status {
        429 => ModelError::RateLimited { retry_after_ms },
        500 | 503 | 504 => ModelError::Unavailable,
        401 | 403 => ModelError::Refused {
            reason: message.to_owned(),
        },
        400 => {
            let lc = message.to_ascii_lowercase();
            if lc.contains("token count")
                || (lc.contains("maximum") && lc.contains("context"))
                || (lc.contains("exceeds") && lc.contains("token"))
            {
                ModelError::ContextLengthExceeded
            } else {
                ModelError::Other(anyhow::anyhow!(
                    "gemini {}: {message}",
                    status_field.unwrap_or("INVALID_ARGUMENT")
                ))
            }
        }
        _ => ModelError::Other(anyhow::anyhow!(
            "gemini http {status} {}: {message}",
            status_field.unwrap_or("")
        )),
    }
}

/// Parse an integer-seconds `Retry-After` header into milliseconds.
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
    fn rate_limited_429_carries_retry_after() {
        let e = classify(429, Some("RESOURCE_EXHAUSTED"), "quota", Some(7000));
        assert!(matches!(
            e,
            ModelError::RateLimited {
                retry_after_ms: Some(7000)
            }
        ));
    }

    #[test]
    fn unavailable_503_500_504() {
        for s in [503u16, 500, 504] {
            assert!(
                matches!(classify(s, None, "x", None), ModelError::Unavailable),
                "status {s}"
            );
        }
    }

    #[test]
    fn forbidden_and_unauthenticated_are_refused() {
        assert!(matches!(
            classify(403, Some("PERMISSION_DENIED"), "no", None),
            ModelError::Refused { .. }
        ));
        assert!(matches!(
            classify(401, Some("UNAUTHENTICATED"), "no", None),
            ModelError::Refused { .. }
        ));
    }

    #[test]
    fn context_overflow_400() {
        let e = classify(
            400,
            Some("INVALID_ARGUMENT"),
            "input token count exceeds the maximum",
            None,
        );
        assert!(matches!(e, ModelError::ContextLengthExceeded));
    }

    #[test]
    fn other_400_is_other() {
        assert!(matches!(
            classify(400, Some("INVALID_ARGUMENT"), "bad field", None),
            ModelError::Other(_)
        ));
    }

    #[test]
    fn retry_after_header_seconds_to_ms() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&h), Some(3000));
        assert_eq!(
            parse_retry_after_ms(&reqwest::header::HeaderMap::new()),
            None
        );
    }
}
