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

/// Fixture MCP server with four tools:
/// - `echo`   — read-only annotated; echoes `{"msg": string}` back as text.
/// - `boom`   — always returns an `is_error` result.
/// - `shape`  — returns `structured_content` `{"ok": true}`.
/// - `sleepy` — sleeps for a minute before answering (cancellation tests).
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

/// Schema for the argument-ignoring fixture tools (`boom`, `shape`, `sleepy`),
/// so the advertised shape matches the handler instead of borrowing `echo`'s.
fn empty_schema() -> std::sync::Arc<rmcp::model::JsonObject> {
    let v = serde_json::json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
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
        let boom = Tool::new("boom", "Always fails", empty_schema());
        let shape = Tool::new("shape", "Returns structured content", empty_schema());
        let sleepy = Tool::new("sleepy", "Sleeps for a minute", empty_schema());
        Ok(ListToolsResult {
            tools: vec![echo, boom, shape, sleepy],
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
            "sleepy" => {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                Ok(CallToolResult::success(vec![Content::text("woke")]))
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown tool {other}"),
                None,
            )),
        }
    }
}

/// Build a [`paigasus_helikon_core::ToolContext`] with a caller-supplied
/// cancellation token. Useful for cancel-behaviour tests.
pub fn tool_ctx_with_cancel(
    cancel: paigasus_helikon_core::CancellationToken,
) -> paigasus_helikon_core::ToolContext<()> {
    paigasus_helikon_core::RunContext::ephemeral(())
        .with_cancel(cancel)
        .to_tool_context()
}

/// Build a [`paigasus_helikon_core::ToolContext`] with a fresh, uncancelled
/// token. Convenience wrapper for tests that don't need cancel control.
pub fn tool_ctx() -> paigasus_helikon_core::ToolContext<()> {
    tool_ctx_with_cancel(paigasus_helikon_core::CancellationToken::new())
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
