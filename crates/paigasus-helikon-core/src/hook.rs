//! The [`Hook`] trait and its carrier types.
//!
//! Hooks intercept lifecycle events (`PreToolUse`, `PostToolUse`,
//! `OnTurnStart`, `OnHandoff`, …). They are *observation and side effects*
//! — distinct from permissions (authorization) and guardrails (content).

use async_trait::async_trait;

use crate::RunContext;

/// Lifecycle interceptor.
///
/// Hooks fire on the events listed in [`HookEvent`]. Each hook returns a
/// [`HookDecision`] that the runner honors before continuing.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{Hook, HookDecision, HookEvent, RunContext};
///
/// struct NoopHook;
///
/// #[async_trait]
/// impl Hook<()> for NoopHook {
///     async fn on_event(
///         &self,
///         _ctx: &RunContext<()>,
///         _event: &HookEvent,
///     ) -> HookDecision {
///         HookDecision::Allow
///     }
/// }
/// ```
#[async_trait]
pub trait Hook<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Fire on `event` and return a [`HookDecision`].
    async fn on_event(&self, ctx: &RunContext<Ctx>, event: &HookEvent) -> HookDecision;
}

/// A lifecycle event seen by a [`Hook`].
///
/// Variants mirror the Claude Agent SDK's hook taxonomy. Additional
/// variants land with the agent-loop ticket.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum HookEvent {
    /// Fired once at the start of a run.
    OnRunStart,
    /// Fired at the start of each turn.
    OnTurnStart {
        /// Zero-based turn index.
        turn: u32,
    },
    /// Fired just before a tool is invoked.
    PreToolUse {
        /// Tool name.
        tool: String,
        /// JSON arguments about to be passed.
        args: serde_json::Value,
    },
    /// Fired just after a tool returns.
    PostToolUse {
        /// Tool name.
        tool: String,
        /// JSON output the tool produced.
        output: serde_json::Value,
    },
    /// Fired at a handoff from one agent to another.
    OnHandoff {
        /// Outgoing agent name.
        from: String,
        /// Incoming agent name.
        to: String,
    },
    /// Fired once at the end of a run.
    OnRunComplete,
}

/// A [`Hook`]'s reply to a [`HookEvent`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum HookDecision {
    /// Allow the event to proceed unchanged.
    Allow,
    /// Block the event with a human-readable reason.
    Deny {
        /// Reason surfaced to the agent.
        reason: String,
    },
    /// Replace the input value the runner is about to use (e.g. sanitize
    /// `PreToolUse` arguments).
    ReplaceInput {
        /// Replacement value.
        value: serde_json::Value,
    },
    /// Replace the output value the runner just observed (e.g. redact
    /// `PostToolUse` output).
    ReplaceOutput {
        /// Replacement value.
        value: serde_json::Value,
    },
    /// Inject a system message into the next model call.
    InjectSystemMessage {
        /// Text to inject.
        text: String,
    },
}
