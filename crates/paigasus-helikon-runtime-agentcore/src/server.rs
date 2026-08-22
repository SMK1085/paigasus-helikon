//! [`AgentCoreServer`] — shared app state, builder, and router factory.

use std::sync::Arc;

use axum::{
    extract::FromRef,
    routing::{get, post},
    Router,
};
use paigasus_helikon_core::{Agent, RunConfig, Runner};
use paigasus_helikon_runtime_axum::{
    ContextProvider, DefaultContextProvider, InMemorySessionProvider, SessionProvider,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

use crate::{
    error::AgentCoreError,
    invoke,
    ping::{self, PingState},
};

/// Capacity of the default in-memory session provider when the caller does not supply
/// a custom [`SessionProvider`] via
/// [`AgentCoreServerBuilder::session_provider`].
const DEFAULT_MAX_SESSIONS: usize = 4096;

/// Capacity of the default [`InMemoryTaskStore`](crate::InMemoryTaskStore) when the
/// caller does not supply one via
/// [`AgentCoreServerBuilder::task_store`]. Smaller than
/// [`DEFAULT_MAX_SESSIONS`] because a task retains its whole event log, not just a key.
#[cfg(feature = "a2a")]
const DEFAULT_MAX_TASKS: usize = 1024;

// ── AppState ──────────────────────────────────────────────────────────────────

/// Inner shared state; allocated once per [`AgentCoreServer`] and reference-counted.
///
/// This is the shape a handler reaches for via `State<AppState<Ctx>>` (or, for `/ping`,
/// via the [`FromRef`] substate below). `pub(crate)` (rather than private) so
/// [`crate::invoke`]'s `/invocations` handler — in its own module, per the crate's
/// one-file-per-route layout — can read every field without re-deriving the state
/// layout; neither the type nor its fields are re-exported from the crate root, so
/// this stays out of the public API.
pub(crate) struct AppStateInner<Ctx> {
    /// The single agent this AgentCore container serves.
    pub(crate) agent: Arc<dyn Agent<Ctx>>,
    /// Execution backend driving each invocation.
    pub(crate) runner: Arc<dyn Runner<Ctx>>,
    /// Session store consulted for requests carrying the AgentCore session header.
    pub(crate) sessions: Arc<dyn SessionProvider>,
    /// Per-request context builder.
    pub(crate) context: Arc<dyn ContextProvider<Ctx>>,
    /// Default run configuration applied to every invocation.
    pub(crate) run_config: RunConfig,
    /// A2A task store backing `tasks/*`. Defaults to a bounded in-memory store.
    #[cfg(feature = "a2a")]
    pub(crate) tasks: Arc<dyn crate::TaskStore>,
    /// Live-run cancellation tokens, keyed by A2A task id.
    #[cfg(feature = "a2a")]
    pub(crate) cancels: Arc<crate::a2a::cancel::CancelRegistry>,
    /// Caller-supplied agent card, overriding the card derived from the agent.
    #[cfg(feature = "a2a")]
    pub(crate) card: Option<crate::AgentCard>,
    /// Caller-supplied agent-card URL, used when `AGENTCORE_RUNTIME_URL` is unset.
    #[cfg(feature = "a2a")]
    pub(crate) card_url: Option<String>,
    /// Shared health-check state backing `GET /ping`.
    ping: Arc<PingState>,
}

/// Cheaply-cloneable axum extraction state.
///
/// All handler tasks share a single [`AppStateInner<Ctx>`] through this wrapper. Cloning
/// is an [`Arc`] increment, not a deep copy. `pub(crate)` for the same reason as
/// [`AppStateInner`] — [`crate::invoke::invocations`] takes `State<AppState<Ctx>>`
/// directly rather than a bespoke extractor.
pub(crate) struct AppState<Ctx> {
    inner: Arc<AppStateInner<Ctx>>,
}

impl<Ctx> Clone for AppState<Ctx> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<Ctx> std::ops::Deref for AppState<Ctx> {
    type Target = AppStateInner<Ctx>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Lets the `/ping` handler extract just the [`PingState`] substate instead of the full
/// [`AppState`], so it type-checks with no dependency on `Ctx` at the call site and stays
/// obviously decoupled from runner/agent/session state.
impl<Ctx> FromRef<AppState<Ctx>> for Arc<PingState> {
    fn from_ref(state: &AppState<Ctx>) -> Self {
        Arc::clone(&state.ping)
    }
}

// ── AgentCoreServerBuilder ────────────────────────────────────────────────────

/// Builder for [`AgentCoreServer`].
///
/// Obtain via [`AgentCoreServer::builder`]. All setters consume and return `Self` for
/// chaining. Call [`build`](AgentCoreServerBuilder::build) once an agent and a context
/// provider have been supplied.
pub struct AgentCoreServerBuilder<Ctx> {
    agent: Option<Arc<dyn Agent<Ctx>>>,
    runner: Option<Arc<dyn Runner<Ctx>>>,
    sessions: Option<Arc<dyn SessionProvider>>,
    context: Option<Arc<dyn ContextProvider<Ctx>>>,
    run_config: RunConfig,
    #[cfg(feature = "a2a")]
    tasks: Option<Arc<dyn crate::TaskStore>>,
    #[cfg(feature = "a2a")]
    card: Option<crate::AgentCard>,
    #[cfg(feature = "a2a")]
    card_url: Option<String>,
}

impl<Ctx: Send + Sync + 'static> AgentCoreServerBuilder<Ctx> {
    fn new() -> Self {
        Self {
            agent: None,
            runner: None,
            sessions: None,
            context: None,
            run_config: RunConfig::default(),
            #[cfg(feature = "a2a")]
            tasks: None,
            #[cfg(feature = "a2a")]
            card: None,
            #[cfg(feature = "a2a")]
            card_url: None,
        }
    }

    /// Set the single [`Agent`] this AgentCore container serves.
    ///
    /// Required: [`build`](AgentCoreServerBuilder::build) returns
    /// [`AgentCoreError::Internal`] if this is never called.
    pub fn agent(mut self, agent: Arc<dyn Agent<Ctx>>) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Override the execution backend. Defaults to [`TokioRunner`].
    pub fn runner(mut self, runner: Arc<dyn Runner<Ctx>>) -> Self {
        self.runner = Some(runner);
        self
    }

    /// Override the session provider. Defaults to an [`InMemorySessionProvider`] bounded
    /// to a fixed capacity.
    pub fn session_provider(mut self, provider: Arc<dyn SessionProvider>) -> Self {
        self.sessions = Some(provider);
        self
    }

    /// Set the context provider.
    ///
    /// Required unless [`with_default_context`](AgentCoreServerBuilder::with_default_context)
    /// is called (only available when `Ctx: Default`). [`build`] returns
    /// [`AgentCoreError::Internal`] if neither is invoked.
    ///
    /// [`build`]: AgentCoreServerBuilder::build
    pub fn context_provider(mut self, provider: Arc<dyn ContextProvider<Ctx>>) -> Self {
        self.context = Some(provider);
        self
    }

    /// Override the run configuration applied to every invocation. Defaults to
    /// [`RunConfig::default`].
    pub fn run_config(mut self, config: RunConfig) -> Self {
        self.run_config = config;
        self
    }

    /// Build an [`AgentCoreServer`].
    ///
    /// # Errors
    ///
    /// - [`AgentCoreError::Internal`] — no agent was registered; call
    ///   [`agent`](AgentCoreServerBuilder::agent).
    /// - [`AgentCoreError::Internal`] — no context provider was supplied (either via
    ///   [`context_provider`](AgentCoreServerBuilder::context_provider) or
    ///   [`with_default_context`](AgentCoreServerBuilder::with_default_context)).
    pub fn build(self) -> Result<AgentCoreServer<Ctx>, AgentCoreError> {
        let agent = self.agent.ok_or_else(|| {
            AgentCoreError::Internal(
                "no agent set; call `.agent(...)` before `.build()`".to_owned(),
            )
        })?;

        let context = self.context.ok_or_else(|| {
            AgentCoreError::Internal(
                "no context provider set; call `.context_provider(…)` or \
                 `.with_default_context()` (requires Ctx: Default)"
                    .to_owned(),
            )
        })?;

        let runner: Arc<dyn Runner<Ctx>> = self.runner.unwrap_or_else(|| Arc::new(TokioRunner));

        let sessions: Arc<dyn SessionProvider> = self
            .sessions
            .unwrap_or_else(|| Arc::new(InMemorySessionProvider::new(DEFAULT_MAX_SESSIONS)));

        Ok(AgentCoreServer {
            state: AppState {
                inner: Arc::new(AppStateInner {
                    agent,
                    runner,
                    sessions,
                    context,
                    run_config: self.run_config,
                    #[cfg(feature = "a2a")]
                    tasks: self.tasks.unwrap_or_else(|| {
                        Arc::new(crate::InMemoryTaskStore::new(DEFAULT_MAX_TASKS))
                    }),
                    #[cfg(feature = "a2a")]
                    cancels: Arc::new(crate::a2a::cancel::CancelRegistry::default()),
                    #[cfg(feature = "a2a")]
                    card: self.card,
                    #[cfg(feature = "a2a")]
                    card_url: self.card_url,
                    ping: Arc::new(PingState::default()),
                }),
            },
        })
    }
}

/// A2A builder configuration. Gated on the `a2a` feature so the whole surface — fields,
/// setters, and their `build()` initializers — compiles out together.
#[cfg(feature = "a2a")]
impl<Ctx: Send + Sync + 'static> AgentCoreServerBuilder<Ctx> {
    /// Override the A2A task store. Defaults to a bounded
    /// [`InMemoryTaskStore`](crate::InMemoryTaskStore).
    ///
    /// Supply a durable store to survive AgentCore's abrupt container termination — the
    /// default loses every task with the microVM, which makes `tasks/get` and
    /// `tasks/resubscribe` useless across a restart.
    pub fn task_store(mut self, store: Arc<dyn crate::TaskStore>) -> Self {
        self.tasks = Some(store);
        self
    }

    /// Replace the agent card derived from the configured agent.
    ///
    /// Use this when the derived card is wrong for the deployment — most often to
    /// publish the real agent version (the derived card reports *this crate's* version,
    /// since a library cannot read its host binary's) or a curated skill list.
    pub fn agent_card(mut self, card: crate::AgentCard) -> Self {
        self.card = Some(card);
        self
    }

    /// Set the agent card's `url` explicitly, for deployments where
    /// `AGENTCORE_RUNTIME_URL` is not set.
    ///
    /// Ignored when [`agent_card`](AgentCoreServerBuilder::agent_card) supplies a
    /// complete card, which carries its own url.
    pub fn agent_card_url(mut self, url: impl Into<String>) -> Self {
        self.card_url = Some(url.into());
        self
    }
}

impl<Ctx: Default + Send + Sync + 'static> AgentCoreServerBuilder<Ctx> {
    /// Install [`DefaultContextProvider`], satisfying the context-provider requirement
    /// for `Ctx` types that implement [`Default`].
    ///
    /// This method is only available when `Ctx: Default`. For a `Ctx` that does not
    /// implement `Default`, supply a custom [`ContextProvider`] via
    /// [`context_provider`](AgentCoreServerBuilder::context_provider) instead.
    pub fn with_default_context(self) -> Self {
        self.context_provider(Arc::new(DefaultContextProvider))
    }
}

// ── AgentCoreServer ───────────────────────────────────────────────────────────

/// Self-hosted HTTP server that mounts a single [`Agent`] on an axum router satisfying
/// the AWS Bedrock AgentCore HTTP-protocol container contract.
///
/// # Quick start
///
/// ```ignore
/// # use std::sync::Arc;
/// # use paigasus_helikon_runtime_agentcore::AgentCoreServer;
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let server = AgentCoreServer::<()>::builder()
///     .with_default_context()
///     .agent(Arc::new(my_agent))
///     .build()?;
///
/// server.serve().await?;
/// # Ok(())
/// # }
/// ```
pub struct AgentCoreServer<Ctx> {
    state: AppState<Ctx>,
}

impl<Ctx: Send + Sync + 'static> AgentCoreServer<Ctx> {
    /// Return a new builder.
    pub fn builder() -> AgentCoreServerBuilder<Ctx> {
        AgentCoreServerBuilder::new()
    }

    /// Build the axum [`Router`].
    ///
    /// Pure: spawns nothing. Mounts `GET /ping` (see [`PingState`]; always resolves,
    /// independent of the agent/runner/session state) and `POST /invocations` (accepts
    /// [`InvocationRequest`](crate::InvocationRequest)'s three body shapes and serves
    /// both the buffered-JSON and Server-Sent-Events response modes — see the crate's
    /// top-level docs for the full request/response contract). Also mounts the optional
    /// `GET /ws` WebSocket endpoint when the `ws` feature is enabled (default on),
    /// carrying the same request vocabulary as `POST /invocations` over a persistent
    /// connection. Suitable for embedding into a larger router or for testing with
    /// `tower`'s `ServiceExt::oneshot` (except `/ws`, which needs a real listener — see
    /// `src/ws.rs`'s tests).
    pub fn router(&self) -> Router {
        #[allow(unused_mut)]
        let mut router = Router::new()
            .route("/ping", get(ping::ping))
            .route("/invocations", post(invoke::invocations::<Ctx>));

        #[cfg(feature = "ws")]
        {
            router = router.route("/ws", get(crate::ws::ws_upgrade::<Ctx>));
        }

        router.with_state(self.state.clone())
    }

    /// Return a handle to the shared health-check state backing `GET /ping`.
    ///
    /// Call [`PingState::set_busy`] on the returned handle to flag long-running
    /// asynchronous work (e.g. performed by a tool) so the *next* health check reflects
    /// it immediately.
    pub fn ping_state(&self) -> Arc<PingState> {
        Arc::clone(&self.state.ping)
    }

    /// Return a clone of the configured [`Agent`] handle.
    ///
    /// `pub(crate)`: an internal wiring seam for [`crate::mcp`]'s MCP-protocol mode
    /// (feature `mcp`), which needs the shared agent to build its own
    /// `McpAgentServer`. Not public API — callers configure the agent via
    /// [`AgentCoreServerBuilder::agent`] and never need it handed back.
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub(crate) fn agent(&self) -> Arc<dyn Agent<Ctx>> {
        Arc::clone(&self.state.agent)
    }

    /// Return a clone of the shared [`AppState`] (an `Arc` increment, not a deep copy).
    ///
    /// `pub(crate)`: an internal wiring seam for the AG-UI protocol mode's (feature
    /// `ag-ui`) own router, which — unlike [`crate::mcp`]'s ping-only need — requires
    /// the full state (agent, runner, context provider, run config) rather than just
    /// [`PingState`]. A named accessor rather than exposing the field directly, matching
    /// this crate's existing [`agent`](AgentCoreServer::agent)/
    /// [`ping_state`](AgentCoreServer::ping_state) style.
    #[cfg_attr(not(feature = "ag-ui"), allow(dead_code))]
    pub(crate) fn state_for_agui(&self) -> AppState<Ctx> {
        self.state.clone()
    }

    /// Return a clone of the shared [`AppState`] for the A2A protocol mode's router.
    ///
    /// The A2A counterpart of [`state_for_agui`](AgentCoreServer::state_for_agui): its
    /// handlers need the full state, including the `a2a`-gated task store, cancel
    /// registry, and agent-card overrides.
    #[cfg(feature = "a2a")]
    pub(crate) fn state_for_a2a(&self) -> AppState<Ctx> {
        self.state.clone()
    }

    /// Bind `0.0.0.0:8080` — the fixed port AgentCore's HTTP-protocol contract expects —
    /// and serve until the process is terminated.
    ///
    /// Logs `"ready in {ms}ms"` immediately after the listener is bound: AgentCore's
    /// cold-start acceptance criterion is measured against this app-side signal (the
    /// platform's own microVM provisioning latency is outside this crate's control).
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::Internal`] if binding the listener or the serve loop
    /// itself fails.
    pub async fn serve(self) -> Result<(), AgentCoreError> {
        let start = std::time::Instant::now();
        let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
            .await
            .map_err(|e| AgentCoreError::Internal(format!("failed to bind 0.0.0.0:8080: {e}")))?;
        let router = self.router();
        let elapsed_ms = start.elapsed().as_millis();
        tracing::info!(
            target: "paigasus::runtime_agentcore::server",
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
    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use futures_util::stream::{self, StreamExt as _};
    use paigasus_helikon_core::{AgentError, AgentEvent, AgentInput, RunContext, TokenUsage};
    use tower::ServiceExt as _;

    /// A minimal test [`Agent`] that completes immediately with no events.
    struct NoopAgent;

    #[async_trait]
    impl Agent<()> for NoopAgent {
        fn name(&self) -> &str {
            "noop"
        }

        fn description(&self) -> &str {
            "test-only no-op agent"
        }

        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<futures_util::stream::BoxStream<'static, AgentEvent>, AgentError> {
            Ok(stream::iter(vec![AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            }])
            .boxed())
        }
    }

    fn test_server() -> AgentCoreServer<()> {
        AgentCoreServer::builder()
            .agent(Arc::new(NoopAgent))
            .with_default_context()
            .build()
            .expect("server builds")
    }

    #[tokio::test]
    async fn ping_is_reachable_through_the_full_router() {
        let resp = test_server()
            .router()
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Router-level smoke test: `/invocations` is mounted and reachable. An empty
    /// body is not valid JSON for [`crate::invoke::InvocationRequest`], so this
    /// asserts `400 Bad Request` (proving the route dispatches into the real handler)
    /// rather than `404 Not Found` (which a missing/unmounted route would return).
    /// The full request/response contract (JSON/SSE modes, session handling) is
    /// covered by `invoke.rs`'s own tests.
    #[tokio::test]
    async fn invocations_is_reachable_through_the_full_router() {
        let resp = test_server()
            .router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/invocations")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn build_without_agent_errors() {
        let result = AgentCoreServer::<()>::builder()
            .with_default_context()
            .build();
        let Err(err) = result else {
            panic!("expected build() to fail without an agent");
        };
        assert!(matches!(err, AgentCoreError::Internal(_)));
    }

    #[test]
    fn build_without_context_provider_errors() {
        let result = AgentCoreServer::<()>::builder()
            .agent(Arc::new(NoopAgent))
            .build();
        let Err(err) = result else {
            panic!("expected build() to fail without a context provider");
        };
        assert!(matches!(err, AgentCoreError::Internal(_)));
    }

    #[tokio::test]
    async fn ping_state_accessor_shares_state_with_the_mounted_handler() {
        let server = test_server();
        let ping_state = server.ping_state();
        ping_state.set_busy(true);

        let resp = server
            .router()
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["status"], "HealthyBusy");
    }
}
