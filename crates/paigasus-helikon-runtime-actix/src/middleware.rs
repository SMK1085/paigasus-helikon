//! Request-level authentication middleware for the actix runtime.
//!
//! [`AuthGuard`] is a hand-rolled actix-web [`Transform`]/[`Service`] pair that
//! gates **every** wrapped route behind an [`AuthLayer`], matching the axum
//! runtime's router-level gate. It is installed by
//! [`AgentServer::configure`](crate::AgentServer::configure) only when an
//! [`AuthLayer`] is configured; otherwise the scope is registered unwrapped and
//! no authentication runs.
//!
//! # Flow
//!
//! On each [`ServiceRequest`] the guard calls
//! [`AuthLayer::authenticate`](crate::AuthLayer::authenticate) with the shared
//! [`HttpRequest`](actix_web::HttpRequest):
//!
//! - on `Err(rejection)` it short-circuits with the JSON error response from
//!   [`ServerError::Unauthorized`], and the wrapped service never runs;
//! - on `Ok(())` it forwards to the wrapped service unchanged.
//!
//! Any identity the auth layer inserts into `req.extensions_mut()` survives to
//! the handler, because the `ServiceRequest` and the handler's `HttpRequest`
//! share one `RefCell<Extensions>` — this is the documented auth→context bridge.
//!
//! # `!Send` futures
//!
//! [`AuthLayer`] is `#[async_trait(?Send)]`, so its future is not required to be
//! `Send`; the guard's `call` future is `Pin<Box<dyn Future>>` (no `Send`
//! bound), which actix polls to completion on the worker thread that received
//! the request. This mirrors actix-web's request-bound execution model.

use std::{
    future::{ready, Future, Ready},
    pin::Pin,
    rc::Rc,
    sync::Arc,
};

use actix_web::{
    body::EitherBody,
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    Error, ResponseError,
};

use crate::{auth::AuthLayer, error::ServerError};

/// Middleware factory that gates every wrapped route behind an [`AuthLayer`].
///
/// Wrap a scope with `.wrap(AuthGuard::new(auth))` to require authentication on
/// all of its routes.
pub(crate) struct AuthGuard {
    auth: Arc<dyn AuthLayer>,
}

impl AuthGuard {
    /// Construct a guard that delegates authentication to `auth`.
    pub(crate) fn new(auth: Arc<dyn AuthLayer>) -> Self {
        Self { auth }
    }
}

impl<S, B> Transform<S, ServiceRequest> for AuthGuard
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Transform = AuthGuardService<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AuthGuardService {
            service: Rc::new(service),
            auth: Arc::clone(&self.auth),
        }))
    }
}

/// The [`Service`] produced by [`AuthGuard`]; authenticates each request before
/// forwarding it to the wrapped `service`.
pub(crate) struct AuthGuardService<S> {
    service: Rc<S>,
    auth: Arc<dyn AuthLayer>,
}

impl<S, B> Service<ServiceRequest> for AuthGuardService<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let service = Rc::clone(&self.service);
        let auth = Arc::clone(&self.auth);
        Box::pin(async move {
            // The `&HttpRequest` borrow ends before `req` is moved into
            // `service.call(req)` below, so there is no conflict.
            if let Err(rejection) = auth.authenticate(req.request()).await {
                let response = ServerError::Unauthorized(rejection).error_response();
                return Ok(req.into_response(response).map_into_right_body());
            }
            service
                .call(req)
                .await
                .map(ServiceResponse::map_into_left_body)
        })
    }
}
