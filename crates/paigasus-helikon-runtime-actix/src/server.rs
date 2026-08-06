//! [`AgentServer`] — shared app state, builder, `configure()` router factory, and
//! listener helpers for the actix-web runtime.

use std::{collections::HashMap, sync::Arc, time::Duration};

use actix_web::{
    web::{self, Data, ServiceConfig},
    App, HttpServer,
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

/// Cheaply-cloneable actix extraction state.
///
/// All handler tasks share a single [`AppStateInner<Ctx>`] through this wrapper.
/// Cloning is an [`Arc`] increment, not a deep copy. The wrapper is
/// `Send + Sync + Clone + 'static` (all inner fields are `Send + Sync`), which is
/// what [`web::Data`] and the `configure()` closure bound require.
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
    /// that authenticates for it — via [`AgentServer::configure`] — since no `AuthLayer` is
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
    ///   `max_sessions` / [`max_in_flight`](AgentServerBuilder::max_in_flight) was
    ///   set to `0`.
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

/// Self-hosted HTTP server that mounts one or more [`Agent`]s on an actix-web app.
///
/// # Quick start
///
/// ```ignore
/// # use std::sync::Arc;
/// # use paigasus_helikon_runtime_actix::AgentServer;
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

    /// Returns a closure that mounts the agent routes on an actix [`App`] at root.
    ///
    /// Call it once and pass the result to [`App::configure`]; it may be cloned
    /// freely, so the same closure can configure every worker's `App`.
    ///
    /// The routes mounted are `GET /agents`, `POST /agents/{name}/runs`,
    /// `GET /agents/{name}/runs/{id}/events`, and — behind the `openapi`
    /// feature — `GET /openapi.json`.
    pub fn configure(&self) -> impl Fn(&mut ServiceConfig) + Send + Clone + 'static {
        let state = self.state.clone();
        move |cfg: &mut ServiceConfig| {
            // Idempotent: spawns exactly one sweeper across all workers, on the
            // process-wide runtime. Runs on the embed path too (host calls
            // configure()); a built-but-never-served server never calls this, so
            // it leaks no sweeper.
            state
                .registry
                .spawn_sweeper(&crate::runtime::shared_handle());
            let scope = web::scope("")
                .app_data(Data::new(state.clone()))
                .route("/agents", web::get().to(handlers::agents::list::<Ctx>))
                .route(
                    "/agents/{name}/runs",
                    web::post().to(handlers::runs::create_run::<Ctx>),
                )
                .route(
                    "/agents/{name}/runs/{id}/events",
                    web::get().to(handlers::events::events::<Ctx>),
                );
            #[cfg(feature = "openapi")]
            let scope = scope.route(
                "/openapi.json",
                web::get().to(handlers::openapi::openapi_json::<Ctx>),
            );
            //
            // When an `AuthLayer` is configured, wrap the whole scope in the
            // `AuthGuard` middleware so EVERY route is gated (parity with the
            // axum runtime's router-level gate). `.wrap()` changes the scope's
            // type, so the `cfg.service(...)` call is branched rather than the
            // scope reassigned.
            match &state.auth {
                Some(auth) => {
                    cfg.service(scope.wrap(crate::middleware::AuthGuard::new(Arc::clone(auth))))
                }
                None => cfg.service(scope),
            };
        }
    }

    /// Bind `addr` and start serving.
    ///
    /// Convenience wrapper around
    /// [`serve_with_listener`](AgentServer::serve_with_listener).
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Internal`] if binding or serving fails.
    pub async fn serve(self, addr: impl std::net::ToSocketAddrs) -> Result<(), ServerError> {
        let listener =
            std::net::TcpListener::bind(addr).map_err(|e| ServerError::Internal(e.to_string()))?;
        self.serve_with_listener(listener).await
    }

    /// Start serving on `listener`.
    ///
    /// Spawns the (idempotent) run-registry sweeper via [`configure`](AgentServer::configure)
    /// and drives the actix serve loop until it exits.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Internal`] if the listener cannot be adopted or the
    /// serve loop returns an error.
    pub async fn serve_with_listener(
        self,
        listener: std::net::TcpListener,
    ) -> Result<(), ServerError> {
        listener
            .set_nonblocking(true)
            .map_err(|e| ServerError::Internal(e.to_string()))?;
        let cfg = self.configure();
        HttpServer::new(move || App::new().configure(cfg.clone()))
            .listen(listener)
            .map_err(|e| ServerError::Internal(e.to_string()))?
            .run()
            .await
            .map_err(|e| ServerError::Internal(e.to_string()))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use paigasus_helikon_core::{AgentError, AgentEvent, AgentInput, RunContext};

    /// Minimal agent used purely to exercise the builder's registration paths.
    struct TestAgent {
        name: String,
    }

    #[async_trait]
    impl Agent<()> for TestAgent {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "test agent"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            Ok(Box::pin(futures_util::stream::empty()))
        }
    }

    fn agent(name: &str) -> Arc<dyn Agent<()>> {
        Arc::new(TestAgent {
            name: name.to_owned(),
        })
    }

    /// Registering two agents with the same name queues a duplicate error that
    /// `build()` surfaces as [`ServerError::BadRequest`].
    #[test]
    fn duplicate_agent_name_is_bad_request() {
        let result = AgentServer::<()>::builder()
            .with_default_context()
            .agent(agent("a"))
            .agent(agent("a"))
            .build();
        assert!(matches!(result, Err(ServerError::BadRequest(_))));
    }

    /// Building without a context provider is a configuration error
    /// ([`ServerError::Internal`]).
    #[test]
    fn missing_context_provider_is_internal() {
        let result = AgentServer::<()>::builder().agent(agent("a")).build();
        assert!(matches!(result, Err(ServerError::Internal(_))));
    }

    /// `max_sessions(0)` with the default in-memory store is rejected before the
    /// store is constructed (it would otherwise panic), as
    /// [`ServerError::BadRequest`].
    #[test]
    fn zero_max_sessions_is_bad_request() {
        let result = AgentServer::<()>::builder()
            .with_default_context()
            .max_sessions(0)
            .build();
        assert!(matches!(result, Err(ServerError::BadRequest(_))));
    }

    /// The common path — default context plus one agent — builds successfully
    /// and creates no runtime.
    #[test]
    fn happy_path_builds() {
        let server = AgentServer::<()>::builder()
            .with_default_context()
            .agent(agent("a"))
            .build();
        assert!(server.is_ok());
    }

    /// `max_in_flight(0)` must be rejected before construction — it is
    /// unconditional (unlike `max_sessions`, no custom component overrides it),
    /// since a zero cap would reject every run. Asserts the message names
    /// `max_in_flight` specifically, not just the error variant — otherwise a
    /// deleted guard that happened to trip some other `BadRequest` path would
    /// still pass this test.
    #[test]
    fn zero_max_in_flight_is_bad_request() {
        let result = AgentServer::<()>::builder()
            .with_default_context()
            .max_in_flight(0)
            .build();
        let err = result.err().expect("max_in_flight(0) must fail the build");
        match err {
            ServerError::BadRequest(msg) => {
                assert!(
                    msg.contains("max_in_flight"),
                    "expected a max_in_flight message, got: {msg}"
                );
            }
            other => panic!("expected ServerError::BadRequest, got: {other}"),
        }
    }
}
