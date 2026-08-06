//! Authentication middleware trait for the actix runtime server.
//!
//! The [`AuthLayer`] trait is the single extension point for request authentication.
//! The server calls [`AuthLayer::authenticate`] before dispatching any request to an
//! agent handler. Implementations decide whether a request is allowed and, on success,
//! may attach identity information to [`actix_web::HttpRequest::extensions_mut`] for
//! downstream use.

use actix_web::HttpRequest;
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
/// identity value into `req.extensions_mut()`:
///
/// ```ignore
/// req.extensions_mut().insert(MyIdentity { user_id: ... });
/// ```
///
/// The server's [`ContextProvider`](crate::ContextProvider) receives the same
/// `&HttpRequest` when it builds the context value for a run, so identity values
/// placed here are available to context-building logic and, through the context, to
/// agent handlers. This is the documented auth→context bridge.
///
/// One extension type is **not** opaque to the server: [`Principal`]. Insert it
/// to name the authenticated caller, and the server scopes every session that
/// caller reaches to that name:
///
/// ```ignore
/// req.extensions_mut().insert(Principal(user_id));
/// ```
///
/// Without it, a request carrying `X-Session-Id` is refused with `403 Forbidden`
/// — see
/// [`AgentServerBuilder::require_principal`](crate::AgentServerBuilder::require_principal).
///
/// On failure the implementation returns an [`AuthRejection`] carrying the HTTP
/// status code (typically `401 Unauthorized` or `403 Forbidden`) and a
/// human-readable message. The server converts this into a JSON error response
/// and does **not** forward the request to any handler.
///
/// # `RefCell` borrows
///
/// actix request extensions are `RefCell`-backed — drop the `RefMut` from
/// `extensions_mut()` before any `.await`, and do not read `extensions()` while a
/// mutable borrow is live, or it panics at runtime.
///
/// # Thread safety
///
/// Implementations must be `Send + Sync` because the server holds a single
/// shared instance behind an `Arc` and calls `authenticate` concurrently from
/// multiple Tokio tasks.
///
/// # `?Send` futures
///
/// `actix_web::HttpRequest` wraps an `Rc` internally and is therefore `!Sync`
/// (so `&HttpRequest` is `!Send`). Any implementation that actually reads from
/// `req` — the common case — captures a `!Send` reference in its future, so
/// this trait is declared `#[async_trait(?Send)]`: the returned future is not
/// required to be `Send`. This matches actix-web's own execution model, where
/// request-bound futures are polled to completion on the worker thread that
/// received the request and never migrate across threads.
#[async_trait(?Send)]
pub trait AuthLayer: Send + Sync {
    /// Inspect and optionally mutate the request `req` to authenticate the
    /// caller.
    ///
    /// - Return `Ok(())` to allow the request to proceed. Optionally insert an
    ///   identity value into `req.extensions_mut()` for downstream consumers.
    /// - Return `Err(`[`AuthRejection`]`)` to reject the request. The server
    ///   will respond with the status code and message from the rejection.
    async fn authenticate(&self, req: &HttpRequest) -> Result<(), AuthRejection>;
}

/// A stable identity for the authenticated caller.
///
/// An [`AuthLayer`] establishes it by inserting the value into the request's
/// extensions. The server then scopes every session the caller reaches to that
/// identity, so two callers can no longer collide on one `X-Session-Id`
/// (CWE-639).
///
/// A server built with an [`AuthLayer`] but whose layer never inserts a
/// `Principal` refuses any request carrying `X-Session-Id` with `403 Forbidden`
/// — see [`AgentServerBuilder::require_principal`](crate::AgentServerBuilder::require_principal).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Principal(pub String);

#[cfg(test)]
mod tests {
    use actix_web::{http::StatusCode, test::TestRequest, HttpMessage};

    use crate::error::AuthRejection;

    use super::AuthLayer;

    /// A minimal identity value an auth impl may insert into `req.extensions_mut()`.
    #[derive(Clone)]
    struct Identity(String);

    /// Mock auth layer used by the test suite only.
    struct MockAuthLayer;

    #[async_trait::async_trait(?Send)]
    impl AuthLayer for MockAuthLayer {
        async fn authenticate(&self, req: &actix_web::HttpRequest) -> Result<(), AuthRejection> {
            let token = match req.headers().get("authorization") {
                None => {
                    return Err(AuthRejection {
                        status: StatusCode::UNAUTHORIZED,
                        message: "missing authorization header".into(),
                    })
                }
                Some(value) => value.to_str().unwrap_or("").to_owned(),
            };
            req.extensions_mut().insert(Identity(token));
            Ok(())
        }
    }

    #[tokio::test]
    async fn reject_when_header_missing() {
        let layer = MockAuthLayer;
        let req = TestRequest::default().to_http_request();
        let err = layer.authenticate(&req).await.unwrap_err();
        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn inserts_identity_on_success() {
        let layer = MockAuthLayer;
        let req = TestRequest::default()
            .insert_header(("authorization", "Bearer tok123"))
            .to_http_request();
        layer.authenticate(&req).await.unwrap();
        let identity = req.extensions().get::<Identity>().unwrap().clone();
        assert_eq!(identity.0, "Bearer tok123");
    }
}
