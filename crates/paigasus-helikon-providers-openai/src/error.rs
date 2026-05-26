//! Map [`async_openai::error::OpenAIError`] into
//! [`paigasus_helikon_core::ModelError`].
//!
//! Per ADR-10 ("no silent auto-retry in the loop"), the loop never
//! retries on `ModelError`; the application configures retries via
//! `RunConfig::retry_policy`. Auth failures (401/403) map to
//! `Refused` (non-retryable, the correct semantic for bad credentials).
//! Generic 5xx maps to `Unavailable`.

use async_openai::error::{ApiErrorResponse, OpenAIError};
use paigasus_helikon_core::ModelError;

/// Translate an upstream [`OpenAIError`] into a [`ModelError`].
///
/// Mapping heuristics:
/// - `ApiError` with `code = "context_length_exceeded"` → `ContextLengthExceeded`.
/// - `ApiError` with `type = "content_filter"` → `Refused { reason: message }`.
/// - `ApiError` with HTTP 401 or 403 → `Refused { reason }` (bad credentials).
/// - `ApiError` with HTTP 429 → `RateLimited { retry_after_ms: None }`.
///   async-openai 0.40 does not parse a `Retry-After` header into `ApiError`;
///   callers wanting the header value must inspect it before the error surfaces.
/// - `ApiError` with HTTP 5xx (502, 503, 504) → `Unavailable`.
/// - `Reqwest` / `StreamError` → `Transport`.
/// - `JSONDeserialize` → `Other` (malformed response).
/// - Everything else → `Other(anyhow!(...))`.
pub(crate) fn map_openai_error(e: OpenAIError) -> ModelError {
    match e {
        OpenAIError::ApiError(resp) => map_api_error_response(resp),
        OpenAIError::Reqwest(re) => ModelError::Transport(re.to_string()),
        OpenAIError::JSONDeserialize(je, _body) => {
            ModelError::Other(anyhow::anyhow!("malformed openai response: {je}"))
        }
        OpenAIError::StreamError(se) => ModelError::Transport(se.to_string()),
        OpenAIError::InvalidArgument(s) => {
            ModelError::Other(anyhow::anyhow!("invalid argument: {s}"))
        }
        #[cfg(not(target_family = "wasm"))]
        OpenAIError::FileSaveError(s) => ModelError::Other(anyhow::anyhow!("file io (save): {s}")),
        #[cfg(not(target_family = "wasm"))]
        OpenAIError::FileReadError(s) => ModelError::Other(anyhow::anyhow!("file io (read): {s}")),
    }
}

fn map_api_error_response(resp: ApiErrorResponse) -> ModelError {
    let status = resp.status_code;
    let api = resp.api_error;
    let code = api.code.as_deref();
    let ty = api.r#type.as_deref();
    let msg = api.message.clone();

    if code == Some("context_length_exceeded") {
        return ModelError::ContextLengthExceeded;
    }
    if ty == Some("content_filter") {
        return ModelError::Refused { reason: msg };
    }

    // async-openai 0.40 surfaces the HTTP status code directly on
    // `ApiErrorResponse`; prefer it over message-string heuristics.
    match status.as_u16() {
        401 | 403 => ModelError::Refused { reason: msg },
        429 => ModelError::RateLimited {
            retry_after_ms: None,
        },
        502..=504 => ModelError::Unavailable,
        _ => ModelError::Other(anyhow::anyhow!("openai api error [{status}]: {msg}")),
    }
}

// Silence dead-code warnings until the backend modules consume map_openai_error
// (Task E1+).
#[allow(dead_code)]
const _SILENCE_DEAD_CODE: fn(OpenAIError) -> ModelError = map_openai_error;

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::error::{ApiError, ApiErrorResponse};

    fn api_response(
        status: u16,
        message: &str,
        code: Option<&str>,
        ty: Option<&str>,
    ) -> OpenAIError {
        OpenAIError::ApiError(ApiErrorResponse {
            status_code: reqwest::StatusCode::from_u16(status).expect("valid status"),
            api_error: ApiError {
                message: message.to_owned(),
                r#type: ty.map(str::to_owned),
                param: None,
                code: code.map(str::to_owned),
            },
        })
    }

    #[test]
    fn maps_context_length_exceeded() {
        let e = api_response(400, "ctx too long", Some("context_length_exceeded"), None);
        assert!(matches!(
            map_openai_error(e),
            ModelError::ContextLengthExceeded
        ));
    }

    #[test]
    fn maps_content_filter_to_refused() {
        let e = api_response(400, "blocked", None, Some("content_filter"));
        match map_openai_error(e) {
            ModelError::Refused { reason } => assert!(reason.contains("blocked")),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[test]
    fn maps_401_to_refused() {
        let e = api_response(401, "invalid api key", None, None);
        match map_openai_error(e) {
            ModelError::Refused { reason } => assert!(reason.contains("invalid api key")),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[test]
    fn maps_429_to_rate_limited() {
        let e = api_response(429, "rate limit exceeded", None, None);
        match map_openai_error(e) {
            ModelError::RateLimited { retry_after_ms } => assert!(retry_after_ms.is_none()),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn maps_503_to_unavailable() {
        let e = api_response(503, "service unavailable", None, None);
        assert!(matches!(map_openai_error(e), ModelError::Unavailable));
    }

    #[test]
    fn maps_generic_api_error_to_other() {
        let e = api_response(400, "kaboom", None, None);
        match map_openai_error(e) {
            ModelError::Other(_) => {}
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn maps_json_deserialize_to_other() {
        let json_err: serde_json::Error = serde_json::from_str::<u32>("not-a-number").unwrap_err();
        let e = OpenAIError::JSONDeserialize(json_err, "not-a-number".to_owned());
        match map_openai_error(e) {
            ModelError::Other(err) => {
                assert!(err.to_string().contains("malformed openai response"));
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn maps_stream_error_to_transport() {
        let e = OpenAIError::StreamError(Box::new(async_openai::error::StreamError::EventStream(
            "upstream eof".to_owned(),
        )));
        match map_openai_error(e) {
            ModelError::Transport(s) => assert!(s.contains("upstream eof")),
            other => panic!("expected Transport, got {other:?}"),
        }
    }
}
