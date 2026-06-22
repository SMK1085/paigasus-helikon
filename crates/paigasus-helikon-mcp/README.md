# paigasus-helikon-mcp

Model Context Protocol (MCP) integration for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. Wraps [`rmcp`](https://crates.io/crates/rmcp) (the official Rust MCP SDK) in both directions:

- **Client** — `McpServerHandle` connects to an external MCP server (stdio child process or streamable HTTP) and re-exposes its tools as [`paigasus-helikon-core`](https://crates.io/crates/paigasus-helikon-core) `Tool`s.
- **Server** — `McpAgentServer` wraps any Paigasus Helikon `Agent` and serves it as a single MCP tool over stdio or streamable HTTP.

SSE transports are not supported: rmcp removed them in 0.11.0 and the 2025-03-26 MCP spec revision deprecated HTTP+SSE in favor of streamable HTTP.

## Install

```bash
cargo add paigasus-helikon-mcp
```

Most users enable the `mcp` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::mcp`.

## Example

Expose an external MCP server's tools to an agent — explicit path:

```rust
use paigasus_helikon_mcp::McpServerHandle;

let fs = McpServerHandle::stdio(tokio::process::Command::new("npx"), |cmd| {
    cmd.args(["-y", "@modelcontextprotocol/server-filesystem", "/data"]);
})
.tool_prefix("fs_")
.connect()
.await?;

let tools = fs.tools::<()>(); // pass to LlmAgent::builder().tools(...)
```

`McpServerHandle` implements `ToolSource<Ctx>` from `paigasus-helikon-core`, so you can also register handles directly on the agent builder and let `.build_resolved()` discover and merge the tools in one step:

```rust
// After connecting the handle as above (without .tool_prefix if not needed):
// let agent = LlmAgent::builder::<()>()
//     .name("assistant")
//     .model(model)
//     .mcp_servers([fs])
//     .build_resolved()
//     .await?;
```

A duplicate tool name across registered sources fails the build with `ToolSourceError::DuplicateName`.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-mcp)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [MCP integration](https://smk1085.github.io/paigasus-helikon/concepts/mcp-integration.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
