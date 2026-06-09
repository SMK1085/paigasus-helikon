//! Shared test fixture: an in-process rmcp server over `tokio::io::duplex`.
// Not every test binary uses every item in this shared module.
#![allow(dead_code)]

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo, Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ErrorData;

/// Fixture MCP server with three tools:
/// - `echo`   — read-only annotated; echoes `{"msg": string}` back as text.
/// - `boom`   — always returns an `is_error` result.
/// - `shape`  — returns `structured_content` `{"ok": true}`.
#[derive(Clone, Default)]
pub struct FixtureServer;

fn echo_schema() -> std::sync::Arc<rmcp::model::JsonObject> {
    let v = serde_json::json!({
        "type": "object",
        "properties": { "msg": { "type": "string" } },
        "required": ["msg"]
    });
    match v {
        serde_json::Value::Object(o) => std::sync::Arc::new(o),
        _ => unreachable!(),
    }
}

impl ServerHandler for FixtureServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let echo = Tool::new("echo", "Echo a message back", echo_schema())
            .with_annotations(ToolAnnotations::new().read_only(true));
        let boom = Tool::new("boom", "Always fails", echo_schema());
        let shape = Tool::new("shape", "Returns structured content", echo_schema());
        Ok(ListToolsResult {
            tools: vec![echo, boom, shape],
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match request.name.as_ref() {
            "echo" => {
                let msg = request
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("msg"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("<missing>")
                    .to_owned();
                Ok(CallToolResult::success(vec![Content::text(msg)]))
            }
            "boom" => Ok(CallToolResult::error(vec![Content::text("kaboom")])),
            "shape" => Ok(CallToolResult::structured(serde_json::json!({"ok": true}))),
            other => Err(ErrorData::invalid_params(
                format!("unknown tool {other}"),
                None,
            )),
        }
    }
}

/// Connect a `McpServerHandle` to an in-process `FixtureServer` over duplex
/// pipes. The server task runs until the client closes.
pub async fn connect_fixture(
    options: paigasus_helikon_mcp::McpConnectOptions,
) -> paigasus_helikon_mcp::McpServerHandle {
    use rmcp::ServiceExt;
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        if let Ok(running) = FixtureServer.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    paigasus_helikon_mcp::McpServerHandle::connect_transport(client_io, options)
        .await
        .expect("fixture connect failed")
}
