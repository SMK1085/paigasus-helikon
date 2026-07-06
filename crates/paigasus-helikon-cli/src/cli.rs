//! Clap command tree for the `helikon` binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Paigasus Helikon agent CLI.
#[derive(Debug, Parser)]
#[command(name = "helikon", version, about = "Paigasus Helikon agent CLI")]
pub struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Interactive REPL with hot-reloading agent definitions.
    Repl(ReplArgs),
    /// Evaluation commands.
    Eval {
        /// Eval subcommand.
        #[command(subcommand)]
        command: EvalCommand,
    },
    /// MCP server commands.
    Mcp {
        /// MCP subcommand.
        #[command(subcommand)]
        command: McpCommand,
    },
}

/// Arguments for `helikon repl`.
#[derive(Debug, clap::Args)]
pub struct ReplArgs {
    /// Path to the agents sidecar file.
    #[arg(long, default_value = "agents.toml")]
    pub agents: PathBuf,
    /// Agent to talk to first (default: first agent in the file).
    #[arg(long)]
    pub agent: Option<String>,
}

/// `helikon eval …` subcommands.
#[derive(Debug, Subcommand)]
pub enum EvalCommand {
    /// Run a JSONL dataset against an agent and print scores.
    Run(EvalRunArgs),
}

/// Arguments for `helikon eval run`.
#[derive(Debug, clap::Args)]
pub struct EvalRunArgs {
    /// Path to the JSONL dataset.
    pub dataset: PathBuf,
    /// Agent name from the sidecar file.
    #[arg(long)]
    pub agent: String,
    /// Path to the agents sidecar file.
    #[arg(long, default_value = "agents.toml")]
    pub agents: PathBuf,
    /// Emit the full report as JSON instead of a table.
    #[arg(long)]
    pub json: bool,
    /// Fail (exit 1) if the mean non-skipped score is below this value.
    #[arg(long)]
    pub fail_under: Option<f64>,
    /// Trace sink, e.g. `sqlite:traces.db`.
    #[arg(long)]
    pub trace: Option<String>,
}

/// `helikon mcp …` subcommands.
#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// Serve a sidecar agent as an MCP server (stdio by default).
    Serve(McpServeArgs),
}

/// Arguments for `helikon mcp serve`.
#[derive(Debug, clap::Args)]
pub struct McpServeArgs {
    /// Agent name from the sidecar file.
    #[arg(long)]
    pub agent: String,
    /// Path to the agents sidecar file.
    #[arg(long, default_value = "agents.toml")]
    pub agents: PathBuf,
    /// Serve over streamable HTTP on this address instead of stdio.
    #[arg(long)]
    pub http: Option<String>,
}
