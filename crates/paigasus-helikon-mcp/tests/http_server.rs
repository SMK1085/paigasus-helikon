//! Streamable-HTTP end-to-end: bind a real port, connect with rmcp's
//! reqwest-backed client transport.

use std::time::Duration;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};
use paigasus_helikon_mcp::McpAgentServer;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;

struct EchoAgent;

#[async_trait]
impl Agent<()> for EchoAgent {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "echoes"
    }

    async fn run(
        &self,
        _ctx: RunContext<()>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        // Echo the first user text back.
        let text = match input.messages.first() {
            Some(Item::UserMessage { content }) => content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        };
        Ok(Box::pin(async_stream::stream! {
            yield AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text { text }],
                    agent: Some("echo".into()),
                },
            };
            yield AgentEvent::RunCompleted { usage: TokenUsage::default() };
        }))
    }
}

/// reqwest internally needs the multi-thread tokio runtime for its async I/O.
#[tokio::test(flavor = "multi_thread")]
async fn serves_streamable_http_end_to_end() {
    // Avoid the bind-drop-rebind race: hold the listener and use the service
    // escape hatch + axum directly, mirroring serve_streamable_http's wiring.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = McpAgentServer::with_default_ctx(EchoAgent)
        .name("http-test")
        .version("0.0.1");
    let service = server.streamable_http_service().expect("service");
    let router = axum::Router::new().nest_service("/mcp", service);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let transport = StreamableHttpClientTransport::from_uri(format!("http://{addr}/mcp"));
    let client = ().serve(transport).await.expect("http connect");
    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools[0].name, "echo");

    let result = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("echo").with_arguments(
                serde_json::json!({"input": "ping"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(result.content[0].as_text().unwrap().text, "ping");
    let _ = tokio::time::timeout(Duration::from_secs(3), client.cancel()).await;
}

/// AWS Bedrock AgentCore's MCP protocol injects its own platform-generated
/// `Mcp-Session-Id` header on every request — an id this server has never
/// issued. rmcp's default (stateful) mode would 404 on that unrecognized
/// session, but `stateful_mode: false` never reads the header at all: each
/// request is served directly, without a preceding `initialize` handshake.
/// This posts a raw `tools/list` JSON-RPC request carrying such a header
/// directly (bypassing rmcp's typed client, which would otherwise manage
/// the session lifecycle itself) and asserts the response is 200-class, not
/// a 4xx rejection.
#[tokio::test(flavor = "multi_thread")]
async fn stateless_streamable_http_accepts_unknown_session_id() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = McpAgentServer::with_default_ctx(EchoAgent)
        .name("stateless-test")
        .version("0.0.1");
    // `StreamableHttpServerConfig` is `#[non_exhaustive]`, so external crates
    // must go through `Default::default()` + the `with_*` builder methods
    // rather than struct-literal syntax.
    let config =
        rmcp::transport::StreamableHttpServerConfig::default().with_legacy_session_mode(false);
    let service = server
        .streamable_http_service_with(config)
        .expect("service");
    let router = axum::Router::new().nest_service("/mcp", service);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let response = reqwest::Client::new()
        .post(format!("http://{addr}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", "platform-generated-id-0123456789abcdef")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .send()
        .await
        .expect("request sent");

    assert!(
        response.status().is_success(),
        "expected a 200-class response for an unknown pre-set session id in \
         stateless mode, got: {}",
        response.status()
    );
    let body = response.text().await.expect("response body");
    assert!(
        body.contains("\"echo\""),
        "expected the tools/list result to include the echo tool, got: {body}"
    );
}

#[tokio::test]
async fn serve_streamable_http_bind_failure_returns_bind_error() {
    let server = McpAgentServer::with_default_ctx(EchoAgent)
        .name("x")
        .version("0");
    let err = server
        .serve_streamable_http("not-an-addr:0")
        .await
        .unwrap_err();
    assert!(
        matches!(err, paigasus_helikon_mcp::McpError::Bind { .. }),
        "expected McpError::Bind, got: {err:?}"
    );
}
