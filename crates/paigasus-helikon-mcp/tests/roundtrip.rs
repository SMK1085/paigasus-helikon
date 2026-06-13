//! AC2: an `LlmAgent` exposed via `McpAgentServer` is callable from another
//! MCP-aware client — here, our own `McpServerHandle` ("second Paigasus
//! instance"), full both-directions round-trip in-process.

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    CancellationToken, FinishReason, Instructions, LlmAgent, Model, ModelCapabilities, ModelError,
    ModelEvent, ModelRequest, ModelSettings, RunConfig,
};
use paigasus_helikon_mcp::{McpAgentServer, McpConnectOptions, McpServerHandle};

struct ScriptedModel {
    scripts: std::sync::Mutex<std::collections::VecDeque<Vec<ModelEvent>>>,
}

impl ScriptedModel {
    fn new(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: std::sync::Mutex::new(scripts.into()),
        })
    }
}

#[async_trait]
impl Model for ScriptedModel {
    async fn invoke(
        &self,
        _req: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let s = self.scripts.lock().unwrap().pop_front().unwrap_or_default();
        Ok(Box::pin(futures_util::stream::iter(s.into_iter().map(Ok))))
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    fn provider(&self) -> &str {
        "openai"
    }
    fn model(&self) -> &str {
        "gpt-4o"
    }
}

#[tokio::test]
async fn llm_agent_round_trips_through_both_halves() {
    let model = ScriptedModel::new(vec![vec![
        ModelEvent::TokenDelta { text: "42".into() },
        ModelEvent::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cached_input_tokens: None,
            reasoning_tokens: None,
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]]);

    // Mirror the field set from crates/paigasus-helikon/tests/otel_spans.rs —
    // it is the canonical LlmAgent construction example.
    let agent: LlmAgent<(), ScriptedModel> = LlmAgent {
        name: "answerer".into(),
        description: "answers questions".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools: Vec::new(),
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    };

    let server = McpAgentServer::with_default_ctx(agent)
        .name("paigasus-roundtrip")
        .version("0.0.1");

    // Serve over duplex; consume through our own client wrapper.
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let _ = server.serve_transport(server_io).await;
    });
    let handle = McpServerHandle::connect_transport(client_io, McpConnectOptions::new())
        .await
        .expect("connect to own server");

    let tools = handle.tools::<()>();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "answerer");

    let out = tools[0]
        .invoke(
            &support::tool_ctx(),
            serde_json::json!({"input": "meaning of life?"}),
        )
        .await
        .unwrap();
    assert_eq!(out.content, serde_json::json!("42"));
}
