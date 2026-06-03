//! Handoff carrier types.
//!
//! A [`Handoff`] is a candidate agent an [`crate::LlmAgent`] may transfer the
//! conversation to. When the agent's `handoffs` list is non-empty, the loop
//! injects a synthetic `transfer_to_<slug>` tool per handoff; a model call to
//! one switches the active agent (see the agent-loop driver).

use std::sync::Arc;

use crate::Agent;

/// A candidate agent the conversation may be transferred to.
///
/// Constructed via [`Handoff::to`] (owned agent) or [`Handoff::shared`]
/// (pre-wrapped trait object). This is intentionally a thin wrapper around
/// `Arc<dyn Agent<Ctx>>`; it is the named home for future per-edge config
/// (tool-name override, transcript input-filter).
pub struct Handoff<Ctx> {
    agent: Arc<dyn Agent<Ctx>>,
}

impl<Ctx> Clone for Handoff<Ctx> {
    fn clone(&self) -> Self {
        Self {
            agent: Arc::clone(&self.agent),
        }
    }
}

impl<Ctx> Handoff<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Transfer target from an owned agent (wrapped in `Arc`).
    pub fn to(agent: impl Agent<Ctx> + 'static) -> Self {
        Self {
            agent: Arc::new(agent),
        }
    }

    /// Transfer target from a pre-wrapped trait object.
    pub fn shared(agent: Arc<dyn Agent<Ctx>>) -> Self {
        Self { agent }
    }

    /// The target agent.
    pub fn agent(&self) -> &Arc<dyn Agent<Ctx>> {
        &self.agent
    }

    /// Project the pure-data [`HandoffDef`] the state machine consumes.
    pub fn to_def(&self) -> HandoffDef {
        HandoffDef {
            tool_name: format!("transfer_to_{}", slug(self.agent.name())),
            target: self.agent.name().to_owned(),
            description: self.agent.description().to_owned(),
        }
    }
}

/// Pure-data description of one injected `transfer_to_*` tool.
///
/// Built by the agent-loop driver from each [`Handoff`] before the run, and
/// passed into [`crate::TransitionCtx`] so the pure state machine can both
/// advertise the transfer tools and recognize a call to one.
#[derive(Debug, Clone)]
pub struct HandoffDef {
    /// The synthetic tool name the model sees, `transfer_to_<slug>`.
    pub tool_name: String,
    /// The **real** target agent name (used in events and target lookup).
    pub target: String,
    /// The target agent's description (shown to the model).
    pub description: String,
}

/// Lowercase `name`, collapsing every run of non-ASCII-alphanumeric characters
/// to a single `_`, with leading/trailing `_` trimmed. `"Investing specialist"`
/// → `"investing_specialist"`. Non-ASCII characters (accents, CJK, …) are
/// treated as non-alphanumeric and collapsed, so an all-non-ASCII name slugs to
/// the empty string (yielding `transfer_to_`); the driver's collision check
/// still rejects two such names loudly rather than silently mis-routing.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_underscore = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    out.trim_matches('_').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentEvent, AgentInput, RunContext};
    use async_trait::async_trait;
    use futures_core::stream::BoxStream;

    struct NamedAgent {
        name: String,
        description: String,
    }

    #[async_trait]
    impl Agent<()> for NamedAgent {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            &self.description
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, crate::AgentError> {
            Ok(Box::pin(futures_util::stream::empty()))
        }
    }

    #[test]
    fn slug_sanitizes_names() {
        assert_eq!(slug("Investing specialist"), "investing_specialist");
        assert_eq!(slug("AML cytogenetics"), "aml_cytogenetics");
        assert_eq!(slug("budgeting"), "budgeting");
        assert_eq!(slug("  weird !! name  "), "weird_name");
    }

    #[test]
    fn to_def_derives_tool_name_target_and_description() {
        let h = Handoff::to(NamedAgent {
            name: "Investing specialist".to_owned(),
            description: "Handles investing questions.".to_owned(),
        });
        let def = h.to_def();
        assert_eq!(def.tool_name, "transfer_to_investing_specialist");
        assert_eq!(def.target, "Investing specialist");
        assert_eq!(def.description, "Handles investing questions.");
    }
}
