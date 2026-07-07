//! Internal implementation of the `helikon` / `paigasus-helikon` CLI.
//!
//! **Internal — no stability guarantees.** This library target exists so
//! the two binaries can share code; its API may change in any release.

pub mod cli;
pub mod eval_cmd;
pub mod mcp_cmd;
pub mod model;
pub mod registry;
pub mod repl;
pub mod rhai_tool;
pub mod sidecar;

use clap::Parser as _;
use std::process::ExitCode;

/// Entry point shared by both binaries.
pub fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("failed to start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: cli::Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        cli::Command::Repl(args) => repl::run(args).await,
        cli::Command::Eval { command } => match command {
            cli::EvalCommand::Run(args) => eval_cmd::run(args).await,
        },
        cli::Command::Mcp { command } => match command {
            cli::McpCommand::Serve(args) => mcp_cmd::serve(args).await,
        },
    }
}
