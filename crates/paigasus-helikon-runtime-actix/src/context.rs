//! Per-request context construction for the actix runtime.
//!
//! [`ContextProvider`] is the operator's seam for customising the
//! [`RunContext`] that every incoming HTTP request receives.  Before a handler
//! calls into the agent the server calls
//! [`ContextProvider::build`] with the resolved session and cancellation token.
//! The provider is responsible for populating:
//!
//! - the *user context* (`Ctx`) extracted from the request (e.g. parsed JWT
//!   claims, tenant id, feature flags);
//! - optionally, a stricter [`PermissionMode`] or a custom
//!   [`ApprovalHandler`] so that tool calls from network clients default to
//!   *deny* rather than the core's interactive prompt;
//! - any [`HookRegistry`] or [`TracerHandle`] the operator wants attached.
//!
//! [`DefaultContextProvider`] covers the common case where `Ctx: Default` and
//! no request-level data needs to be extracted.  It leaves all optional
//! settings at their core defaults — note that the default approval handler is
//! *deny*, which is the safe posture for a network service.
//!
//! [`PermissionMode`]: paigasus_helikon_core::PermissionMode
//! [`ApprovalHandler`]: paigasus_helikon_core::ApprovalHandler
//! [`HookRegistry`]: paigasus_helikon_core::HookRegistry
//! [`TracerHandle`]: paigasus_helikon_core::TracerHandle

use std::sync::Arc;

use actix_web::HttpRequest;
use async_trait::async_trait;
use paigasus_helikon_core::{RunContext, Session};
use tokio_util::sync::CancellationToken;

use crate::error::ServerError;

// ---------------------------------------------------------------------------
// ContextProvider trait
// ---------------------------------------------------------------------------

/// Builds the per-request [`RunContext`] from parsed request metadata.
///
/// Implement this trait to inject request-scoped data into the run context,
/// e.g. an authenticated user identity parsed from a JWT header, or to
/// tighten the permission posture by calling `.with_permission_mode` /
/// `.with_approval_handler` on the builder.
///
/// The server calls [`build`](ContextProvider::build) once per incoming
/// request, *after* the session has already been resolved and a cancellation
/// token allocated for the run.  The `req` gives access to all HTTP metadata
/// (method, URI, headers, extensions) that was available before the body was
/// consumed.
///
/// # Security seam
///
/// For network-facing services the operator *should* override the core
/// defaults here:
///
/// - Call `.with_permission_mode(PermissionMode::Deny)` to prevent the agent
///   from escalating tool permissions at runtime.
/// - Supply a custom `ApprovalHandler` that enforces the tenant's ACL. Without
///   one, core resolves every `AskUser` approval as *deny* — the safe default
///   for a network service, but it blocks any tool that requires approval.
/// - Attach a `HookRegistry` to emit telemetry or enforce policies on every
///   tool invocation.
///
/// [`DefaultContextProvider`] deliberately leaves these at core defaults —
/// acceptable only when every connected client is already trusted.
///
/// [`PermissionMode`]: paigasus_helikon_core::PermissionMode
/// [`ApprovalHandler`]: paigasus_helikon_core::ApprovalHandler
/// [`HookRegistry`]: paigasus_helikon_core::HookRegistry
///
/// # `?Send` futures
///
/// `actix_web::HttpRequest` wraps an `Rc` internally and is therefore `!Sync`
/// (so `&HttpRequest` is `!Send`). Any implementation that actually reads from
/// `req` — the common case, e.g. parsing a JWT from the headers — captures a
/// `!Send` reference in its future, so this trait is declared
/// `#[async_trait(?Send)]`: the returned future is not required to be `Send`.
/// The server awaits `build` on the worker thread that received the request
/// (before it hands the resulting `RunContext`, which *is* `Send`, to the run's
/// writer task), matching actix-web's request-bound execution model. The trait
/// object itself stays `Send + Sync`, so `Arc<dyn ContextProvider<Ctx>>` remains
/// shareable across workers.
#[async_trait(?Send)]
pub trait ContextProvider<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a [`RunContext`] for the current request.
    ///
    /// # Arguments
    ///
    /// * `req` — the HTTP request (headers, URI, extensions, …) available
    ///   before the body is consumed.
    /// * `session` — the session resolved by the server's [`SessionProvider`]
    ///   for this request.
    /// * `cancel` — cancellation token tied to the lifetime of this run; the
    ///   server cancels it when the client disconnects.
    ///
    /// # Errors
    ///
    /// Return [`ServerError::Unauthorized`] when the request's credentials are
    /// invalid; [`ServerError::BadRequest`] for malformed extraction; any other
    /// [`ServerError`] variant for unexpected failures.
    ///
    /// [`SessionProvider`]: crate::SessionProvider
    async fn build(
        &self,
        req: &HttpRequest,
        session: Arc<dyn Session>,
        cancel: CancellationToken,
    ) -> Result<RunContext<Ctx>, ServerError>;
}

// ---------------------------------------------------------------------------
// DefaultContextProvider
// ---------------------------------------------------------------------------

/// A zero-configuration [`ContextProvider`] for contexts that implement
/// [`Default`].
///
/// `build` constructs the context via `Ctx::default()` and attaches the
/// supplied session and cancellation token.  All other [`RunContext`] settings
/// (permission mode, approval handler, hooks, tracer) are left at their core
/// defaults.
///
/// This is the right choice for:
/// - unit and integration tests where context setup is not under test;
/// - development servers where all connected clients are already trusted;
/// - simple deployments where `Ctx = ()`.
///
/// For production network services, implement [`ContextProvider`] directly
/// to set a stricter permission posture (e.g. `.with_permission_mode` +
/// `.with_approval_handler`).
pub struct DefaultContextProvider;

#[async_trait(?Send)]
impl<Ctx> ContextProvider<Ctx> for DefaultContextProvider
where
    Ctx: Default + Send + Sync + 'static,
{
    async fn build(
        &self,
        _req: &HttpRequest,
        session: Arc<dyn Session>,
        cancel: CancellationToken,
    ) -> Result<RunContext<Ctx>, ServerError> {
        Ok(RunContext::ephemeral(Ctx::default())
            .with_session(session)
            .with_cancel(cancel))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_provider_builds_context_for_unit_ctx() {
        use actix_web::test::TestRequest;
        let req = TestRequest::default().to_http_request();
        let session = Arc::new(paigasus_helikon_core::MemorySession::new())
            as Arc<dyn paigasus_helikon_core::Session>;
        let cancel = tokio_util::sync::CancellationToken::new();
        let _ctx: RunContext<()> = DefaultContextProvider
            .build(&req, session, cancel)
            .await
            .unwrap();
    }

    /// A [`ContextProvider`] that actually READS `req` (the trait's documented
    /// primary use case, e.g. extracting a tenant/JWT from headers). This
    /// captures a `!Send` `&HttpRequest` in its future and therefore only
    /// compiles under `#[async_trait(?Send)]` — a plain (Send-mode)
    /// `#[async_trait]` would reject it. This test is the compile-time guard that
    /// keeps the trait `?Send`.
    struct HeaderReadingProvider;

    #[async_trait(?Send)]
    impl ContextProvider<()> for HeaderReadingProvider {
        async fn build(
            &self,
            req: &HttpRequest,
            session: Arc<dyn Session>,
            cancel: CancellationToken,
        ) -> Result<RunContext<()>, ServerError> {
            // Reading the request is what makes the returned future `!Send`.
            let _has_tenant = req.headers().get("x-tenant").is_some();
            Ok(RunContext::ephemeral(())
                .with_session(session)
                .with_cancel(cancel))
        }
    }

    #[tokio::test]
    async fn provider_reading_request_compiles_and_builds() {
        use actix_web::test::TestRequest;
        let req = TestRequest::default()
            .insert_header(("x-tenant", "acme"))
            .to_http_request();
        let session = Arc::new(paigasus_helikon_core::MemorySession::new())
            as Arc<dyn paigasus_helikon_core::Session>;
        let cancel = tokio_util::sync::CancellationToken::new();
        let _ctx: RunContext<()> = HeaderReadingProvider
            .build(&req, session, cancel)
            .await
            .unwrap();
    }
}
