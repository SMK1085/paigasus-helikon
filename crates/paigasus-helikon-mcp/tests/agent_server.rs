//! Agent-as-MCP-server tests over an in-process duplex transport.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};
use paigasus_helikon_mcp::McpAgentServer;
use rmcp::model::CallToolRequestParams;
use rmcp::ServiceExt;

/// Agent that (optionally after a delay) replies with a fixed string.
/// Honors cancellation during the delay.
struct ScriptedAgent {
    reply: String,
    delay: Option<Duration>,
}

#[async_trait]
impl Agent<()> for ScriptedAgent {
    fn name(&self) -> &str {
        "triage helper" // note: space — exercises tool-name sanitization
    }
    fn description(&self) -> &str {
        "test agent"
    }
    async fn run(
        &self,
        ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let reply = self.reply.clone();
        let delay = self.delay;
        let cancel = ctx.cancel().clone();
        Ok(Box::pin(async_stream::stream! {
            yield AgentEvent::RunStarted { agent: "triage helper".into() };
            if let Some(d) = delay {
                tokio::select! {
                    _ = tokio::time::sleep(d) => {},
                    _ = cancel.cancelled() => {
                        yield AgentEvent::RunFailed { error: "cancelled".into() };
                        return;
                    }
                }
            }
            yield AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text { text: reply.clone() }],
                    agent: Some("triage helper".into()),
                },
            };
            yield AgentEvent::RunCompleted { usage: TokenUsage::default() };
        }))
    }
}

/// Serve `server` over duplex and return a connected raw rmcp client.
async fn connect_client(
    server: McpAgentServer<()>,
) -> rmcp::service::RunningService<rmcp::service::RoleClient, ()> {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let _ = server.serve_transport(server_io).await;
    });
    ().serve(client_io).await.expect("client connect")
}

#[tokio::test]
async fn lists_one_sanitized_tool() {
    let server = McpAgentServer::new(ScriptedAgent {
        reply: "ok".into(),
        delay: None,
    })
    .name("paigasus-test")
    .version("0.0.1")
    .with_ctx(|| ());

    let client = connect_client(server).await;
    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "triage_helper"); // space sanitized to underscore
    assert_eq!(tools[0].description.as_deref(), Some("test agent"));
}

#[tokio::test]
async fn call_tool_runs_agent_and_returns_final_text() {
    let server = McpAgentServer::new(ScriptedAgent {
        reply: "the answer".into(),
        delay: None,
    })
    .with_ctx(|| ());

    let client = connect_client(server).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("triage_helper").with_arguments(
                serde_json::json!({"input": "question"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let text = result.content[0].as_text().unwrap().text.clone();
    assert_eq!(text, "the answer");
}

#[tokio::test]
async fn ctx_factory_runs_per_request() {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let server = McpAgentServer::new(ScriptedAgent {
        reply: "ok".into(),
        delay: None,
    })
    .with_ctx(|| {
        COUNT.fetch_add(1, Ordering::SeqCst);
    });

    let client = connect_client(server).await;
    for _ in 0..2 {
        client
            .call_tool(
                CallToolRequestParams::new("triage_helper").with_arguments(
                    serde_json::json!({"input": "q"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();
    }
    assert_eq!(COUNT.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn unknown_tool_is_a_protocol_error() {
    let server = McpAgentServer::new(ScriptedAgent {
        reply: "ok".into(),
        delay: None,
    })
    .with_ctx(|| ());
    let client = connect_client(server).await;
    let err = client
        .call_tool(
            CallToolRequestParams::new("nope").with_arguments(
                serde_json::json!({"input": "q"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("nope") || msg.to_lowercase().contains("unknown"));
}

#[tokio::test]
async fn missing_input_is_a_protocol_error() {
    let server = McpAgentServer::new(ScriptedAgent {
        reply: "ok".into(),
        delay: None,
    })
    .with_ctx(|| ());
    let client = connect_client(server).await;
    let err = client
        .call_tool(CallToolRequestParams::new("triage_helper"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("input"));
}

/// Agent whose run stream reports failure.
struct FailingAgent;

#[async_trait]
impl Agent<()> for FailingAgent {
    fn name(&self) -> &str {
        "failer"
    }
    fn description(&self) -> &str {
        "always fails"
    }
    async fn run(
        &self,
        _ctx: RunContext<()>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        Ok(Box::pin(async_stream::stream! {
            yield AgentEvent::RunFailed { error: "model exploded".into() };
        }))
    }
}

#[tokio::test]
async fn run_failure_surfaces_as_is_error() {
    let server = McpAgentServer::with_default_ctx(FailingAgent);
    let client = connect_client(server).await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("failer").with_arguments(
                serde_json::json!({"input": "q"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    let text = result.content[0].as_text().unwrap().text.clone();
    assert!(text.contains("model exploded"));
}

/// Agent with a non-`Default` `Ctx`, to prove the factory requirement.
struct NeedsCtxAgent;

#[async_trait]
impl Agent<String> for NeedsCtxAgent {
    fn name(&self) -> &str {
        "needs-ctx"
    }
    fn description(&self) -> &str {
        "agent over String ctx"
    }
    async fn run(
        &self,
        _ctx: RunContext<String>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        Ok(Box::pin(futures_util::stream::empty()))
    }
}

#[tokio::test]
async fn missing_ctx_factory_errors_at_serve() {
    let server: McpAgentServer<String> = McpAgentServer::new(NeedsCtxAgent);
    let (_client_io, server_io) = tokio::io::duplex(1024);
    let err = server.serve_transport(server_io).await.unwrap_err();
    assert!(err.to_string().contains("context factory"), "got: {err}");
}

#[tokio::test]
async fn non_string_input_is_a_protocol_error() {
    let server = McpAgentServer::new(ScriptedAgent {
        reply: "ok".into(),
        delay: None,
    })
    .with_ctx(|| ());
    let client = connect_client(server).await;
    let err = client
        .call_tool(
            CallToolRequestParams::new("triage_helper").with_arguments(
                serde_json::json!({"input": 42})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("input") && msg.contains("string"),
        "got: {msg}"
    );
}
