//! Run-scoped context types.
//!
//! [`RunContext`] carries user data, session handle, hook registry, tracer,
//! and cancellation token across the agent loop.

use std::sync::Arc;

use crate::{Hook, Session, ToolContext};

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
