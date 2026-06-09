//! `McpServerHandle` — connection handle to an external MCP server.

use std::sync::Arc;

use paigasus_helikon_core::Tool;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::ServiceExt;
use tokio_util::sync::CancellationToken;

use crate::client::search::SearchTool;
use crate::client::tool::McpTool;
use crate::error::McpError;

/// Options applied at connect time.
#[derive(Debug, Clone, Default)]
pub struct McpConnectOptions {
    pub(crate) lazy: bool,
    pub(crate) tool_prefix: Option<String>,
}

impl McpConnectOptions {
    /// Default options: eager schemas, no prefix.
    pub fn new() -> Self {
        Self::default()
    }

    /// Lazy mode: tools advertise placeholder schemas; a `search_tools`
    /// meta-tool serves the real schemas on demand. See the crate docs.
    pub fn lazy(mut self, lazy: bool) -> Self {
        self.lazy = lazy;
        self
    }

    /// Prefix prepended to every exposed tool name (and the `search_tools`
    /// meta-tool), to avoid collisions when one agent uses several servers.
    pub fn tool_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.tool_prefix = Some(prefix.into());
        self
    }
}

/// Which transport `connect()` should build.
enum TransportKind {
    Stdio(tokio::process::Command),
    ChildProcess(rmcp::transport::TokioChildProcess),
    StreamableHttp(rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig),
}

/// Builder returned by [`McpServerHandle::stdio`] /
/// [`McpServerHandle::child_process`] / [`McpServerHandle::streamable_http`].
pub struct McpConnectBuilder {
    kind: TransportKind,
    options: McpConnectOptions,
}

impl McpConnectBuilder {
    /// See [`McpConnectOptions::lazy`].
    pub fn lazy(mut self, lazy: bool) -> Self {
        self.options = self.options.lazy(lazy);
        self
    }

    /// See [`McpConnectOptions::tool_prefix`].
    pub fn tool_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.options = self.options.tool_prefix(prefix);
        self
    }

    /// Spawn/dial the transport, run the MCP `initialize` handshake, and
    /// fetch the tool list (one paginated `tools/list` sweep).
    pub async fn connect(self) -> Result<McpServerHandle, McpError> {
        match self.kind {
            TransportKind::Stdio(cmd) => {
                let transport = rmcp::transport::TokioChildProcess::new(cmd)?;
                McpServerHandle::connect_transport(transport, self.options).await
            }
            TransportKind::ChildProcess(transport) => {
                McpServerHandle::connect_transport(transport, self.options).await
            }
            TransportKind::StreamableHttp(config) => {
                let transport = rmcp::transport::StreamableHttpClientTransport::from_config(config);
                McpServerHandle::connect_transport(transport, self.options).await
            }
        }
    }
}

struct HandleInner {
    service: RunningService<RoleClient, ()>,
    tools: Vec<rmcp::model::Tool>,
    options: McpConnectOptions,
    cancel: CancellationToken,
}

impl Drop for HandleInner {
    fn drop(&mut self) {
        // Tear the connection task (and any child process) down with the
        // last handle clone.
        self.cancel.cancel();
    }
}

/// A live connection to an external MCP server. Cheap to clone; the
/// connection (and a stdio child process) lives until the last clone —
/// including clones held by the tools themselves — is dropped, or
/// [`McpServerHandle::close`] is called.
#[derive(Clone)]
pub struct McpServerHandle {
    inner: Arc<HandleInner>,
}

impl McpServerHandle {
    /// Spawn `command` as a child process speaking MCP over stdio.
    /// `configure` mutates the command (args, env, cwd) before spawning.
    pub fn stdio(
        mut command: tokio::process::Command,
        configure: impl FnOnce(&mut tokio::process::Command),
    ) -> McpConnectBuilder {
        configure(&mut command);
        McpConnectBuilder {
            kind: TransportKind::Stdio(command),
            options: McpConnectOptions::default(),
        }
    }

    /// Bring a fully configured [`rmcp::transport::TokioChildProcess`]
    /// (built via its `builder()`) for explicit lifecycle control.
    pub fn child_process(transport: rmcp::transport::TokioChildProcess) -> McpConnectBuilder {
        McpConnectBuilder {
            kind: TransportKind::ChildProcess(transport),
            options: McpConnectOptions::default(),
        }
    }

    /// Connect to a streamable-HTTP MCP server at `uri`. For auth headers or
    /// retry tuning, build an
    /// [`rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig`]
    /// and use [`McpServerHandle::streamable_http_with_config`].
    pub fn streamable_http(uri: impl Into<std::sync::Arc<str>>) -> McpConnectBuilder {
        let config =
            rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                uri.into(),
            );
        Self::streamable_http_with_config(config)
    }

    /// Streamable-HTTP with a caller-built transport config (auth header,
    /// custom headers, retry policy).
    pub fn streamable_http_with_config(
        config: rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig,
    ) -> McpConnectBuilder {
        McpConnectBuilder {
            kind: TransportKind::StreamableHttp(config),
            options: McpConnectOptions::default(),
        }
    }

    /// Escape hatch: connect over any rmcp client transport (used by tests
    /// for in-process duplex transports, and available for custom
    /// transports such as unix sockets).
    pub async fn connect_transport<T, E, A>(
        transport: T,
        options: McpConnectOptions,
    ) -> Result<Self, McpError>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let cancel = CancellationToken::new();
        let service =
            ().serve_with_ct(transport, cancel.clone())
                .await
                .map_err(|e| McpError::Connect(Box::new(e)))?;
        let tools = service.list_all_tools().await?;
        Ok(Self {
            inner: Arc::new(HandleInner {
                service,
                tools,
                options,
                cancel,
            }),
        })
    }

    /// The tools this server exposes, adapted to core's `Tool<Ctx>`.
    ///
    /// Synchronous: discovery already happened at connect. In lazy mode the
    /// returned tools advertise placeholder schemas and a `search_tools`
    /// meta-tool is appended.
    pub fn tools<Ctx>(&self) -> Vec<Arc<dyn Tool<Ctx>>>
    where
        Ctx: Send + Sync + 'static,
    {
        let lazy = self.inner.options.lazy;
        let mut out: Vec<Arc<dyn Tool<Ctx>>> = self
            .inner
            .tools
            .iter()
            .map(|t| Arc::new(McpTool::new(self.clone(), t, lazy)) as Arc<dyn Tool<Ctx>>)
            .collect();
        if lazy {
            out.push(Arc::new(SearchTool::new(self.clone())) as Arc<dyn Tool<Ctx>>);
        }
        out
    }

    /// Close the connection (cancels the rmcp task; kills a stdio child).
    /// Outstanding and subsequent tool calls fail with a transport error.
    pub fn close(&self) {
        self.inner.cancel.cancel();
    }

    /// Prefix a wire name with the configured tool prefix.
    pub(crate) fn prefixed(&self, wire_name: &str) -> String {
        match &self.inner.options.tool_prefix {
            Some(p) => format!("{p}{wire_name}"),
            None => wire_name.to_owned(),
        }
    }

    /// The cached remote tool descriptors (wire names, unprefixed).
    pub(crate) fn cached_tools(&self) -> &[rmcp::model::Tool] {
        &self.inner.tools
    }

    /// Issue a raw `tools/call` for `wire_name` (unprefixed).
    pub(crate) async fn call_tool_raw(
        &self,
        wire_name: &str,
        arguments: Option<rmcp::model::JsonObject>,
    ) -> Result<rmcp::model::CallToolResult, McpError> {
        let mut params = CallToolRequestParams::new(wire_name.to_owned());
        if let Some(args) = arguments {
            params = params.with_arguments(args);
        }
        Ok(self.inner.service.call_tool(params).await?)
    }
}
