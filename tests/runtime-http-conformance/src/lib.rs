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

/// What a [`ScriptedAgent`] does when run.
///
/// Three behaviours are needed because the parity suite must reach three
/// server code paths: the normal terminal path, the run-start error path
/// (redaction), and the never-terminates path (in-flight cap).
enum Behaviour {
    /// Emit a fixed event sequence, then end the stream.
    Script(Vec<AgentEvent>),
    /// Fail before emitting anything at all.
    FailToStart,
    /// Never terminate; the runner's cancel token is the only way out.
    Hang,
}

/// A deterministic [`Agent`] that replays a fixed behaviour instead of talking
/// to a real model, so every run is byte-reproducible.
struct ScriptedAgent {
    /// Agent name returned by [`Agent::name`].
    name: String,
    /// Human-readable description returned by [`Agent::description`].
    description: String,
    /// What this agent does on each [`Agent::run`] call.
    behaviour: Behaviour,
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
        match &self.behaviour {
            Behaviour::Script(events) => Ok(stream::iter(events.clone()).boxed()),
            // `TokioRunner::run_streamed` does `agent.run(..).await?`, and
            // `RunError: From<AgentError>`, so the server records a
            // `start_error` of exactly "agent failed: max turns (1) exceeded".
            // The redaction assertions grep for that substring's absence.
            Behaviour::FailToStart => Err(AgentError::MaxTurnsExceeded(1)),
            // `TokioRunner::controlled` selects on the cancel token, so the
            // agent itself need not handle cancellation.
            Behaviour::Hang => Ok(stream::pending().boxed()),
        }
    }
}

/// The shared agent set mounted on both runtimes by the parity suite.
///
/// - `echo` — emits one assistant [`AgentEvent::MessageOutput`] carrying the
///   text `"echo"` followed by a terminal [`AgentEvent::RunCompleted`]. The
///   events carry no per-run identifiers, so bodies differ between runtimes
///   only in the injected `run_id`, which the parity test normalizes.
/// - `boom` — fails before emitting any event, exercising the redacted 500 and
///   the redacted synthetic SSE/WebSocket terminal frames.
/// - `hang` — never produces a terminal event, so a run of it holds an
///   in-flight slot until cancelled. Used to drive the admission cap.
///
/// The return type is `Vec<Arc<dyn Agent<()>>>` so the same values can be handed
/// to both `paigasus_helikon_runtime_axum::AgentServer::<()>` and
/// `paigasus_helikon_runtime_actix::AgentServer::<()>`.
pub fn scripted_agents() -> Vec<Arc<dyn Agent<()>>> {
    vec![
        Arc::new(ScriptedAgent {
            name: "echo".to_owned(),
            description: "scripted echo agent".to_owned(),
            behaviour: Behaviour::Script(vec![
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
            ]),
        }),
        Arc::new(ScriptedAgent {
            name: "boom".to_owned(),
            description: "scripted agent that fails to start".to_owned(),
            behaviour: Behaviour::FailToStart,
        }),
        Arc::new(ScriptedAgent {
            name: "hang".to_owned(),
            description: "scripted agent that never terminates".to_owned(),
            behaviour: Behaviour::Hang,
        }),
    ]
}
