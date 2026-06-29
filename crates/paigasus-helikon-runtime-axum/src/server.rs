//! [`AgentServer`] — shared app state, builder, router factory, and listener helpers.

use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{routing::get, Router};
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
    // Used by run handlers added in Task 10.
    #[allow(dead_code)]
    pub runner: Arc<dyn Runner<Ctx>>,
    /// Mounted agents, keyed by [`paigasus_helikon_core::Agent::name`].
    pub agents: HashMap<String, Arc<dyn Agent<Ctx>>>,
    /// Session store.
    // Used by run handlers added in Task 10.
    #[allow(dead_code)]
    pub sessions: Arc<dyn SessionProvider>,
    /// Per-request context builder.
    // Used by run handlers added in Task 10.
    #[allow(dead_code)]
    pub context: Arc<dyn ContextProvider<Ctx>>,
    /// Optional request authentication gate.
    // Used by run handlers added in Task 10.
    #[allow(dead_code)]
    pub auth: Option<Arc<dyn AuthLayer>>,
    /// Default run configuration applied to every run.
    // Used by run handlers added in Task 10.
    #[allow(dead_code)]
    pub run_config: RunConfig,
    /// Per-session run serialisation locks (consumed by Task 10 transport handlers).
    #[allow(dead_code)]
    pub locks: SessionLocks,
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

    /// Build an [`AgentServer`].
    ///
    /// # Errors
    ///
    /// - [`ServerError::BadRequest`] — a duplicate agent name was registered.
    /// - [`ServerError::Internal`] — no context provider was supplied (either via
    ///   [`context_provider`](AgentServerBuilder::context_provider) or
    ///   [`with_default_context`](AgentServerBuilder::with_default_context)).
    pub fn build(self) -> Result<AgentServer<Ctx>, ServerError> {
        if let Some(name) = self.dup_error {
            return Err(ServerError::BadRequest(format!(
                "duplicate agent name: {name}"
            )));
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

        let registry = RunRegistry::new(self.retention, self.max_runs, self.max_events_per_run);

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
    /// Pure: spawns nothing.  Suitable for embedding into a larger router or for
    /// testing with axum's `Router::oneshot`.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/agents", get(handlers::agents::list::<Ctx>))
            .with_state(self.state.clone())
    }

    /// Start serving on `listener`.
    ///
    /// Spawns the run-registry sweeper background task, then drives the axum
    /// serve loop until it exits.
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
