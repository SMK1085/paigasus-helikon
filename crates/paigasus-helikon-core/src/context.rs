//! Run-scoped context types.
//!
//! [`RunContext`] carries user data, session handle, hook registry, tracer,
//! and cancellation token across the agent loop. The field shape lands with
//! the agent-loop ticket — today the type exists so the trait signatures
//! that reference it resolve.

use std::marker::PhantomData;

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
