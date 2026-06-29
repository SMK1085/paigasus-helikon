//! Shared test helpers for the `paigasus-helikon-runtime-axum` integration tests.

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};

/// A test [`Agent`] that emits a fixed sequence of events rather than
/// talking to any real model.
pub struct ScriptedAgent {
    /// Agent name returned by [`Agent::name`].
    pub name: String,
    /// Events to emit on each [`Agent::run`] call.
    pub events: Vec<AgentEvent>,
}

#[async_trait]
impl<Ctx: Send + Sync + 'static> Agent<Ctx> for ScriptedAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "scripted test agent"
    }

    async fn run(
        &self,
        _ctx: RunContext<Ctx>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        Ok(stream::iter(self.events.clone()).boxed())
    }
}

/// Returns a minimal event sequence: one assistant "echo" message followed by
/// [`AgentEvent::RunCompleted`].
pub fn echo_script() -> Vec<AgentEvent> {
    vec![
        AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: vec![ContentPart::Text {
                    text: "echo".to_owned(),
                }],
                agent: None,
            },
        },
        AgentEvent::RunCompleted {
            usage: TokenUsage::default(),
        },
    ]
}
