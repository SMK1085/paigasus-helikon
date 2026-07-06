//! Internal implementation of the `helikon` / `paigasus-helikon` CLI.
//!
//! **Internal — no stability guarantees.** This library target exists so
//! the two binaries can share code; its API may change in any release.

pub mod cli;

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
        cli::Command::Repl(_args) => anyhow::bail!("repl: implemented in a later task"),
        cli::Command::Eval { .. } => anyhow::bail!("eval: implemented in a later task"),
        cli::Command::Mcp { .. } => anyhow::bail!("mcp: implemented in a later task"),
    }
}
