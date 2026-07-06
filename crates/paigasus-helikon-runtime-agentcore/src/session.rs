//! Session-id header handling for `POST /invocations`.
//!
//! AWS Bedrock AgentCore's HTTP-protocol contract lets the caller pin an invocation to
//! a specific session via an optional request header. Header name lookups are
//! case-insensitive per HTTP — [`axum::http::HeaderMap`] normalises header names to
//! lower case on both insert and lookup — so the AWS-documented mixed-case spelling
//! (`X-Amzn-Bedrock-AgentCore-Runtime-Session-Id`) and this module's lower-case
//! [`SESSION_HEADER`] constant refer to the exact same header.
//!
//! Absent the header, [`crate::invoke::invocations`] resolves a fresh, unshared
//! session (one microVM instance is, by AgentCore's execution model, already one
//! session) via the configured
//! [`SessionProvider`](paigasus_helikon_runtime_axum::SessionProvider). A present
//! header is validated here — 33 to 256 characters, inclusive — before it ever reaches
//! the session provider; two requests presenting the exact same valid id are then
//! routed to the same session by the provider's ordinary `Some(id)` path.

use axum::http::HeaderMap;

use crate::error::AgentCoreError;

/// The AgentCore session-id request header, lower-cased for
/// [`HeaderMap::get`]'s case-insensitive string lookup. See the [module docs](self).
pub(crate) const SESSION_HEADER: &str = "x-amzn-bedrock-agentcore-runtime-session-id";

/// Inclusive lower bound on a valid session id's length, per the AgentCore
/// HTTP-protocol contract.
const MIN_SESSION_ID_LEN: usize = 33;

/// Inclusive upper bound on a valid session id's length, per the AgentCore
/// HTTP-protocol contract.
const MAX_SESSION_ID_LEN: usize = 256;

/// Validate a session id's length (33..=256 bytes).
///
/// # Errors
///
/// [`AgentCoreError::BadRequest`] if `v` is shorter than [`MIN_SESSION_ID_LEN`] or
/// longer than [`MAX_SESSION_ID_LEN`] bytes.
pub(crate) fn validate_session_id(v: &str) -> Result<&str, AgentCoreError> {
    let len = v.len();
    if (MIN_SESSION_ID_LEN..=MAX_SESSION_ID_LEN).contains(&len) {
        Ok(v)
    } else {
        Err(AgentCoreError::BadRequest(format!(
            "{SESSION_HEADER} must be between {MIN_SESSION_ID_LEN} and {MAX_SESSION_ID_LEN} \
             characters, got {len}"
        )))
    }
}

/// Extract and validate the optional AgentCore session header from `headers`.
///
/// Returns `Ok(None)` when the header is absent, so the caller can fall back to a
/// fresh, ephemeral session. Returns [`AgentCoreError::BadRequest`] when the header is
/// present but is not valid UTF-8, or when it fails [`validate_session_id`].
pub(crate) fn extract_session_id(headers: &HeaderMap) -> Result<Option<&str>, AgentCoreError> {
    let Some(value) = headers.get(SESSION_HEADER) else {
        return Ok(None);
    };
    let s = value.to_str().map_err(|_| {
        AgentCoreError::BadRequest(format!("{SESSION_HEADER} header value is not valid UTF-8"))
    })?;
    validate_session_id(s).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_session_id_accepts_boundary_lengths() {
        assert!(validate_session_id(&"a".repeat(MIN_SESSION_ID_LEN)).is_ok());
        assert!(validate_session_id(&"a".repeat(MAX_SESSION_ID_LEN)).is_ok());
    }

    #[test]
    fn validate_session_id_rejects_out_of_range_lengths() {
        assert!(matches!(
            validate_session_id(&"a".repeat(MIN_SESSION_ID_LEN - 1)),
            Err(AgentCoreError::BadRequest(_))
        ));
        assert!(matches!(
            validate_session_id(&"a".repeat(MAX_SESSION_ID_LEN + 1)),
            Err(AgentCoreError::BadRequest(_))
        ));
    }

    #[test]
    fn extract_session_id_absent_is_none() {
        let headers = HeaderMap::new();
        assert_eq!(extract_session_id(&headers).unwrap(), None);
    }

    #[test]
    fn extract_session_id_present_and_valid() {
        let mut headers = HeaderMap::new();
        let id = "a".repeat(40);
        headers.insert(SESSION_HEADER, id.parse().unwrap());
        assert_eq!(extract_session_id(&headers).unwrap(), Some(id.as_str()));
    }

    #[test]
    fn extract_session_id_present_but_too_short_errors() {
        let mut headers = HeaderMap::new();
        headers.insert(SESSION_HEADER, "short".parse().unwrap());
        assert!(matches!(
            extract_session_id(&headers),
            Err(AgentCoreError::BadRequest(_))
        ));
    }

    /// Header lookups are case-insensitive per HTTP: a request presenting the
    /// AWS-documented mixed-case spelling must resolve through the same lower-case
    /// [`SESSION_HEADER`] constant.
    #[test]
    fn header_lookup_is_case_insensitive() {
        let mut headers = HeaderMap::new();
        let id = "a".repeat(40);
        headers.insert(
            "X-Amzn-Bedrock-AgentCore-Runtime-Session-Id"
                .parse::<axum::http::HeaderName>()
                .unwrap(),
            id.parse().unwrap(),
        );
        assert_eq!(extract_session_id(&headers).unwrap(), Some(id.as_str()));
    }
}
