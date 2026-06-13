//! Crate error type. Client-side tool failures surface as core
//! `ToolError`, never as [`McpError`] — agents only ever see the `Tool`
//! trait.

/// Errors from connecting to, serving, or talking to MCP endpoints.
///
/// Named `McpError` in this crate's namespace; note rmcp's own
/// protocol-error type is `rmcp::ErrorData` (aliased `McpError` upstream) —
/// always refer to that one as `ErrorData` in this crate to avoid confusion.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    /// The connection / MCP `initialize` handshake failed.
    #[error("failed to connect to MCP server: {0}")]
    Connect(#[from] Box<rmcp::service::ClientInitializeError>),
    /// Spawning the child-process transport failed.
    #[error("failed to spawn MCP server process: {0}")]
    Spawn(#[from] std::io::Error),
    /// An MCP request failed after the connection was established.
    #[error("MCP request failed: {0}")]
    Service(#[from] rmcp::ServiceError),
    /// Binding the HTTP listener failed.
    #[error("failed to bind {addr}: {source}")]
    Bind {
        /// The address that could not be bound.
        addr: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The running server terminated abnormally (initialize failure, task
    /// panic, or HTTP serve error). Constructed only at this crate's serve
    /// call sites; external callers wrapping other errors use [`McpError::Other`].
    #[error("MCP server terminated abnormally: {0}")]
    Serve(#[source] anyhow::Error),
    /// Anything else.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
