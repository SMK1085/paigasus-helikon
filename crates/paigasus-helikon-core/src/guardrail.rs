//! The [`Guardrail`] trait and its carrier types.
//!
//! Guardrails validate input/output **in parallel** with the agent
//! (optimistic execution). When a tripwire fires, the run halts. See the
//! *Permissions, Guardrails & Hooks* concept page.

use async_trait::async_trait;

use crate::RunContext;

/// Input/output safety check that runs in parallel with the agent.
///
/// # Example
///
/// ```
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{
///     Guardrail, GuardrailError, GuardrailInput, GuardrailVerdict, RunContext,
/// };
///
/// struct NoopGuardrail;
///
/// #[async_trait]
/// impl Guardrail<()> for NoopGuardrail {
///     async fn check(
///         &self,
///         _ctx: &RunContext<()>,
///         _input: GuardrailInput<'_>,
///     ) -> Result<GuardrailVerdict, GuardrailError> {
///         Ok(GuardrailVerdict::Pass)
///     }
/// }
/// ```
#[async_trait]
pub trait Guardrail<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Inspect `input` and return a [`GuardrailVerdict`]. A `Tripwire`
    /// verdict halts the run.
    async fn check(
        &self,
        ctx: &RunContext<Ctx>,
        input: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError>;
}

/// What a [`Guardrail`] inspects.
///
/// Variants will grow alongside the wire-format ticket.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum GuardrailInput<'a> {
    /// User-supplied text entering the agent.
    UserText(&'a str),
    /// Model-emitted text leaving the agent.
    ModelOutput(&'a str),
}

/// The outcome of a [`Guardrail::check`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum GuardrailVerdict {
    /// All clear — the run continues.
    Pass,
    /// Tripwire fired — the run halts and the runner emits a corresponding
    /// agent event.
    Tripwire {
        /// Which kind of tripwire fired.
        kind: GuardrailKind,
        /// Free-form auxiliary information.
        info: serde_json::Value,
    },
}

/// The category of a fired tripwire.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum GuardrailKind {
    /// Input failed a policy check.
    InputPolicy,
    /// Output failed a policy check.
    OutputPolicy,
    /// Provider-specific or custom tripwire that does not map to a known
    /// variant.
    Other {
        /// Human-readable reason supplied by the guardrail.
        reason: String,
    },
}

/// Errors raised by [`Guardrail::check`] itself (distinct from a tripwire
/// firing — a tripwire is a *successful* verdict that halts the run).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GuardrailError {
    /// Escape hatch.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
