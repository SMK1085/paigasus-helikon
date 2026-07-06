//! `helikon mcp serve`: expose one sidecar agent as an MCP server, over
//! stdio by default or streamable HTTP via `--http`.

use std::process::ExitCode;

use paigasus_helikon_mcp::McpAgentServer;

use crate::cli::McpServeArgs;
use crate::registry::AgentRegistry;

/// Runs `helikon mcp serve` and returns the process exit code.
///
/// # Errors
///
/// Returns an error if the sidecar fails to load, the named agent doesn't
/// exist or fails to build (missing tool script, instructions file, etc.),
/// or the server terminates abnormally (bind failure, transport error).
pub async fn serve(args: McpServeArgs) -> anyhow::Result<ExitCode> {
    let registry = AgentRegistry::load(&args.agents)?;
    let agent = registry.build_agent(&args.agent)?;
    let server = McpAgentServer::with_default_ctx(agent).name(format!("helikon-{}", args.agent));

    match args.http {
        Some(addr) => server.serve_streamable_http(&addr).await?,
        None => server.serve_stdio().await?,
    }

    Ok(ExitCode::SUCCESS)
}
