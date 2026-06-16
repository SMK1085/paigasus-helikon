# MCP Integration

The `paigasus-helikon-mcp` crate (facade feature `mcp`, re-exported as `paigasus_helikon::mcp`) wraps [`rmcp`](https://crates.io/crates/rmcp), the official Rust MCP SDK, in both directions:

- **Client** — connect to an external MCP server and re-expose its tools to an agent as core [`Tool<Ctx>`](./tools.md) implementations.
- **Server** — wrap any `Agent<Ctx>` and serve it as a single MCP tool.

Two transports are supported: a stdio child process and streamable HTTP. SSE transports are not supported — rmcp removed them in `0.11.0` and the 2025-03-26 MCP spec revision deprecated HTTP+SSE in favor of streamable HTTP.

The whole crate is published on [crates.io](https://crates.io/crates/paigasus-helikon-mcp) / [docs.rs](https://docs.rs/paigasus-helikon-mcp). There is no discovery sugar on the agent builder yet: you connect explicitly, then hand the resulting `Vec<Arc<dyn Tool<Ctx>>>` to `LlmAgent::builder().tools(...)` like any other tool list.

## Client: external tools into an agent

`McpServerHandle` is the connection handle. Construct it with one of three transport entry points, each returning an `McpConnectBuilder`:

- `McpServerHandle::stdio(command, configure)` — spawn a child process speaking MCP over stdio. `configure` is an `FnOnce(&mut tokio::process::Command)` that sets args, env, and cwd before the spawn.
- `McpServerHandle::child_process(transport)` — bring a fully built `rmcp::transport::TokioChildProcess` for explicit lifecycle control.
- `McpServerHandle::streamable_http(uri)` — dial a streamable-HTTP server. For auth headers or retry tuning, build a `StreamableHttpClientTransportConfig` and use `McpServerHandle::streamable_http_with_config`.

`McpConnectBuilder` carries two options — `.tool_prefix(prefix)` and `.lazy(bool)` (see below) — and `.connect()` runs the MCP `initialize` handshake and fetches the tool list in one paginated sweep.

```rust,no_run
# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
use paigasus_helikon_mcp::McpServerHandle;

let fs = McpServerHandle::stdio(tokio::process::Command::new("npx"), |cmd| {
    cmd.args(["-y", "@modelcontextprotocol/server-filesystem", "/data"]);
})
.tool_prefix("fs_")
.connect()
.await?;

// Discovery already happened at connect, so `tools()` is synchronous.
let tools = fs.tools::<()>(); // Vec<Arc<dyn Tool<()>>>
// .tools(tools) on an LlmAgent::builder, then run the agent.
# let _ = tools;
# Ok(())
# }
```

`tools::<Ctx>()` adapts each remote tool to a `McpTool<Ctx>`. `Ctx` is a phantom — MCP tools never read the user context, so one handle serves agents of any context type. Each adapted tool reports the remote tool's name (with the configured prefix), description, input schema, and an effect derived from the server's annotations: a `read_only_hint == true` becomes `ToolEffect::ReadOnly`, everything else `ToolEffect::SideEffect`. Server-declared annotations are untrusted metadata, so `ToolEffect::Write` is never produced — an MCP tool can never unlock `AcceptEdits` auto-approval (see [Permissions, Guardrails & Hooks](./permissions-guardrails-hooks.md)).

When invoked, a tool issues an MCP `tools/call` and maps the result into a `ToolOutput`: a server's `structured_content` is passed through as-is; a single text result becomes a JSON string; multiple content blocks become a JSON array. An MCP error result (`is_error`) surfaces as `ToolError`. Calls race the run's `CancellationToken`, so a hung server can't outlive a cancelled agent run.

`McpServerHandle` is cheap to clone; the connection (and any stdio child process) lives until the last clone is dropped — including the clones held inside the tools themselves — or `handle.close()` is called.

### Lazy mode

`McpConnectOptions::new().lazy(true)` (via the builder's `.lazy(true)`) trades eager schemas for a smaller initial prompt. In lazy mode every adapted tool advertises a placeholder schema (`{"type": "object", "additionalProperties": true}`) and an extra `search_tools` meta-tool is appended to the returned list. `search_tools` takes `{"query": string}`, matches the query case-insensitively against the cached tool names and descriptions, and returns `{"tools": [...], "total_matches": N}` — each entry carrying the matched tool's real name, description, and full input schema. The list is capped at 20 entries; `total_matches` always reports the pre-cap count, and `"truncated": true` is added to the envelope only when more than 20 matched. The prefix applies to the meta-tool too, so a `"fs_"` prefix yields `fs_search_tools`. This keeps a server with thousands of tools from flooding the agent's tool list.

## Server: an agent over MCP

`McpAgentServer<Ctx>` wraps one `Agent<Ctx>` and serves it as a single MCP tool. The tool's name is the agent's name (sanitized to `[a-zA-Z0-9_-]+`), its description is the agent's description, and its input schema is `{"input": string}`; calling it runs the agent and returns the final text output.

Each request needs a fresh user context, so a context factory is required before serving:

- `McpAgentServer::new(agent)` then `.with_ctx(factory)`, where `factory: Fn() -> Ctx`, or
- `McpAgentServer::with_default_ctx(agent)` when `Ctx: Default`.

Builder methods configure the rest: `.name(..)` and `.version(..)` set the MCP `Implementation` reported at initialize, `.instructions(..)` sets the optional MCP `instructions`, and `.with_run_config(RunConfig)` configures each request's agent run. `RunConfig::timeout` is enforced at this boundary (core's `collect()` has no timer); on expiry the call returns an MCP error result rather than hanging. A client disconnect or `notifications/cancelled` propagates into the run's `CancellationToken`.

```rust,no_run
# use async_trait::async_trait;
# use futures_core::stream::BoxStream;
# use paigasus_helikon_core::{Agent, AgentError, AgentEvent, AgentInput, RunContext};
# struct MyAgent;
# #[async_trait]
# impl Agent<()> for MyAgent {
#     fn name(&self) -> &str { "assistant" }
#     fn description(&self) -> &str { "answers questions" }
#     async fn run(&self, _ctx: RunContext<()>, _input: AgentInput)
#         -> Result<BoxStream<'static, AgentEvent>, AgentError> {
#         Ok(Box::pin(futures_util::stream::empty()))
#     }
# }
# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
use paigasus_helikon_mcp::McpAgentServer;

let server = McpAgentServer::with_default_ctx(MyAgent)
    .name("my-agent-server")
    .version("0.1.0");

// Block on stdio until the client disconnects:
server.serve_stdio().await?;
# Ok(())
# }
```

For HTTP, `serve_streamable_http(addr)` binds a port and mounts the MCP endpoint at `/mcp`. To embed the agent in an existing hyper/axum router instead, `streamable_http_service()` returns a tower `StreamableHttpService<AgentMcpHandler<Ctx>, LocalSessionManager>` you can `nest_service` yourself. `serve_transport(transport)` is the escape hatch for any other rmcp server transport (in-process duplex transports, custom sockets).

## Errors

The crate's own error type is `McpError` (`#[non_exhaustive]`): variants cover the connect/`initialize` handshake (`Connect`), child-process spawn (`Spawn`), in-flight requests (`Service`), HTTP bind (`Bind`), and abnormal server termination (`Serve`), plus an `Other` (`#[error(transparent)]`) catch-all for anything else. Client-side *tool* failures never surface as `McpError` — they map to core's `ToolError`, because an agent only ever sees the `Tool` trait. (rmcp's own protocol-error type, which it aliases `McpError`, is referred to here as `ErrorData` to avoid the name clash.)

## Runtime note

Connect child-process transports from a multi-thread tokio runtime (`#[tokio::main]`'s default). Under a current-thread runtime the `initialize` handshake can stall against the child's pipe I/O.

See [Tools](./tools.md) for the `Tool<Ctx>` trait these adapters implement, and the [crate reference](../reference/crates.md) for version and feature details.
