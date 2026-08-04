//! Shared HTTP wire-format conformance fixtures for the Helikon HTTP runtimes.
//!
//! This internal (never-published) crate hosts the agent set that the
//! cross-runtime parity suite in `tests/parity.rs` mounts on **both** the
//! `paigasus-helikon-runtime-axum` and `paigasus-helikon-runtime-actix` servers.
//! Because both runtimes are generic over the same core
//! [`Agent`] trait, one agent set drives both
//! servers, and the test then asserts that the two runtimes emit byte- and
//! structurally-identical HTTP responses.
//!
//! The lone public entry point is [`scripted_agents`].
#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};

/// A deterministic [`Agent`] that replays a fixed sequence of [`AgentEvent`]s
/// instead of talking to a real model, so every run is byte-reproducible.
struct ScriptedAgent {
    /// Agent name returned by [`Agent::name`].
    name: String,
    /// Human-readable description returned by [`Agent::description`].
    description: String,
    /// Events emitted, in order, on each [`Agent::run`] call.
    events: Vec<AgentEvent>,
}

#[async_trait]
impl Agent<()> for ScriptedAgent {
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
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        Ok(stream::iter(self.events.clone()).boxed())
    }
}

/// The shared echo agent set mounted on both runtimes by the parity suite.
///
/// Returns a single agent named `echo` whose every run emits one assistant
/// [`AgentEvent::MessageOutput`] carrying the text `"echo"` followed by a
/// terminal [`AgentEvent::RunCompleted`]. The events are fixed and carry no
/// per-run identifiers, so the resulting HTTP bodies differ between the axum and
/// actix runtimes only in the injected `run_id` — which the parity test
/// normalizes before its byte comparison.
///
/// The return type is `Vec<Arc<dyn Agent<()>>>` so the exact same values can be
/// handed to both `paigasus_helikon_runtime_axum::AgentServer::<()>` and
/// `paigasus_helikon_runtime_actix::AgentServer::<()>`, which share the core
/// [`Agent`] trait object.
pub fn scripted_agents() -> Vec<Arc<dyn Agent<()>>> {
    let echo = ScriptedAgent {
        name: "echo".to_owned(),
        description: "scripted echo agent".to_owned(),
        events: vec![
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
        ],
    };
    vec![Arc::new(echo)]
}
