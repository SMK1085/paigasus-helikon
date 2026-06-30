//! Error types for the axum runtime server.
//!
//! [`ServerError`] is the central error enum returned by every handler. It implements
//! [`axum::response::IntoResponse`] so that handlers can use `?` and the appropriate
//! HTTP status code is automatically written to the response.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

/// A small JSON body serialised into every error response.
///
/// The field `error` carries a human-readable message. The HTTP status code conveys
/// the error class; callers should not parse the message programmatically.
#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// Human-readable description of what went wrong.
    pub error: String,
}

/// Rejection emitted by the authentication layer.
///
/// Both the status code and the message are supplied by the authentication
/// implementation so that the server can distinguish between 401 (missing or
/// invalid credentials) and 403 (valid credentials but insufficient permissions).
#[derive(Debug, Clone)]
pub struct AuthRejection {
    /// HTTP status code that should be returned to the caller (401 or 403).
    pub status: StatusCode,
    /// Human-readable reason for the rejection.
    pub message: String,
}

/// Top-level error type returned by all axum handlers in this crate.
///
/// The enum is `#[non_exhaustive]` so that future variants (e.g. new protocol
/// errors) can be added without breaking callers that match exhaustively.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ServerError {
    /// The requested agent identifier was not registered with the server (HTTP 404).
    #[error("unknown agent: {0}")]
    UnknownAgent(String),

    /// The request payload or query parameters are malformed or semantically invalid (HTTP 400).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Authentication or authorisation failed (HTTP 401 or 403, depending on the inner rejection).
    #[error("unauthorized: {0}")]
    Unauthorized(#[from] AuthRejection),

    /// A run could not be started due to an internal error (HTTP 500).
    #[error("run start failed: {0}")]
    RunStart(String),

    /// The service is temporarily unable to handle the request (HTTP 503).
    #[error("service unavailable: {0}")]
    Unavailable(String),

    /// An unexpected internal error occurred (HTTP 500).
    #[error("internal error: {0}")]
    Internal(String),
}

// Required for `#[error("unauthorized: {0}")]` with `#[from] AuthRejection`.
impl std::fmt::Display for AuthRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.status)
    }
}

// Required for `#[from] AuthRejection` in the thiserror derive.
impl std::error::Error for AuthRejection {}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let status = match &self {
            ServerError::UnknownAgent(_) => StatusCode::NOT_FOUND,
            ServerError::BadRequest(_) => StatusCode::BAD_REQUEST,
            // Clamp to a real auth status: a buggy `AuthLayer` must never let a
            // 2xx/3xx leak through as the response code for a rejected request.
            ServerError::Unauthorized(rej) => match rej.status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => rej.status,
                _ => StatusCode::UNAUTHORIZED,
            },
            ServerError::RunStart(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ServerError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ServerError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    #[test]
    fn status_mapping() {
        assert_eq!(
            ServerError::UnknownAgent("x".into())
                .into_response()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ServerError::BadRequest("x".into()).into_response().status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ServerError::RunStart("x".into()).into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            ServerError::Unavailable("x".into())
                .into_response()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            ServerError::Internal("x".into()).into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    /// An [`AuthRejection`] carrying a 401 or 403 passes through unchanged, but
    /// any other status (a buggy auth layer) is clamped to `401 Unauthorized`.
    #[test]
    fn unauthorized_status_is_clamped() {
        let unauthorized = ServerError::Unauthorized(AuthRejection {
            status: StatusCode::UNAUTHORIZED,
            message: "no creds".into(),
        });
        assert_eq!(
            unauthorized.into_response().status(),
            StatusCode::UNAUTHORIZED
        );

        let forbidden = ServerError::Unauthorized(AuthRejection {
            status: StatusCode::FORBIDDEN,
            message: "denied".into(),
        });
        assert_eq!(forbidden.into_response().status(), StatusCode::FORBIDDEN);

        // A bogus 2xx from a misbehaving auth layer must not leak through.
        let bogus = ServerError::Unauthorized(AuthRejection {
            status: StatusCode::OK,
            message: "oops".into(),
        });
        assert_eq!(bogus.into_response().status(), StatusCode::UNAUTHORIZED);
    }
}
