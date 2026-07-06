//! `helikon repl`: an interactive REPL over a live [`AgentRegistry`], with
//! hot-reloading agent definitions and a handful of slash commands.
//!
//! The loop and its I/O are intentionally thin glue — the testable unit is
//! [`parse_repl_command`] (see `tests/repl_commands.rs`).

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use futures_util::stream::StreamExt as _;
use paigasus_helikon_core::{
    AgentEvent, AgentInput, MemorySession, RunConfig, RunContext, Runner as _, Session,
};
use paigasus_helikon_runtime_tokio::TokioRunner;
use tokio::io::{AsyncBufReadExt as _, BufReader};

use crate::cli::ReplArgs;
use crate::registry::AgentRegistry;

/// A parsed REPL input line.
///
/// [`parse_repl_command`] is total: every `&str` maps to exactly one
/// variant, including the empty line (`Say(String::new())`, treated as a
/// no-op turn by the run loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplCommand {
    /// `/agents` — list every agent declared in the sidecar.
    Agents,
    /// `/switch NAME` — switch the current agent.
    Switch(String),
    /// `/reload` — force an immediate reload of the sidecar file.
    Reload,
    /// `/quit` or `/exit` — leave the REPL.
    Quit,
    /// Any other line starting with `/`, carried verbatim (trimmed).
    Unknown(String),
    /// Plain text — a turn for the current agent (trimmed; may be empty).
    Say(String),
}

/// Parses one line of REPL input into a [`ReplCommand`].
///
/// Whitespace is trimmed before matching. Recognized commands are
/// `/agents`, `/switch NAME`, `/reload`, `/quit`, and `/exit`; any other
/// line starting with `/` is [`ReplCommand::Unknown`]; everything else is
/// [`ReplCommand::Say`].
pub fn parse_repl_command(line: &str) -> ReplCommand {
    let trimmed = line.trim();
    let mut parts = trimmed.splitn(2, ' ');
    let head = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();

    match head {
        "/agents" => ReplCommand::Agents,
        "/switch" => ReplCommand::Switch(rest.to_owned()),
        "/reload" => ReplCommand::Reload,
        "/quit" | "/exit" => ReplCommand::Quit,
        _ if head.starts_with('/') => ReplCommand::Unknown(trimmed.to_owned()),
        _ => ReplCommand::Say(trimmed.to_owned()),
    }
}

/// Runs the interactive REPL and returns the process exit code.
///
/// # Errors
///
/// Returns an error if the sidecar fails to load, `--agent` names an agent
/// that doesn't exist, the sidecar has no agents at all, or the file
/// watcher fails to start.
pub async fn run(args: ReplArgs) -> anyhow::Result<ExitCode> {
    let registry = Arc::new(AgentRegistry::load(&args.agents)?);

    let (mut current, auto_selected) = match &args.agent {
        Some(name) => {
            if !registry.has_agent(name) {
                anyhow::bail!("no such agent '{name}' in '{}'", args.agents.display());
            }
            (name.clone(), false)
        }
        None => {
            let first = registry.first_agent().context("sidecar has no agents")?;
            (first, true)
        }
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<(), String>>();
    let _debouncer = registry.watch(move |res| {
        let _ = tx.send(res.map_err(|e| e.to_string()));
    })?;

    let session: Arc<dyn Session> = Arc::new(MemorySession::new());

    print_banner(&registry, &current, auto_selected, &args.agents);

    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    loop {
        print!("{current}> ");
        std::io::stdout().flush().ok();

        tokio::select! {
            reload = rx.recv() => {
                match reload {
                    Some(Ok(())) => println!("\nreloaded {}", args.agents.display()),
                    Some(Err(err)) => {
                        println!("\nreload failed (keeping previous definitions): {err}");
                    }
                    None => {
                        // The debouncer is held alive in this scope, so its
                        // sender never drops while the loop runs.
                    }
                }
            }
            line = lines.next_line() => {
                let Some(line) = line? else {
                    // EOF on stdin: exit cleanly.
                    break;
                };
                match parse_repl_command(&line) {
                    ReplCommand::Agents => print_agents(&registry, &current),
                    ReplCommand::Switch(name) => {
                        if name.is_empty() {
                            println!("usage: /switch NAME");
                        } else if registry.has_agent(&name) {
                            current = name;
                            println!("switched to '{current}'");
                        } else {
                            println!("no such agent '{name}'");
                        }
                    }
                    ReplCommand::Reload => match registry.reload() {
                        Ok(()) => println!("reloaded {}", args.agents.display()),
                        Err(e) => println!("reload failed (keeping previous definitions): {e:#}"),
                    },
                    ReplCommand::Quit => break,
                    ReplCommand::Unknown(cmd) => println!("unknown command: {cmd}"),
                    ReplCommand::Say(text) => {
                        if text.is_empty() {
                            continue;
                        }
                        run_turn(&registry, &current, Arc::clone(&session), &text).await;
                    }
                }
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Prints the startup banner: available agents, the current agent, the
/// slash commands, and a note about hot reload.
fn print_banner(registry: &AgentRegistry, current: &str, auto_selected: bool, path: &Path) {
    let names = registry.agent_names();
    println!("Paigasus Helikon REPL — sidecar: {}", path.display());
    println!("agents: {}", names.join(", "));
    if auto_selected {
        println!("current agent: {current} (alphabetically first — no --agent given)");
    } else {
        println!("current agent: {current}");
    }
    println!("commands: /agents  /switch NAME  /reload  /quit (or /exit)");
    println!("editing the sidecar file hot-reloads its definitions on your next turn");
    println!();
}

/// Prints every agent name, marking the current one.
fn print_agents(registry: &AgentRegistry, current: &str) {
    for name in registry.agent_names() {
        if name == current {
            println!("* {name}");
        } else {
            println!("  {name}");
        }
    }
}

/// Builds the current agent and runs one streamed turn against the shared
/// session, printing token deltas as they arrive.
///
/// Build and run failures are printed and swallowed — a bad turn must not
/// kill the REPL.
async fn run_turn(registry: &AgentRegistry, current: &str, session: Arc<dyn Session>, text: &str) {
    let agent = match registry.build_agent(current) {
        Ok(agent) => agent,
        Err(e) => {
            println!("error: {e:#}");
            return;
        }
    };

    let ctx = RunContext::ephemeral(()).with_session(session);
    let streaming = match TokioRunner
        .run_streamed(
            &agent,
            ctx,
            AgentInput::from_user_text(text),
            RunConfig::default(),
        )
        .await
    {
        Ok(streaming) => streaming,
        Err(e) => {
            println!("error: {e}");
            return;
        }
    };

    let mut events = streaming.events;
    while let Some(event) = events.next().await {
        match event {
            AgentEvent::TokenDelta { text } => {
                print!("{text}");
                std::io::stdout().flush().ok();
            }
            AgentEvent::MessageOutput { .. } => println!(),
            AgentEvent::RunFailed { error } => println!("error: {error}"),
            _ => {}
        }
    }
}
