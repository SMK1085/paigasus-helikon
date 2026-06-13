//! Real-model demo: an agent explores a sandbox with the FS + Bash tools,
//! with the `Bash` tool gated by a `PermissionPolicy`.
//!
//! Run with a key:
//! `OPENAI_API_KEY=... cargo run -p paigasus-helikon-tools --example explore_sandbox`
//!
//! This example is the canonical reference for gating `BashTool` with a
//! `PermissionPolicy`. With no `ApprovalHandler` installed, an `AskUser`
//! decision falls back to `Deny` — so the shell is blocked by safe default.

use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession,
    PermissionDecision, PermissionPolicy, RunContext, TracerHandle,
};
use paigasus_helikon_providers_openai::OpenAiModel;
use paigasus_helikon_tools::{BashTool, EditTool, ReadTool, Sandbox, WriteTool};

/// Allow every tool except `Bash`, which is escalated to `AskUser`. Because no
/// [`ApprovalHandler`](paigasus_helikon_core::ApprovalHandler) is installed on
/// the [`RunContext`], `AskUser` resolves to `Deny` — a safe default that gates
/// the shell without requiring interactive approval in the demo.
struct GateBash;

#[async_trait]
impl PermissionPolicy<()> for GateBash {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        tool: &str,
        _args: &serde_json::Value,
    ) -> PermissionDecision {
        if tool == "Bash" {
            PermissionDecision::AskUser {
                prompt: "Allow the agent to run a shell command?".to_owned(),
            }
        } else {
            PermissionDecision::Allow
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Demo sandbox = the current directory. cap-std confines all FS access to
    // it (no `../` escape), but Write/Edit CAN create/overwrite files within
    // it — point this at a throwaway dir if you adapt the prompt to write.
    let sandbox = Sandbox::open(".")?;
    let model = OpenAiModel::chat("gpt-5-mini").build()?;

    let agent = LlmAgent::builder::<()>()
        .name("sandbox-explorer")
        .model(model)
        // `Bash` is listed so the model attempts it — demonstrating that the
        // GateBash policy blocks the call with a surfaced denial (the
        // AskUser → Deny fallback), rather than the tool silently missing.
        .instructions(
            "You can inspect the sandbox with Read/Write/Edit/Bash. Answer the \
             user's question about its contents concisely.",
        )
        .tool(ReadTool::<()>::new(sandbox.clone()))
        .tool(WriteTool::<()>::new(sandbox.clone()))
        .tool(EditTool::<()>::new(sandbox.clone()))
        .tool(BashTool::<()>::builder(sandbox).build())
        .build();

    // Install the gating policy on the run context. No ApprovalHandler is
    // installed, so AskUser on Bash resolves to Deny (safe default).
    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::<()>::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
    .with_permission_policy(Arc::new(GateBash));

    let input =
        AgentInput::from_user_text("List the files here and summarize what this project is.");
    let mut stream = agent.run(ctx, input).await?;
    let mut stdout = std::io::stdout();
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TokenDelta { text } => {
                print!("{text}");
                stdout.flush()?;
            }
            // Surface in-run failures (bad key, rejected model, …) instead of
            // exiting silently; fatal errors arrive as a stream event, not the
            // outer Result.
            AgentEvent::RunFailed { error } => anyhow::bail!("run failed: {error}"),
            _ => {}
        }
    }
    println!();
    Ok(())
}
