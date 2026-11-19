//! Turn-boundary checkpoint: a host hook that runs between the tool results
//! of one turn and the model call of the next turn.

use async_trait::async_trait;

use crate::{ContentPart, RunContext};

/// Where the loop is when it calls the checkpoint.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnBoundary {
    /// The turn whose tool results the loop just recorded (0-based, per loop).
    pub completed_turn: u32,
    /// The turn that the next model call starts.
    pub next_turn: u32,
    /// The name of the agent whose loop is at the boundary (the handoff
    /// target in a child loop).
    pub agent: String,
    /// The [`RunContext::agent_depth`] of that loop.
    pub depth: u32,
}

impl TurnBoundary {
    /// Make a boundary value. This is public so that hosts can unit-test
    /// their own checkpoint.
    pub fn new(completed_turn: u32, next_turn: u32, agent: impl Into<String>, depth: u32) -> Self {
        Self {
            completed_turn,
            next_turn,
            agent: agent.into(),
            depth,
        }
    }
}

/// What the loop does after the checkpoint.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum CheckpointDecision {
    /// Call the model as normal.
    Continue,
    /// Add these parts as one user message, then call the model. The parts
    /// must be user parts (`Text`, `Image` or `Audio`). An empty list is the
    /// same as `Continue`.
    Inject(Vec<ContentPart>),
    /// End the run here with a stopped stop reason.
    Stop {
        /// The reason that the run reports.
        reason: String,
    },
}

/// A host hook that the agent loop calls at each turn boundary.
///
/// A turn boundary is the point after the tool results of turn `n-1` are
/// recorded and before the model call of turn `n`. The first model call, a
/// structured-output final turn, a repair attempt and a turn that ends in a
/// final answer or a handoff are not boundaries. The loop does not call the
/// checkpoint when the next turn would exceed the maximum number of turns.
///
/// # Rules
///
/// 1. **Commit rule.** The run owns injected content only after the
///    `UserMessageInjected` event is in the event stream. Before that event
///    the host owns the content. After it, the run can still end before the
///    model sees the content (for example a hook denial, a model error, a
///    cancel or a timeout). The host must not inject that content again.
/// 2. **Trust.** Injected content has user-role trust. A host that injects a
///    message from a third party gives that message more trust than a tool
///    result has.
/// 3. **Do not write to the session.** Do not append to `ctx.session()` from
///    the callback. The recorder appends the events of the run when the run
///    ends, so an append from the callback lands before the events of the run.
/// 4. **`Stop` is best effort.** A run that ends with a final answer has no
///    next boundary, so a `Stop` decision can never apply to it.
/// 5. **Time counts.** The time that `at_boundary` waits counts against the
///    run timeout. The loop has no separate timeout for the callback.
/// 6. **Cancel safety.** If the runner drops the stream (cancel or timeout)
///    while `at_boundary` waits, the future is dropped. `at_boundary` must be
///    cancel-safe.
/// 7. **Do not cancel from the callback** to stop the run. A cancel ends the
///    run as cancelled, which is the hard stop that `Stop` replaces. Return
///    `Stop` instead.
///
/// # Guardrails and their limits
///
/// The loop runs the input guardrails on the text of injected content. The
/// guardrails run even when the content has no `Text` part. A guardrail
/// cannot check image or audio parts, because a guardrail input holds only
/// text. If a guardrail trips (or returns an error), the loop does not store
/// the content and ends the run with a stopped stop reason. The reason text
/// alone does not identify this case, because a host can use the same text.
/// The reliable signal is a `GuardrailTriggered` event before the stop event,
/// with no `UserMessageInjected` event between them.
///
/// # Persistence limit
///
/// The runner stores the events of the run at the end of the run, on a best
/// effort basis. If that append fails, the injected content is lost even
/// though `UserMessageInjected` was in the stream. A host that needs a
/// guarantee must check the stored session.
#[async_trait]
pub trait TurnCheckpoint<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Decide what the loop does at this turn boundary.
    async fn at_boundary(
        &self,
        ctx: &RunContext<Ctx>,
        boundary: &TurnBoundary,
    ) -> CheckpointDecision;
}
