//! `AgentCoreServer::serve_mcp` — MCP-protocol mode (feature `mcp`, default on).
//!
//! AWS Bedrock AgentCore supports an MCP runtime type in addition to its default
//! HTTP-protocol contract (`src/invoke.rs`, `src/ping.rs`): the container instead
//! serves a single streamable-HTTP MCP endpoint on port `8000`. This module adds
//! [`AgentCoreServer::mcp_router`] and [`AgentCoreServer::serve_mcp`] for that mode,
//! reusing [`paigasus_helikon_mcp::McpAgentServer`] to wrap the same configured
//! [`Agent`] as a single MCP tool.

use std::sync::Arc;

use async_trait::async_trait;
use axum::{routing::get, Router};
use futures_util::stream::BoxStream;
use paigasus_helikon_core::{Agent, AgentError, AgentEvent, AgentInput, RunContext};
use paigasus_helikon_mcp::McpAgentServer;
use rmcp::transport::StreamableHttpServerConfig;

use crate::{error::AgentCoreError, ping, server::AgentCoreServer};

/// Fixed bind address for MCP mode — distinct from the HTTP-protocol contract's
/// `0.0.0.0:8080` ([`AgentCoreServer::serve`]).
const MCP_ADDR: &str = "0.0.0.0:8000";

/// Forwards to an already-shared [`Agent`] so it can be handed to
/// [`McpAgentServer::with_default_ctx`], which takes an owned, concrete [`Agent`]
/// implementation rather than an already-type-erased trait object.
///
/// [`paigasus_helikon_core::Agent`] has no blanket `impl<Ctx> Agent<Ctx> for
/// Arc<dyn Agent<Ctx>>` — under Rust's orphan rule, only `paigasus-helikon-core`
/// itself could add one (both the trait and `Arc` are foreign to this crate) — so
/// this crate-local newtype forwards the three trait methods instead. Not worth a
/// `paigasus-helikon-core` version bump for this single call site.
struct SharedAgent<Ctx>(Arc<dyn Agent<Ctx>>);

#[async_trait]
impl<Ctx: Send + Sync + 'static> Agent<Ctx> for SharedAgent<Ctx> {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn description(&self) -> &str {
        self.0.description()
    }

    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        self.0.run(ctx, input).await
    }
}

impl<Ctx: Default + Send + Sync + 'static> AgentCoreServer<Ctx> {
    /// Build the MCP-protocol router: the configured agent's single tool mounted at
    /// `/mcp`, plus a trivial `/ping` sharing the same [`PingState`](crate::PingState)
    /// as the HTTP-protocol router (see [`AgentCoreServer::router`]'s `/ping` for the
    /// exact contract; `/ping` is not part of MCP itself — cheap insurance for
    /// container health probes that expect *something* on this port too). Pure:
    /// spawns nothing; suitable for embedding or for testing with `tower`'s
    /// `ServiceExt::oneshot`.
    ///
    /// Only available for `Ctx: Default`: [`McpAgentServer`] takes a zero-argument
    /// per-call context factory ([`McpAgentServer::with_default_ctx`]), not the
    /// request-derived
    /// [`ContextProvider`](paigasus_helikon_runtime_axum::ContextProvider) that the
    /// HTTP-protocol mode ([`AgentCoreServer::router`]) uses — there is no HTTP
    /// request to derive a context from inside an MCP tool call. Each call also gets
    /// its own fresh, unshared in-memory session (see [`McpAgentServer`]'s docs): MCP
    /// mode cannot use a persistent session backend in v0, so this
    /// [`AgentCoreServer`]'s configured session/context providers are not consulted
    /// by this method at all — only the agent itself is shared between the two
    /// modes.
    ///
    /// The MCP service is configured for AgentCore's reality rather than rmcp's
    /// loopback-only defaults:
    ///
    /// - **Stateless mode** (`with_legacy_session_mode(false)`) — required because
    ///   AgentCore injects its own platform-generated `Mcp-Session-Id` header on
    ///   every request, an id this server never issued. rmcp's default *stateful*
    ///   mode would 404 on that unrecognized session; stateless mode never reads the
    ///   header at all (see
    ///   [`paigasus_helikon_mcp::McpAgentServer::streamable_http_service_with`]).
    ///   rmcp 3 renamed this from `with_stateful_mode` and narrowed its reach: per
    ///   SEP-2567 sessions are removed from protocol version `2026-07-28`, so a
    ///   client negotiating that version is served statelessly regardless. The flag
    ///   still matters here because it governs the legacy (`< 2026-07-28`) path,
    ///   which is what a client pinning an older protocol version would take.
    /// - **`disable_allowed_hosts()`** — rmcp's DNS-rebinding guard defaults to
    ///   accepting only a `Host` header of `localhost`/`127.0.0.1`/`::1`, to protect
    ///   a locally-running dev server. Real AgentCore traffic arrives from inside the
    ///   platform's microVM with an arbitrary, non-loopback `Host` header, so the
    ///   default would reject every real request with `403 Forbidden`. AgentCore's
    ///   network boundary (the microVM itself, not this in-process check) is the
    ///   actual perimeter here, so disabling the guard trades a check that cannot
    ///   distinguish "AgentCore" from "DNS rebinding" in this deployment for one that
    ///   actually serves traffic.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::Internal`] if building the underlying MCP service
    /// fails (a misconfigured [`McpAgentServer`] — this crate always supplies a
    /// context factory via [`McpAgentServer::with_default_ctx`], so in practice this
    /// does not occur).
    pub fn mcp_router(&self) -> Result<Router, AgentCoreError> {
        let agent = SharedAgent(self.agent());
        let mcp_server = McpAgentServer::with_default_ctx(agent);
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .disable_allowed_hosts();
        let service = mcp_server
            .streamable_http_service_with(config)
            .map_err(|e| AgentCoreError::Internal(e.to_string()))?;

        let ping_router = Router::new()
            .route("/ping", get(ping::ping))
            .with_state(self.ping_state());

        Ok(Router::new()
            .nest_service("/mcp", service)
            .merge(ping_router))
    }

    /// Serve the configured agent as an MCP server instead of the HTTP-protocol
    /// contract: binds `0.0.0.0:8000` and serves [`AgentCoreServer::mcp_router`]
    /// until the process is terminated.
    ///
    /// Logs `"ready in {ms}ms"` immediately after the listener is bound, exactly like
    /// [`AgentCoreServer::serve`] — the two modes share the same app-side cold-start
    /// measurement convention.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::Internal`] if building the MCP router, binding the
    /// listener, or the serve loop itself fails.
    pub async fn serve_mcp(self) -> Result<(), AgentCoreError> {
        let start = std::time::Instant::now();
        let router = self.mcp_router()?;
        let listener = tokio::net::TcpListener::bind(MCP_ADDR)
            .await
            .map_err(|e| AgentCoreError::Internal(format!("failed to bind {MCP_ADDR}: {e}")))?;
        let elapsed_ms = start.elapsed().as_millis();
        tracing::info!(
            target: "paigasus::runtime_agentcore::mcp",
            elapsed_ms = elapsed_ms as u64,
            "ready in {elapsed_ms}ms"
        );
        axum::serve(listener, router)
            .await
            .map_err(|e| AgentCoreError::Internal(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
    };
    use futures_util::stream::{self, StreamExt as _};
    use paigasus_helikon_core::TokenUsage;
    use tower::ServiceExt as _;

    /// A minimal test [`Agent`] exposing one MCP tool (`echo`) with no real logic.
    struct EchoAgent;

    #[async_trait]
    impl Agent<()> for EchoAgent {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "test-only echo agent"
        }

        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            Ok(stream::iter(vec![AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            }])
            .boxed())
        }
    }

    fn test_server() -> AgentCoreServer<()> {
        AgentCoreServer::builder()
            .agent(Arc::new(EchoAgent))
            .with_default_context()
            .build()
            .expect("server builds")
    }

    /// Mirrors `paigasus-helikon-mcp`'s own stateless-mode test (Task 11), but
    /// through the AgentCore mount and adding a non-loopback `Host` header — the
    /// exact platform-injection scenario this module's stateless + disabled-hosts
    /// configuration exists for.
    #[tokio::test]
    async fn mcp_mode_accepts_unknown_session_id_and_non_loopback_host() {
        let router = test_server().mcp_router().expect("mcp router builds");

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .header(header::HOST, "agentcore-runtime.internal")
                    .header("Mcp-Session-Id", "platform-generated-id-0123456789abcdef")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(
            response.status().is_success(),
            "expected a 2xx response, got {}",
            response.status()
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            text.contains("echo"),
            "response did not list the echo tool: {text}"
        );
    }

    #[tokio::test]
    async fn ping_is_reachable_on_the_mcp_router() {
        let response = test_server()
            .mcp_router()
            .expect("mcp router builds")
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
