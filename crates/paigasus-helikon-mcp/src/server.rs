//! `McpAgentServer` — expose any `Agent<Ctx>` as an MCP server.

use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentInput, HookRegistry, MemorySession, RunConfig, RunContext, RunResultStreaming,
    Session, TracerHandle,
};
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, JsonObject, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool as McpToolDef,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, ServiceExt};

use crate::error::McpError;

type CtxFactory<Ctx> = Arc<dyn Fn() -> Ctx + Send + Sync>;

/// Serves one [`Agent`] as an MCP server exposing a single tool
/// (`{"input": string}` → the agent's final text output).
///
/// Run timeouts come from [`RunConfig::timeout`] (enforced here with
/// `tokio::time::timeout` — core's `collect()` has no timer), and the MCP
/// request's cancellation (client disconnect, `notifications/cancelled`)
/// propagates into the run's `CancellationToken`.
pub struct McpAgentServer<Ctx> {
    agent: Arc<dyn Agent<Ctx>>,
    ctx_factory: Option<CtxFactory<Ctx>>,
    name: String,
    version: String,
    instructions: Option<String>,
    run_config: RunConfig,
    tool_name: String,
}

impl<Ctx> McpAgentServer<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Wrap `agent`. A per-request context factory must be supplied via
    /// [`McpAgentServer::with_ctx`] before serving (or use
    /// [`McpAgentServer::with_default_ctx`]).
    pub fn new(agent: impl Agent<Ctx> + 'static) -> Self {
        let agent: Arc<dyn Agent<Ctx>> = Arc::new(agent);
        let tool_name = sanitize_tool_name(agent.name());
        Self {
            agent,
            ctx_factory: None,
            name: "paigasus-helikon-agent".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            instructions: None,
            run_config: RunConfig::default(),
            tool_name,
        }
    }

    /// Convenience for `Ctx: Default`: per-request contexts are `Ctx::default()`.
    pub fn with_default_ctx(agent: impl Agent<Ctx> + 'static) -> Self
    where
        Ctx: Default,
    {
        Self::new(agent).with_ctx(Ctx::default)
    }

    /// MCP server name (the `Implementation.name` reported at initialize).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// MCP server version.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Optional MCP `instructions` surfaced to connecting clients.
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Per-request user-context factory.
    pub fn with_ctx(mut self, factory: impl Fn() -> Ctx + Send + Sync + 'static) -> Self {
        self.ctx_factory = Some(Arc::new(factory));
        self
    }

    /// Run configuration for each request's agent run. `timeout` is enforced
    /// at this boundary; driver-scoped knobs pass through to the run.
    pub fn with_run_config(mut self, config: RunConfig) -> Self {
        self.run_config = config;
        self
    }

    fn handler(&self) -> Result<AgentMcpHandler<Ctx>, McpError> {
        let ctx_factory = self.ctx_factory.clone().ok_or_else(|| {
            McpError::Other(anyhow::anyhow!(
                "McpAgentServer needs a context factory: call .with_ctx(...) \
                 or construct via McpAgentServer::with_default_ctx"
            ))
        })?;
        Ok(AgentMcpHandler {
            agent: Arc::clone(&self.agent),
            ctx_factory,
            name: self.name.clone(),
            version: self.version.clone(),
            instructions: self.instructions.clone(),
            run_config: self.run_config.clone(),
            tool_name: self.tool_name.clone(),
        })
    }

    /// Serve over an arbitrary rmcp server transport. Blocks until the
    /// client disconnects. (Escape hatch; also used by in-process tests.)
    pub async fn serve_transport<T, E, A>(&self, transport: T) -> Result<(), McpError>
    where
        T: rmcp::transport::IntoTransport<RoleServer, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let handler = self.handler()?;
        let running = handler
            .serve(transport)
            .await
            .map_err(|e| McpError::Serve(anyhow::Error::new(e)))?;
        running
            .waiting()
            .await
            .map_err(|e| McpError::Serve(anyhow::Error::new(e)))?;
        Ok(())
    }

    /// Serve over stdio. Blocks until the client disconnects.
    pub async fn serve_stdio(&self) -> Result<(), McpError> {
        self.serve_transport(rmcp::transport::stdio()).await
    }

    /// Build a tower [`StreamableHttpService`] for this agent, suitable for
    /// nesting into any hyper/axum router. Sessions are managed in-process
    /// by a [`LocalSessionManager`]; each session gets a clone of the
    /// handler.
    pub fn streamable_http_service(
        &self,
    ) -> Result<StreamableHttpService<AgentMcpHandler<Ctx>, LocalSessionManager>, McpError> {
        let handler = self.handler()?;
        Ok(StreamableHttpService::new(
            move || Ok(handler.clone()),
            LocalSessionManager::default().into(),
            StreamableHttpServerConfig::default(),
        ))
    }

    /// Serve over streamable HTTP, binding `addr` (e.g. `"127.0.0.1:8080"`).
    /// The MCP endpoint is mounted at path `/mcp`. Blocks until the server
    /// task exits.
    pub async fn serve_streamable_http(&self, addr: &str) -> Result<(), McpError> {
        let service = self.streamable_http_service()?;
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| McpError::Bind {
                addr: addr.to_owned(),
                source,
            })?;
        let router = axum::Router::new().nest_service("/mcp", service);
        axum::serve(listener, router)
            .await
            .map_err(|e| McpError::Serve(anyhow::Error::new(e)))?;
        Ok(())
    }
}

/// MCP tool names must match `[a-zA-Z0-9_-]+`; anything else becomes `_`.
fn sanitize_tool_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "agent".to_owned()
    } else {
        s
    }
}

/// The JSON Schema for the single agent tool: `{"input": string}`.
fn input_schema() -> Arc<JsonObject> {
    let v = serde_json::json!({
        "type": "object",
        "properties": {
            "input": { "type": "string", "description": "The user message to send to the agent." }
        },
        "required": ["input"]
    });
    match v {
        serde_json::Value::Object(o) => Arc::new(o),
        _ => unreachable!("input schema literal is an object"),
    }
}

/// The rmcp [`ServerHandler`] behind [`McpAgentServer`]. One instance per
/// stdio connection; cloned per HTTP session. Public only because it appears
/// in the [`McpAgentServer::streamable_http_service`] return type — construct
/// it through [`McpAgentServer`], not directly.
pub struct AgentMcpHandler<Ctx> {
    agent: Arc<dyn Agent<Ctx>>,
    ctx_factory: CtxFactory<Ctx>,
    name: String,
    version: String,
    instructions: Option<String>,
    run_config: RunConfig,
    tool_name: String,
}

// Manual impl: `#[derive(Clone)]` would demand `Ctx: Clone`, but every field
// is clonable regardless of `Ctx` (the agent and factory are behind `Arc`).
impl<Ctx> Clone for AgentMcpHandler<Ctx> {
    fn clone(&self) -> Self {
        Self {
            agent: Arc::clone(&self.agent),
            ctx_factory: Arc::clone(&self.ctx_factory),
            name: self.name.clone(),
            version: self.version.clone(),
            instructions: self.instructions.clone(),
            run_config: self.run_config.clone(),
            tool_name: self.tool_name.clone(),
        }
    }
}

impl<Ctx: Send + Sync + 'static> ServerHandler for AgentMcpHandler<Ctx> {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(self.name.clone(), self.version.clone()));
        if let Some(instructions) = &self.instructions {
            info = info.with_instructions(instructions.clone());
        }
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tool = McpToolDef::new(
            self.tool_name.clone(),
            self.agent.description().to_owned(),
            input_schema(),
        );
        Ok(ListToolsResult {
            tools: vec![tool],
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if request.name != self.tool_name {
            return Err(ErrorData::invalid_params(
                format!("unknown tool: {}", request.name),
                None,
            ));
        }
        let input = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("input"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ErrorData::invalid_params(
                    "missing required string argument `input`".to_owned(),
                    None,
                )
            })?
            .to_owned();

        // The run's token is a child of rmcp's per-request token, so a client
        // disconnect or `notifications/cancelled` cancels the agent run.
        let cancel = context.ct.child_token();
        let run_ctx = RunContext::new(
            Arc::new((self.ctx_factory)()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::builder().build(),
            cancel.clone(),
        )
        .with_run_config(self.run_config.clone());

        let agent = Arc::clone(&self.agent);
        let run = async move {
            let stream = agent
                .run(run_ctx, AgentInput::from_user_text(input))
                .await
                .map_err(|e| e.to_string())?;
            RunResultStreaming::new(stream)
                .collect()
                .await
                .map_err(|e| e.to_string())
        };

        // Core has no timer; RunConfig::timeout is enforced at this boundary.
        let outcome = match self.run_config.timeout {
            Some(deadline) => match tokio::time::timeout(deadline, run).await {
                Ok(outcome) => outcome,
                Err(_elapsed) => {
                    cancel.cancel();
                    return Ok(CallToolResult::error(vec![Content::text(
                        "agent run timed out",
                    )]));
                }
            },
            None => run.await,
        };

        match outcome {
            Ok(result) => Ok(CallToolResult::success(vec![Content::text(
                result.final_output,
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "agent run failed: {e}"
            ))])),
        }
    }
}
