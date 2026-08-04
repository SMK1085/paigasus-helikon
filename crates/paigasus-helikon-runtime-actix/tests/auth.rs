//! Integration tests for the optional auth middleware gate.
//!
//! Two properties are under test:
//!
//! 1. **Gate** — when an [`AuthLayer`] is configured, EVERY route is gated: a
//!    request with no `authorization` header is rejected with `401` *before* any
//!    handler runs (parity with the axum runtime's router-level gate).
//! 2. **Identity bridge** — an identity the auth layer inserts into
//!    `req.extensions_mut()` in the middleware is visible to the
//!    [`ContextProvider`] reading `req.extensions()` in the handler, because
//!    actix's `ServiceRequest` and the handler's `HttpRequest` share one
//!    `RefCell<Extensions>`.

mod support;

use std::sync::Arc;

use actix_web::{http::StatusCode, HttpMessage, HttpRequest};
use async_trait::async_trait;
use paigasus_helikon_core::{RunContext, Session};
use paigasus_helikon_runtime_actix::{
    AgentServer, AuthLayer, AuthRejection, ContextProvider, ServerError,
};
use tokio_util::sync::CancellationToken;

use support::{echo_script, spawn_actix_server, ScriptedAgent};

/// A minimal identity value the mock auth layer inserts into `req.extensions_mut()`
/// on a successful authentication.
#[derive(Clone)]
struct Identity(String);

/// Mock [`AuthLayer`]: reject with `401` when the `authorization` header is
/// missing; otherwise insert an [`Identity`] carrying the header value into the
/// shared request extensions.
struct MockAuthLayer;

#[async_trait(?Send)]
impl AuthLayer for MockAuthLayer {
    async fn authenticate(&self, req: &HttpRequest) -> Result<(), AuthRejection> {
        let token = match req.headers().get("authorization") {
            None => {
                return Err(AuthRejection {
                    status: StatusCode::UNAUTHORIZED,
                    message: "missing authorization header".into(),
                })
            }
            Some(value) => value.to_str().unwrap_or("").to_owned(),
        };
        // Insert the identity, then let the `RefMut` from `extensions_mut()` drop
        // at the end of the statement — never hold it across the `.await` below
        // (there is none here) or a later borrow would panic.
        req.extensions_mut().insert(Identity(token));
        Ok(())
    }
}

/// Build an [`AgentServer`] mounting one `echo` agent behind [`MockAuthLayer`],
/// and spawn it. Uses the default (unit) context provider.
fn spawn_guarded_echo_server() -> String {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .auth(Arc::new(MockAuthLayer))
        .agent(Arc::new(ScriptedAgent {
            name: "echo".into(),
            events: echo_script(),
        }))
        .build()
        .expect("server builds");
    spawn_actix_server(server)
}

/// A request with NO `authorization` header is rejected with `401` on every
/// route — proving the guard gates the whole scope, not just one endpoint.
#[tokio::test]
async fn no_auth_header_is_401_on_all_routes() {
    let base = spawn_guarded_echo_server();
    let client = reqwest::Client::new();

    let list = client
        .get(format!("{base}/agents"))
        .send()
        .await
        .expect("GET /agents");
    assert_eq!(list.status(), 401, "GET /agents must be gated");

    let run = client
        .post(format!("{base}/agents/echo/runs"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .expect("POST /agents/echo/runs");
    assert_eq!(
        run.status(),
        401,
        "POST /agents/{{name}}/runs must be gated"
    );

    // The OpenAPI document route is gated too (the auth wrap sits above the
    // whole scope, so it applies even to the read-only spec endpoint).
    let openapi = client
        .get(format!("{base}/openapi.json"))
        .send()
        .await
        .expect("GET /openapi.json");
    assert_eq!(openapi.status(), 401, "GET /openapi.json must be gated");

    // The WebSocket events route is gated *before* the upgrade: with no auth
    // header the handshake fails with HTTP 401 (no `101 Switching Protocols`).
    let host = base.strip_prefix("http://").unwrap_or(&base);
    let ws_url =
        format!("ws://{host}/agents/echo/runs/00000000-0000-0000-0000-000000000000/events");
    let err = tokio_tungstenite::connect_async(ws_url)
        .await
        .expect_err("WS upgrade with no auth header must fail the handshake, not upgrade");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), 401, "WS handshake must be gated with 401");
        }
        other => panic!("expected a 401 handshake failure, got: {other:?}"),
    }
}

/// A request WITH the `authorization` header passes the guard and reaches the
/// handler (`200`).
#[tokio::test]
async fn with_auth_header_passes_gate() {
    let base = spawn_guarded_echo_server();
    let resp = reqwest::Client::new()
        .get(format!("{base}/agents"))
        .header("authorization", "Bearer tok")
        .send()
        .await
        .expect("GET /agents with auth");
    assert_eq!(resp.status(), 200);
}

/// A [`ContextProvider`] that requires an [`Identity`] to be present in the
/// request extensions, erroring otherwise.
///
/// It reads the SAME `RefCell<Extensions>` the middleware's [`MockAuthLayer`]
/// wrote to, so a `200` from a run proves the auth→context identity handoff.
struct IdentityRequiringProvider;

#[async_trait(?Send)]
impl ContextProvider<()> for IdentityRequiringProvider {
    async fn build(
        &self,
        req: &HttpRequest,
        session: Arc<dyn Session>,
        cancel: CancellationToken,
    ) -> Result<RunContext<()>, ServerError> {
        // Read (and drop the `Ref` at the end of this block — never hold it
        // across the `.await`-free build below). If the identity is absent the
        // middleware→handler bridge is broken; if present, assert the exact
        // token value round-tripped from the middleware.
        match req.extensions().get::<Identity>() {
            None => {
                return Err(ServerError::Internal(
                    "identity missing from request extensions — auth→context bridge broken".into(),
                ))
            }
            Some(identity) if identity.0 != "Bearer tok123" => {
                return Err(ServerError::Internal(format!(
                    "unexpected identity token from middleware: {}",
                    identity.0
                )))
            }
            Some(_) => {}
        }
        Ok(RunContext::ephemeral(())
            .with_session(session)
            .with_cancel(cancel))
    }
}

/// End-to-end proof of the auth→context identity bridge: [`MockAuthLayer`]
/// inserts an [`Identity`] in the middleware; [`IdentityRequiringProvider`]
/// reads it in the handler. A request carrying the `authorization` header
/// therefore builds a context successfully and the run completes (`200`). If the
/// identity did not survive the hop, the provider would fail the run with `500`.
#[tokio::test]
async fn identity_reaches_context_provider() {
    let server = AgentServer::<()>::builder()
        .context_provider(Arc::new(IdentityRequiringProvider))
        .auth(Arc::new(MockAuthLayer))
        .agent(Arc::new(ScriptedAgent {
            name: "echo".into(),
            events: echo_script(),
        }))
        .build()
        .expect("server builds");
    let base = spawn_actix_server(server);

    let resp = reqwest::Client::new()
        .post(format!("{base}/agents/echo/runs"))
        .header("content-type", "application/json")
        .header("authorization", "Bearer tok123")
        .body(r#"{"input":"hi"}"#)
        .send()
        .await
        .expect("POST /agents/echo/runs with auth");
    assert_eq!(
        resp.status(),
        200,
        "identity inserted by the middleware must be visible to the ContextProvider"
    );
}
