//! [`AgentServer`] — shared app state, builder, router factory, and listener helpers.

use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
    routing::{get, post},
    Router,
};
use paigasus_helikon_core::{Agent, RunConfig, Runner};
use paigasus_helikon_runtime_tokio::TokioRunner;

use crate::{
    auth::AuthLayer,
    context::{ContextProvider, DefaultContextProvider},
    error::ServerError,
    handlers,
    registry::RunRegistry,
    session::{InMemorySessionProvider, SessionLocks, SessionProvider},
};

// ── AppState ──────────────────────────────────────────────────────────────────

/// Inner shared state; allocated once and reference-counted.
pub(crate) struct AppStateInner<Ctx> {
    /// In-flight and recently-completed run registry.
    pub registry: Arc<RunRegistry>,
    /// Execution backend used by the run handlers.
    pub runner: Arc<dyn Runner<Ctx>>,
    /// Mounted agents, keyed by [`paigasus_helikon_core::Agent::name`].
    pub agents: HashMap<String, Arc<dyn Agent<Ctx>>>,
    /// Session store.
    pub sessions: Arc<dyn SessionProvider>,
    /// Per-request context builder.
    pub context: Arc<dyn ContextProvider<Ctx>>,
    /// Optional request authentication gate.
    pub auth: Option<Arc<dyn AuthLayer>>,
    /// Default run configuration applied to every run.
    pub run_config: RunConfig,
    /// Per-session run serialisation locks.
    pub locks: SessionLocks,
    /// Refuse `X-Session-Id` from callers with no established
    /// [`Principal`](crate::Principal).
    pub require_principal: bool,
}

/// Cheaply-cloneable axum extraction state.
///
/// All handler tasks share a single [`AppStateInner<Ctx>`] through this wrapper.
/// Cloning is an [`Arc`] increment, not a deep copy.
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

// ── AgentServerBuilder ────────────────────────────────────────────────────────

/// Builder for [`AgentServer`].
///
/// Obtain via [`AgentServer::builder`].  All setters consume and return `Self`
/// for chaining.  Call [`build`](AgentServerBuilder::build) once all agents and
/// optional overrides have been supplied.
pub struct AgentServerBuilder<Ctx> {
    agents: HashMap<String, Arc<dyn Agent<Ctx>>>,
    /// Non-`None` when a duplicate agent name was detected; surfaced by `build()`.
    dup_error: Option<String>,
    runner: Option<Arc<dyn Runner<Ctx>>>,
    sessions: Option<Arc<dyn SessionProvider>>,
    context: Option<Arc<dyn ContextProvider<Ctx>>>,
    auth: Option<Arc<dyn AuthLayer>>,
    run_config: RunConfig,
    max_sessions: usize,
    retention: Duration,
    max_runs: usize,
    max_events_per_run: usize,
    max_in_flight: usize,
    max_run_duration: Duration,
    /// `None` until set explicitly; `build()` then defaults it to
    /// `self.auth.is_some()`.
    require_principal: Option<bool>,
}

impl<Ctx: Send + Sync + 'static> AgentServerBuilder<Ctx> {
    fn new() -> Self {
        Self {
            agents: HashMap::new(),
            dup_error: None,
            runner: None,
            sessions: None,
            context: None,
            auth: None,
            run_config: RunConfig::default(),
            max_sessions: 4096,
            retention: Duration::from_secs(300),
            max_runs: 1024,
            max_events_per_run: 10_000,
            max_in_flight: 1024,
            max_run_duration: Duration::from_secs(3600),
            require_principal: None,
        }
    }

    /// Register an [`Agent`].
    ///
    /// If an agent with the same [`name`](paigasus_helikon_core::Agent::name) has already been
    /// registered, the duplicate is silently dropped and an error is queued; [`build`] will
    /// return that error.
    ///
    /// [`build`]: AgentServerBuilder::build
    pub fn agent(mut self, agent: Arc<dyn Agent<Ctx>>) -> Self {
        use std::collections::hash_map::Entry;
        let name = agent.name().to_owned();
        match self.agents.entry(name) {
            Entry::Occupied(e) => {
                self.dup_error = Some(e.key().clone());
            }
            Entry::Vacant(e) => {
                e.insert(agent);
            }
        }
        self
    }

    /// Override the execution backend. Defaults to [`TokioRunner`].
    pub fn runner(mut self, runner: Arc<dyn Runner<Ctx>>) -> Self {
        self.runner = Some(runner);
        self
    }

    /// Override the session provider. Defaults to an [`InMemorySessionProvider`] with
    /// `max_sessions` capacity.
    pub fn session_provider(mut self, provider: Arc<dyn SessionProvider>) -> Self {
        self.sessions = Some(provider);
        self
    }

    /// Set the context provider.
    ///
    /// Required unless [`with_default_context`](AgentServerBuilder::with_default_context) is
    /// called (which is only available when `Ctx: Default`).  [`build`] returns
    /// [`ServerError::Internal`] if neither is invoked.
    ///
    /// [`build`]: AgentServerBuilder::build
    pub fn context_provider(mut self, provider: Arc<dyn ContextProvider<Ctx>>) -> Self {
        self.context = Some(provider);
        self
    }

    /// Set an authentication layer.  If unset, all requests are admitted without authentication.
    pub fn auth(mut self, auth: Arc<dyn AuthLayer>) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Override the per-run configuration applied to every agent run.
    pub fn run_config(mut self, config: RunConfig) -> Self {
        self.run_config = config;
        self
    }

    /// Set how long completed runs are retained in the registry. Default: 5 minutes.
    pub fn run_retention(mut self, duration: Duration) -> Self {
        self.retention = duration;
        self
    }

    /// Cap the number of retained completed runs. Oldest-completed runs are evicted
    /// when the cap is exceeded. Default: 1 024.
    pub fn max_retained_runs(mut self, max: usize) -> Self {
        self.max_runs = max;
        self
    }

    /// Cap the number of tracked in-memory sessions. Default: 4 096.
    pub fn max_sessions(mut self, max: usize) -> Self {
        self.max_sessions = max;
        self
    }

    /// Cap the number of simultaneously in-flight (non-terminal) runs.
    ///
    /// Once this many runs are live, further run creation is rejected with
    /// `503 Service Unavailable` until a run reaches a terminal state.
    ///
    /// Default: 1 024, matching
    /// [`max_retained_runs`](AgentServerBuilder::max_retained_runs).
    pub fn max_in_flight(mut self, max: usize) -> Self {
        self.max_in_flight = max;
        self
    }

    /// Maximum wall-clock lifetime of a single run.
    ///
    /// A run still live after this long is cancelled and marked terminal by the
    /// registry sweeper, releasing its in-flight slot. Without this a run that
    /// never terminates — a hung agent on a detached `?mode=async` request —
    /// would hold its slot for the process lifetime and eventually exhaust
    /// [`max_in_flight`](AgentServerBuilder::max_in_flight) permanently.
    ///
    /// Default: 1 hour.
    pub fn max_run_duration(mut self, duration: Duration) -> Self {
        self.max_run_duration = duration;
        self
    }

    /// Require an authenticated [`Principal`](crate::Principal) before honouring
    /// an `X-Session-Id` header.
    ///
    /// When enabled, a request that carries `X-Session-Id` but for which no
    /// `Principal` was established is rejected with `403 Forbidden`, because it
    /// would otherwise land in a namespace shared with every other
    /// principal-less caller (CWE-639).
    ///
    /// **Default:** enabled exactly when an [`AuthLayer`] is configured. Set it
    /// explicitly to `true` when the server is *embedded* in a host application
    /// that authenticates for it — via [`AgentServer::router`] — since no `AuthLayer` is
    /// configured on this builder in that topology and the default would leave
    /// the check off.
    pub fn require_principal(mut self, required: bool) -> Self {
        self.require_principal = Some(required);
        self
    }

    /// Permit `X-Session-Id` from callers with no established principal.
    ///
    /// Equivalent to `require_principal(false)`. Appropriate for a single-tenant
    /// service or a shared-API-key deployment that genuinely wants one shared
    /// session namespace.
    ///
    /// This suppresses the 403 **and nothing else**: the session key stays
    /// compound, so a caller that *does* carry a `Principal` is still isolated
    /// to it.
    pub fn allow_unbound_sessions(mut self) -> Self {
        self.require_principal = Some(false);
        self
    }

    /// Build an [`AgentServer`].
    ///
    /// # Errors
    ///
    /// - [`ServerError::BadRequest`] — a duplicate agent name was registered, or
    ///   `max_sessions` / [`max_in_flight`](AgentServerBuilder::max_in_flight) /
    ///   [`max_run_duration`](AgentServerBuilder::max_run_duration) was set to
    ///   `0`.
    /// - [`ServerError::Internal`] — no context provider was supplied (either via
    ///   [`context_provider`](AgentServerBuilder::context_provider) or
    ///   [`with_default_context`](AgentServerBuilder::with_default_context)).
    pub fn build(self) -> Result<AgentServer<Ctx>, ServerError> {
        if let Some(name) = self.dup_error {
            return Err(ServerError::BadRequest(format!(
                "duplicate agent name: {name}"
            )));
        }

        // Reject a zero-capacity in-memory session store *before* constructing
        // it: `InMemorySessionProvider::new(0)` asserts and would panic. A
        // custom session provider sidesteps this since `max_sessions` is unused.
        if self.sessions.is_none() && self.max_sessions == 0 {
            return Err(ServerError::BadRequest(
                "max_sessions must be greater than 0".to_owned(),
            ));
        }

        // Unconditional: a zero cap would reject every run, and no custom
        // component can override it the way a session provider overrides
        // `max_sessions`.
        if self.max_in_flight == 0 {
            return Err(ServerError::BadRequest(
                "max_in_flight must be greater than 0".to_owned(),
            ));
        }

        // Unconditional, same reasoning as `max_in_flight` above: a zero
        // duration is silently indistinguishable from "works" at build time —
        // the sweeper's next tick (at most 30s later) would cancel every run
        // still executing, forever, with no error and no log. Reject it here
        // instead of letting it degrade into a permanent-outage vector.
        if self.max_run_duration.is_zero() {
            return Err(ServerError::BadRequest(
                "max_run_duration must be greater than 0".to_owned(),
            ));
        }

        let context = self.context.ok_or_else(|| {
            ServerError::Internal(
                "no context provider set; call `.context_provider(…)` or \
                 `.with_default_context()` (requires Ctx: Default)"
                    .to_owned(),
            )
        })?;

        let runner: Arc<dyn Runner<Ctx>> = self.runner.unwrap_or_else(|| Arc::new(TokioRunner));

        let sessions: Arc<dyn SessionProvider> = self
            .sessions
            .unwrap_or_else(|| Arc::new(InMemorySessionProvider::new(self.max_sessions)));

        let registry = RunRegistry::new(
            self.retention,
            self.max_runs,
            self.max_events_per_run,
            self.max_in_flight,
            self.max_run_duration,
        );

        // Default the gate to "on whenever this builder authenticates". An
        // embedded deployment whose host authenticates must opt in explicitly.
        let require_principal = self.require_principal.unwrap_or(self.auth.is_some());

        let state = AppState {
            inner: Arc::new(AppStateInner {
                registry,
                runner,
                agents: self.agents,
                sessions,
                context,
                auth: self.auth,
                run_config: self.run_config,
                locks: SessionLocks::new(),
                require_principal,
            }),
        };

        Ok(AgentServer { state })
    }
}

impl<Ctx: Default + Send + Sync + 'static> AgentServerBuilder<Ctx> {
    /// Install [`DefaultContextProvider`], satisfying the context-provider requirement for
    /// `Ctx` types that implement [`Default`].
    ///
    /// This method is only available when `Ctx: Default`.  When `Ctx` does not implement
    /// `Default`, supply a custom [`ContextProvider`] via
    /// [`context_provider`](AgentServerBuilder::context_provider) instead.
    pub fn with_default_context(self) -> Self {
        self.context_provider(Arc::new(DefaultContextProvider))
    }
}

// ── AgentServer ───────────────────────────────────────────────────────────────

/// Self-hosted HTTP server that mounts one or more [`Agent`]s on an axum router.
///
/// # Quick start
///
/// ```ignore
/// # use std::sync::Arc;
/// # use paigasus_helikon_runtime_axum::AgentServer;
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let server = AgentServer::<()>::builder()
///     .with_default_context()
///     .agent(Arc::new(my_agent))
///     .build()?;
///
/// server.serve("0.0.0.0:8080").await?;
/// # Ok(())
/// # }
/// ```
pub struct AgentServer<Ctx> {
    state: AppState<Ctx>,
}

impl<Ctx: Send + Sync + 'static> AgentServer<Ctx> {
    /// Return a new builder.
    pub fn builder() -> AgentServerBuilder<Ctx> {
        AgentServerBuilder::new()
    }

    /// Build the axum [`Router`].
    ///
    /// Also (idempotently) spawns the run registry's background reclaiming
    /// sweeper, so a server embedded via this method reclaims runs that exceed
    /// `max_run_duration` the same way
    /// [`serve_with_listener`](AgentServer::serve_with_listener) does. Without
    /// this, an embedding host that builds its own server around this router
    /// and never calls `serve_with_listener` would never spawn the sweeper —
    /// and the `max_in_flight` admission cap would become a permanent-outage
    /// vector once every slot were consumed by a run that never reaches a
    /// terminal state, exactly the failure `max_run_duration` exists to
    /// prevent. Spawning the sweeper requires an ambient Tokio runtime; if this
    /// is called outside one (e.g. while the embedding host is still
    /// assembling its router before ever running it), the spawn is skipped and
    /// a `tracing::warn!` is logged — call `router()` again from within a
    /// runtime, or call `serve_with_listener` (always async), to actually start
    /// reclamation.
    ///
    /// Otherwise pure: builds and returns a router with no side effects beyond
    /// that one-time sweeper spawn. Suitable for embedding into a larger router
    /// or for testing with axum's `Router::oneshot`.
    ///
    /// When an [`AuthLayer`] is configured the whole router is wrapped in a
    /// request-level authentication middleware, so **every** route — including
    /// `GET /agents`, `GET /openapi.json`, and the WebSocket events endpoint —
    /// is gated, not just the run-creation handler.
    pub fn router(&self) -> Router {
        self.state.registry.spawn_sweeper();
        let router = Router::new()
            .route("/agents", get(handlers::agents::list::<Ctx>))
            .route(
                "/agents/{name}/runs",
                post(handlers::runs::create_run::<Ctx>),
            )
            .route(
                "/agents/{name}/runs/{id}/events",
                get(handlers::events::events::<Ctx>),
            );

        #[cfg(feature = "openapi")]
        let router = router.route("/openapi.json", get(handlers::openapi::openapi_json::<Ctx>));

        let router = router.with_state(self.state.clone());

        // Gate every route behind the auth layer (if configured). The middleware
        // carries its own state clone, so it is applied after `with_state`.
        if self.state.auth.is_some() {
            router.layer(axum::middleware::from_fn_with_state(
                self.state.clone(),
                auth_middleware::<Ctx>,
            ))
        } else {
            router
        }
    }

    /// Start serving on `listener`.
    ///
    /// Spawns the run-registry sweeper background task, then drives the axum
    /// serve loop until it exits. [`router`](AgentServer::router) — called just
    /// below — now spawns the sweeper too (idempotently); this call stays as a
    /// redundant-but-harmless belt-and-braces spawn attempt from within a
    /// context that is always async, so it degrades to a no-op via the
    /// sweeper's internal `OnceCell` guard rather than doing anything.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Internal`] if the serve loop returns an error.
    pub async fn serve_with_listener(
        self,
        listener: tokio::net::TcpListener,
    ) -> Result<(), ServerError> {
        self.state.registry.spawn_sweeper();
        axum::serve(listener, self.router())
            .await
            .map_err(|e| ServerError::Internal(e.to_string()))
    }

    /// Bind `addr` and start serving.
    ///
    /// Convenience wrapper around [`serve_with_listener`](AgentServer::serve_with_listener).
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Internal`] if binding or serving fails.
    pub async fn serve(self, addr: impl tokio::net::ToSocketAddrs) -> Result<(), ServerError> {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| ServerError::Internal(e.to_string()))?;
        self.serve_with_listener(listener).await
    }
}

// ── auth middleware ─────────────────────────────────────────────────────────────

/// Router-level authentication gate.
///
/// Installed by [`AgentServer::router`] only when an [`AuthLayer`] is configured.
/// Runs before any route handler, so every endpoint is authenticated uniformly.
/// On success the request is reassembled from its parts so that any identity the
/// auth layer inserted into `parts.extensions` flows downstream to the
/// [`ContextProvider`].
async fn auth_middleware<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
    req: Request,
    next: Next,
) -> Result<Response, ServerError> {
    if let Some(auth) = &state.auth {
        let (mut parts, body) = req.into_parts();
        auth.authenticate(&mut parts).await?;
        // Reassemble so identity values placed into `parts.extensions` survive.
        let req = Request::from_parts(parts, body);
        Ok(next.run(req).await)
    } else {
        Ok(next.run(req).await)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────
//
// Crate-internal (not `tests/server.rs`) specifically so these two can reach
// `server.state.registry.sweeper_is_spawned()` — a `pub(crate)` peek at the
// registry's sweeper `OnceCell` that lets them prove `router()` alone spawns
// or skips spawning the sweeper, without waiting out its real 30-second tick
// interval or reaching into private state from an external integration test.

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the bug where only `serve_with_listener` spawned the
    /// reclaiming sweeper: a host embedding via `router()` alone — the
    /// documented embed topology (`require_principal`'s own docs name it) —
    /// would never reclaim an overdue run, and `max_in_flight` would become a
    /// permanent-outage vector once every slot was consumed by a run that
    /// never reaches a terminal state. `router()` must spawn the sweeper too,
    /// exactly as actix's `configure()` (its own embed path) already does.
    #[tokio::test]
    async fn router_alone_spawns_the_sweeper() {
        let server = AgentServer::<()>::builder()
            .with_default_context()
            .build()
            .expect("server builds");

        assert!(
            !server.state.registry.sweeper_is_spawned(),
            "the sweeper must not be spawned before router() is ever called"
        );

        let _router = server.router(); // NOT server.serve_with_listener(...)

        assert!(
            server.state.registry.sweeper_is_spawned(),
            "router() alone must spawn the reclaiming sweeper"
        );
    }

    /// `router()` must not panic when called with no ambient Tokio runtime —
    /// e.g. an embedding host assembling its router before ever starting an
    /// async runtime. It degrades to a no-op (logging a warning) instead, and
    /// — critically — does NOT claim the sweeper's `OnceCell` slot in that
    /// case, so a later call made from within a real runtime still spawns it.
    #[test]
    fn router_without_a_runtime_does_not_panic_and_does_not_claim_the_slot() {
        let server = AgentServer::<()>::builder()
            .with_default_context()
            .build()
            .expect("server builds");

        let _router = server.router(); // no #[tokio::test] / runtime in scope

        assert!(
            !server.state.registry.sweeper_is_spawned(),
            "with no ambient runtime, router() must not have spawned the sweeper"
        );
    }
}
