//! Run-scoped context types.
//!
//! [`RunContext`] carries user data, session handle, hook registry, tracer,
//! and cancellation token across the agent loop.

use std::sync::Arc;

use crate::{FailureSlot, Hook, RunConfig, Session, ToolContext};

/// Carries the per-run state shared across the agent loop, tools,
/// guardrails, and hooks.
///
/// `RunContext` does **not** implement `Default` — a context without a
/// session handle is meaningless. Construct via [`RunContext::new`].
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{
///     CancellationToken, ConversationSnapshot, HookRegistry, RunContext,
///     SequenceId, Session, SessionError, SessionEvent, TracerHandle,
/// };
///
/// struct NoopSession;
/// #[async_trait]
/// impl Session for NoopSession {
///     async fn append(&self, _: &[SessionEvent]) -> Result<(), SessionError> { Ok(()) }
///     async fn events(&self, _: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
///         Ok(Vec::new())
///     }
///     async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
///         Ok(ConversationSnapshot::default())
///     }
/// }
///
/// let _ctx: RunContext<()> = RunContext::new(
///     Arc::new(()),
///     Arc::new(NoopSession),
///     HookRegistry::<()>::new(),
///     TracerHandle::default(),
///     CancellationToken::new(),
/// );
/// ```
pub struct RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    user_ctx: Arc<Ctx>,
    session: Arc<dyn Session>,
    hooks: HookRegistry<Ctx>,
    tracer: TracerHandle,
    cancel: CancellationToken,
    /// Per-invocation execution policy, injected by a `Runner` (e.g.
    /// `TokioRunner`). This is the runner-injection channel, **not** general
    /// context state: it is deliberately NOT surfaced into [`ToolContext`] by
    /// [`RunContext::to_tool_context`]. `None` when an agent is run directly
    /// without a runner.
    run_config: Option<RunConfig>,
    /// Out-of-band carrier for the run's terminal structured [`crate::AgentError`].
    /// Written by [`crate::Agent::run`] at the moment of failure; read at the
    /// boundary by a [`crate::Runner`] / [`crate::RunResultStreaming`]. Like
    /// `run_config`, it is **not** projected into [`ToolContext`].
    failure: FailureSlot,
}

impl<Ctx> RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a new [`RunContext`].
    pub fn new(
        user_ctx: Arc<Ctx>,
        session: Arc<dyn Session>,
        hooks: HookRegistry<Ctx>,
        tracer: TracerHandle,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            user_ctx,
            session,
            hooks,
            tracer,
            cancel,
            run_config: None,
            failure: FailureSlot::new(),
        }
    }

    /// Borrow the user context.
    pub fn user_ctx(&self) -> &Arc<Ctx> {
        &self.user_ctx
    }
    /// Borrow the session handle.
    pub fn session(&self) -> &Arc<dyn Session> {
        &self.session
    }
    /// Borrow the hook registry.
    pub fn hooks(&self) -> &HookRegistry<Ctx> {
        &self.hooks
    }
    /// Borrow the tracer handle.
    pub fn tracer(&self) -> &TracerHandle {
        &self.tracer
    }
    /// Borrow the cancellation token.
    pub fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Borrow the per-invocation [`RunConfig`], if a runner installed one.
    pub fn run_config(&self) -> Option<&RunConfig> {
        self.run_config.as_ref()
    }

    /// Clone the handle to this run's [`FailureSlot`].
    ///
    /// A [`crate::Runner`] clones this **before** moving the context into
    /// [`crate::Agent::run`] (the same way it clones `cancel` / `session`), then
    /// reads the structured error after the run's event stream drains.
    pub fn failure_handle(&self) -> FailureSlot {
        self.failure.clone()
    }

    /// Install the per-invocation [`RunConfig`] (consuming builder). A
    /// [`crate::Runner`] calls this before [`crate::Agent::run`].
    pub fn with_run_config(mut self, config: RunConfig) -> Self {
        self.run_config = Some(config);
        self
    }

    /// Project the narrower [`ToolContext`] from this [`RunContext`].
    ///
    /// Tools receive `user_ctx`, `tracer`, and a **child** cancellation
    /// token. The child observes the parent's cancellation but tool-side
    /// `cancel()` calls only cancel the tool's subtree — they do not
    /// propagate back to the run.
    ///
    /// Tools do not see the session handle (the runner owns persistence)
    /// or the hook registry (hooks fire around tool invocations, not
    /// from inside).
    pub fn to_tool_context(&self) -> ToolContext<Ctx> {
        ToolContext::new(
            Arc::clone(&self.user_ctx),
            self.tracer.clone(),
            self.cancel.child_token(),
        )
    }
}

#[cfg(test)]
mod runcontext_tests {
    use super::*;
    use crate::{MemorySession, RunConfig};
    use std::sync::Arc;

    #[test]
    fn failure_handle_shares_the_context_slot() {
        use crate::AgentError;
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            crate::HookRegistry::new(),
            crate::TracerHandle::default(),
            crate::CancellationToken::new(),
        );
        let handle = ctx.failure_handle();
        handle.set(AgentError::MaxTurnsExceeded(2));
        // A second handle from the same ctx observes the write.
        assert!(matches!(
            ctx.failure_handle().take(),
            Some(AgentError::MaxTurnsExceeded(2))
        ));
    }

    #[test]
    fn with_run_config_round_trips_and_defaults_none() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        assert!(ctx.run_config().is_none());

        let ctx =
            ctx.with_run_config(RunConfig::new().with_timeout(std::time::Duration::from_secs(1)));
        assert_eq!(
            ctx.run_config().unwrap().timeout,
            Some(std::time::Duration::from_secs(1))
        );
    }
}

/// Re-export of [`tokio_util::sync::CancellationToken`] so downstream
/// crates need not depend on `tokio-util` directly.
pub use tokio_util::sync::CancellationToken;

/// Registry of hooks active for one run.
///
/// Today the surface is intentionally minimal — just `new`, `push`,
/// `iter`, and `is_empty`. The agent-loop ticket grows this when it
/// needs per-event filtering.
pub struct HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    hooks: Vec<Arc<dyn Hook<Ctx>>>,
}

impl<Ctx> HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Register a hook.
    pub fn push(&mut self, hook: Arc<dyn Hook<Ctx>>) {
        self.hooks.push(hook);
    }

    /// Iterate over registered hooks in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Hook<Ctx>>> {
        self.hooks.iter()
    }

    /// `true` if no hooks are registered.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

impl<Ctx> Default for HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Opaque handle to the per-run tracer.
///
/// Field shape lands with the observability ticket; today this is a
/// unit struct so signatures referring to `TracerHandle` resolve.
// SMA-3xx — gains real fields with the observability ticket.
#[derive(Debug, Clone, Default)]
pub struct TracerHandle {
    _private: (),
}
