//! Shared test helpers for the `paigasus-helikon-runtime-axum` integration tests.
//!
//! This module is compiled into every integration-test binary; not every helper
//! is used by every binary, so dead-code is allowed module-wide.
#![allow(dead_code)]

use std::{net::SocketAddr, sync::Arc};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt as _};
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};
use paigasus_helikon_runtime_axum::AgentServer;

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

/// Build an [`AgentServer`] mounting a single `echo` [`ScriptedAgent`], bind it
/// to an ephemeral loopback port, spawn the serve loop, and return the bound
/// address.
pub async fn spawn_echo_server() -> SocketAddr {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(ScriptedAgent {
            name: "echo".into(),
            events: echo_script(),
        }))
        .build()
        .expect("server builds");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        server
            .serve_with_listener(listener)
            .await
            .expect("serve loop");
    });

    addr
}

/// Parse the `data:` lines of a Server-Sent-Events body back into a
/// `Vec<AgentEvent>`, in order. Non-`data:` lines (blank separators, `event:`
/// type tags) are ignored.
pub fn parse_sse(text: &str) -> Vec<AgentEvent> {
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|data| !data.is_empty())
        .map(|data| serde_json::from_str::<AgentEvent>(data).expect("valid AgentEvent JSON"))
        .collect()
}
