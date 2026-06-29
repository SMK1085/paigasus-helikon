//! Authentication middleware trait for the axum runtime server.
//!
//! The [`AuthLayer`] trait is the single extension point for request authentication.
//! The server calls [`AuthLayer::authenticate`] before dispatching any request to an
//! agent handler. Implementations decide whether a request is allowed and, on success,
//! may attach identity information to [`axum::http::request::Parts::extensions`] for
//! downstream use.

use async_trait::async_trait;

use crate::error::AuthRejection;

/// Middleware hook called by the server on every inbound request before routing.
///
/// Implement this trait to plug in any authentication scheme — API keys, JWTs,
/// mutual TLS, etc. — without touching the agent or transport code.
///
/// # Identity handoff
///
/// On a successful authentication the implementation **may** insert an opaque
/// identity value into `parts.extensions`:
///
/// ```ignore
/// parts.extensions.insert(MyIdentity { user_id: ... });
/// ```
///
/// The server's [`ContextProvider`](crate::ContextProvider) receives the same
/// `&Parts` when it builds the context value for a run, so identity values placed
/// here are available to context-building logic and, through the context, to agent
/// handlers. This is the documented auth→context bridge.
///
/// On failure the implementation returns an [`AuthRejection`] carrying the HTTP
/// status code (typically `401 Unauthorized` or `403 Forbidden`) and a
/// human-readable message. The server converts this into a JSON error response
/// and does **not** forward the request to any handler.
///
/// # Thread safety
///
/// Implementations must be `Send + Sync` because the server holds a single
/// shared instance behind an `Arc` and calls `authenticate` concurrently from
/// multiple Tokio tasks.
#[async_trait]
pub trait AuthLayer: Send + Sync {
    /// Inspect and optionally mutate the request `parts` to authenticate the
    /// caller.
    ///
    /// - Return `Ok(())` to allow the request to proceed. Optionally insert an
    ///   identity value into `parts.extensions` for downstream consumers.
    /// - Return `Err(`[`AuthRejection`]`)` to reject the request. The server
    ///   will respond with the status code and message from the rejection.
    async fn authenticate(
        &self,
        parts: &mut axum::http::request::Parts,
    ) -> Result<(), AuthRejection>;
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderValue, Request, StatusCode};

    use crate::error::AuthRejection;

    use super::AuthLayer;

    /// A minimal identity value an auth impl may insert into `parts.extensions`.
    #[derive(Clone)]
    struct Identity(String);

    /// Mock auth layer used by the test suite only.
    struct MockAuthLayer;

    #[async_trait::async_trait]
    impl AuthLayer for MockAuthLayer {
        async fn authenticate(
            &self,
            parts: &mut axum::http::request::Parts,
        ) -> Result<(), AuthRejection> {
            match parts.headers.get("authorization") {
                None => Err(AuthRejection {
                    status: StatusCode::UNAUTHORIZED,
                    message: "missing authorization header".into(),
                }),
                Some(value) => {
                    let token = value.to_str().unwrap_or("").to_owned();
                    parts.extensions.insert(Identity(token));
                    Ok(())
                }
            }
        }
    }

    fn make_parts(auth_value: Option<&str>) -> axum::http::request::Parts {
        let mut builder = Request::builder();
        if let Some(v) = auth_value {
            builder = builder.header("authorization", HeaderValue::from_str(v).unwrap());
        }
        let (parts, _body) = builder.body(()).unwrap().into_parts();
        parts
    }

    #[tokio::test]
    async fn reject_when_header_missing() {
        let layer = MockAuthLayer;
        let mut parts = make_parts(None);
        let err = layer.authenticate(&mut parts).await.unwrap_err();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn inserts_identity_on_success() {
        let layer = MockAuthLayer;
        let mut parts = make_parts(Some("Bearer tok123"));
        layer.authenticate(&mut parts).await.unwrap();
        let identity = parts.extensions.get::<Identity>().unwrap();
        assert_eq!(identity.0, "Bearer tok123");
    }
}
