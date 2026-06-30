//! Integration test for the router-wide authentication gate.
//!
//! Verifies that when an [`AuthLayer`] is configured, **every** route — not just
//! run-creation — is authenticated. We exercise `GET /agents`, which has no
//! per-handler auth of its own and is therefore only protected by the
//! router-level middleware.

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{request::Parts, StatusCode};
use paigasus_helikon_runtime_axum::{AgentServer, AuthLayer, AuthRejection};

/// Mock auth layer that rejects any request lacking an `Authorization` header.
struct HeaderRequiredAuth;

#[async_trait]
impl AuthLayer for HeaderRequiredAuth {
    async fn authenticate(&self, parts: &mut Parts) -> Result<(), AuthRejection> {
        if parts.headers.contains_key("authorization") {
            Ok(())
        } else {
            Err(AuthRejection {
                status: StatusCode::UNAUTHORIZED,
                message: "missing authorization header".into(),
            })
        }
    }
}

/// Spawn an echo server with the header-requiring auth layer installed.
async fn spawn_authed_server() -> std::net::SocketAddr {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .auth(Arc::new(HeaderRequiredAuth))
        .agent(Arc::new(support::ScriptedAgent {
            name: "echo".into(),
            events: support::echo_script(),
        }))
        .build()
        .expect("server builds");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        server
            .serve_with_listener(listener)
            .await
            .expect("serve loop");
    });
    addr
}

/// `GET /agents` must be gated by the configured auth layer: 401 without the
/// header, 200 with it. This proves auth is enforced router-wide, not only on
/// the run-creation handler.
#[tokio::test]
async fn get_agents_is_gated_by_router_auth() {
    let addr = spawn_authed_server().await;

    // No Authorization header → 401 from the router middleware.
    let resp = reqwest::get(format!("http://{addr}/agents"))
        .await
        .expect("request sent");
    assert_eq!(
        resp.status(),
        401,
        "GET /agents must be rejected without auth"
    );

    // With an Authorization header → 200.
    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/agents"))
        .header("authorization", "Bearer test-token")
        .send()
        .await
        .expect("request sent");
    assert_eq!(
        resp.status(),
        200,
        "GET /agents must succeed once authenticated"
    );
}
