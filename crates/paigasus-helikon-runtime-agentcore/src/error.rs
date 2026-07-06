//! Error types for the AgentCore server.
//!
//! [`AgentCoreError`] is the single error type returned by
//! [`crate::AgentCoreServer::serve`], [`crate::AgentCoreServerBuilder::build`], and every
//! HTTP handler in this crate. It implements [`axum::response::IntoResponse`] so handlers
//! can propagate it with `?` and get a contract-shaped JSON error body with the
//! appropriate HTTP status code.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

/// JSON body serialised into every error response produced by this crate.
///
/// The `error` field carries a human-readable message; callers should key off the HTTP
/// status code, not this string, to distinguish error classes.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// Human-readable description of what went wrong.
    pub error: String,
}

/// Errors produced by the AgentCore server.
///
/// `#[non_exhaustive]` so future variants (e.g. new contract violations) can be added
/// without breaking callers that match exhaustively.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AgentCoreError {
    /// The request violated the AgentCore contract: a malformed body, an invalid or
    /// out-of-range session header, etc. (HTTP 400).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// The requested capability is not yet implemented by this server (HTTP 501).
    ///
    /// Used by the `/invocations` placeholder route until the full request/response
    /// contract is implemented.
    #[error("not implemented: {0}")]
    NotImplemented(String),

    /// An unexpected internal error occurred — including a failure to bind the listener
    /// in [`crate::AgentCoreServer::serve`] or a misconfigured
    /// [`AgentCoreServerBuilder`](crate::AgentCoreServerBuilder) (HTTP 500).
    #[error("internal error: {0}")]
    Internal(String),
}

/// Adapts a [`paigasus_helikon_runtime_axum::ServerError`] — raised by the reused
/// `SessionProvider`/`ContextProvider` seams — into an [`AgentCoreError`], so
/// `/invocations` can propagate either error type with a single `?`.
///
/// `ServerError` is `#[non_exhaustive]`, so this match ends in a wildcard arm: every
/// variant this crate does not special-case (`UnknownAgent`, `Unauthorized`, `RunStart`,
/// `Unavailable`, and any future addition) becomes [`AgentCoreError::Internal`]. Only
/// `BadRequest` is preserved as a client error — the session/context seams reused here
/// never raise `ServerError`'s agent-registry or auth-layer variants, which are
/// specific to `runtime-axum`'s multi-agent router.
impl From<paigasus_helikon_runtime_axum::ServerError> for AgentCoreError {
    fn from(err: paigasus_helikon_runtime_axum::ServerError) -> Self {
        match err {
            paigasus_helikon_runtime_axum::ServerError::BadRequest(msg) => {
                AgentCoreError::BadRequest(msg)
            }
            other => AgentCoreError::Internal(other.to_string()),
        }
    }
}

impl IntoResponse for AgentCoreError {
    fn into_response(self) -> Response {
        let status = match &self {
            AgentCoreError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AgentCoreError::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
            AgentCoreError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };

        let body = ErrorBody {
            error: self.to_string(),
        };

        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert_eq!(
            AgentCoreError::BadRequest("x".into())
                .into_response()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            AgentCoreError::NotImplemented("x".into())
                .into_response()
                .status(),
            StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(
            AgentCoreError::Internal("x".into())
                .into_response()
                .status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
