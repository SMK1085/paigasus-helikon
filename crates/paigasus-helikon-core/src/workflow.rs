//! Deterministic workflow agents (SMA-325): [`SequentialAgent`],
//! [`ParallelAgent`], [`LoopAgent`].
//!
//! Each implements the same [`crate::Agent`] trait as `LlmAgent` and drives
//! sub-agents, merging their event streams with the handoff-driver convention:
//! swallow each child's `RunStarted`, fold `RunCompleted.usage` into a running
//! total, pass everything else through, and emit one outer `RunStarted` /
//! `RunCompleted`. Sub-agents coordinate through the run-scoped
//! [`crate::SessionState`]; each workflow agent auto-writes a child's final text
//! to `state[key]`.

use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;

use crate::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunConfig, RunContext, TokenUsage,
};

/// Concatenate the `ContentPart::Text` of an `Item::AssistantMessage`.
fn assistant_text(item: &Item) -> Option<String> {
    match item {
        Item::AssistantMessage { content, .. } => Some(
            content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// The effective `max_agent_depth` for a (sub-)run.
fn max_depth(run_config: Option<&RunConfig>) -> u32 {
    run_config
        .map(|c| c.max_agent_depth)
        .unwrap_or_else(|| RunConfig::default().max_agent_depth)
}

/// Runs sub-agents in order, threading the shared [`crate::SessionState`].
///
/// After each step completes, its final text is written to `state[key]` (key =
/// the agent's name, or an explicit key via [`SequentialAgent::then_keyed`]), so a
/// later step's dynamic `Instructions` closure can read it. Fail-fast on the first
/// step failure. The outer `RunCompleted` carries usage summed across all steps.
pub struct SequentialAgent<Ctx> {
    name: String,
    description: String,
    agents: Vec<(String, Arc<dyn Agent<Ctx>>)>,
}

impl<Ctx> SequentialAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct an empty sequence.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            agents: Vec::new(),
        }
    }

    /// Append a step keyed by the agent's own name.
    pub fn then(mut self, agent: impl Agent<Ctx> + 'static) -> Self {
        let key = agent.name().to_owned();
        self.agents.push((key, Arc::new(agent)));
        self
    }

    /// Append a step with an explicit state key (use when a name would collide).
    pub fn then_keyed(mut self, key: impl Into<String>, agent: impl Agent<Ctx> + 'static) -> Self {
        self.agents.push((key.into(), Arc::new(agent)));
        self
    }

    /// Append a pre-wrapped step keyed by the agent's name.
    pub fn then_shared(mut self, agent: Arc<dyn Agent<Ctx>>) -> Self {
        let key = agent.name().to_owned();
        self.agents.push((key, agent));
        self
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for SequentialAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }

    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let name = self.name.clone();
        let agents = self.agents.clone();

        let stream = async_stream::stream! {
            let parent_failure = ctx.failure_handle();
            yield AgentEvent::RunStarted { agent: name.clone() };

            let max = max_depth(ctx.run_config());
            if ctx.agent_depth() + 1 > max {
                let err = AgentError::MaxAgentDepthExceeded { depth: ctx.agent_depth() + 1, max };
                let msg = err.to_string();
                parent_failure.set(err);
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            let mut total = TokenUsage::default();
            for (key, agent) in &agents {
                let child = ctx.subagent_child();
                let failure = child.failure_handle();
                yield AgentEvent::AgentUpdated { agent: agent.name().to_owned() };

                let mut sub = match agent.run(child, input.clone()).await {
                    Ok(s) => s,
                    Err(e) => {
                        let msg = e.to_string();
                        parent_failure.set(e);
                        yield AgentEvent::RunFailed { error: msg };
                        return;
                    }
                };

                let mut last_text = String::new();
                let mut failed = false;
                while let Some(ev) = sub.next().await {
                    match ev {
                        AgentEvent::RunStarted { .. } => {}
                        AgentEvent::RunCompleted { usage } => total.add(usage),
                        AgentEvent::RunFailed { error } => {
                            failed = true;
                            yield AgentEvent::RunFailed { error };
                        }
                        AgentEvent::MessageOutput { item } => {
                            if let Some(t) = assistant_text(&item) {
                                last_text = t;
                            }
                            yield AgentEvent::MessageOutput { item };
                        }
                        other => yield other,
                    }
                }

                if failed {
                    if let Some(e) = failure.take() {
                        parent_failure.set(e);
                    }
                    return;
                }
                ctx.state().set(key.clone(), last_text);
            }

            yield AgentEvent::RunCompleted { usage: total };
        };

        Ok(Box::pin(stream))
    }
}
