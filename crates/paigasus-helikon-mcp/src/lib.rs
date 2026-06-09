//! MCP integration for the Paigasus Helikon AI SDK.
//!
//! Wraps [`rmcp`] (the official Rust MCP SDK) in both directions:
//!
//! - **Client** — `McpServerHandle` connects to an external MCP server
//!   (stdio child process or streamable HTTP) and re-exposes its tools as
//!   [`paigasus_helikon_core::Tool`] implementations. Discovery happens once
//!   at connect; `tools()` is synchronous.
//! - **Server** — `McpAgentServer` wraps any
//!   [`paigasus_helikon_core::Agent`] and serves it as a single MCP tool
//!   over stdio or streamable HTTP.
//!
//! SSE transports are not supported: rmcp removed them in 0.11.0 and the
//! 2025-06-18 MCP spec revision deprecated HTTP+SSE in favor of streamable
//! HTTP.

mod client;
mod error;
mod server;

pub use error::McpError;
