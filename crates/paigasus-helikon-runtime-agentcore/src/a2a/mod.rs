//! A2A protocol mode: JSON-RPC 2.0 at the root path, on port 9000.
//!
//! Unlike AG-UI, A2A has its own port, so it does not collide with the HTTP-protocol
//! contract's 8080. It is still a distinct AgentCore runtime type: a container is
//! configured for one `serverProtocol`, and this mode is the one to serve when that
//! setting is `A2A`.

pub(crate) mod cancel;
pub(crate) mod card;
pub(crate) mod rpc;
pub(crate) mod store;
pub(crate) mod types;

use axum::{
    routing::{get, post},
    Router,
};

use crate::{error::AgentCoreError, ping, server::AgentCoreServer};

/// Fixed bind address for A2A mode — distinct from HTTP/AG-UI (8080) and MCP (8000).
const A2A_ADDR: &str = "0.0.0.0:9000";

impl<Ctx: Send + Sync + 'static> AgentCoreServer<Ctx> {
    /// Build the A2A router: `POST /` (JSON-RPC 2.0), the agent card, and `GET /ping`.
    ///
    /// Pure: spawns nothing. Suitable for embedding or for testing with `tower`'s
    /// `ServiceExt::oneshot`.
    ///
    /// Errors are **A2A-specification** JSON-RPC codes carried on an HTTP 200, per the
    /// specification. AWS's published `-32051`…`-32055` table describes what the
    /// *platform* returns to a client in front of this container and is never emitted
    /// here — see [`AgentCoreServer::serve_a2a`] and the crate docs.
    pub fn a2a_router(&self) -> Router {
        Router::new()
            .route("/ping", get(ping::ping))
            .route("/.well-known/agent-card.json", get(card::agent_card::<Ctx>))
            .route("/", post(rpc::dispatch::<Ctx>))
            .with_state(self.state_for_a2a())
    }

    /// Serve the configured agent over A2A: binds `0.0.0.0:9000` and serves
    /// [`AgentCoreServer::a2a_router`] until the process is terminated.
    ///
    /// Logs `"ready in {ms}ms"` immediately after the listener is bound, exactly like
    /// [`AgentCoreServer::serve`].
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::Internal`] if binding the listener or the serve loop
    /// fails.
    pub async fn serve_a2a(self) -> Result<(), AgentCoreError> {
        let start = std::time::Instant::now();
        let router = self.a2a_router();
        let listener = tokio::net::TcpListener::bind(A2A_ADDR)
            .await
            .map_err(|e| AgentCoreError::Internal(format!("failed to bind {A2A_ADDR}: {e}")))?;
        let elapsed_ms = start.elapsed().as_millis();
        tracing::info!(
            target: "paigasus::runtime_agentcore::a2a",
            elapsed_ms = elapsed_ms as u64,
            "ready in {elapsed_ms}ms"
        );
        axum::serve(listener, router)
            .await
            .map_err(|e| AgentCoreError::Internal(e.to_string()))
    }
}
