//! Run-scoped context types.
//!
//! [`RunContext`] carries user data, session handle, hook registry, tracer,
//! and cancellation token across the agent loop. The field shape lands with
//! the agent-loop ticket — today the type exists so the trait signatures
//! that reference it resolve.

use std::marker::PhantomData;
use std::sync::Arc;

use crate::Hook;

/// Carries user context, session handle, hook registry, tracer, and
/// cancellation token across one run of the agent loop.
///
/// Field shape lands with the agent-loop ticket.
///
/// # Example
///
/// ```
/// use paigasus_helikon_core::RunContext;
///
/// let _ctx: RunContext<()> = RunContext::new();
/// ```
pub struct RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a bare [`RunContext`].
    ///
    /// The constructor signature will grow alongside the type's fields in
    /// the agent-loop ticket. Code that needs an empty context today should
    /// use this method rather than relying on struct-literal syntax (the
    /// type is `#[non_exhaustive]`-equivalent because all fields are
    /// private).
    pub fn new() -> Self {
        Self { _ctx: PhantomData }
    }
}

impl<Ctx> Default for RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
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
