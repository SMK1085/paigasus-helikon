//! Real-model demo: an agent researches a question with `WebSearch` + `WebFetch`,
//! with both network tools gated by a `PermissionPolicy`.
//!
//! Run with keys:
//! `OPENAI_API_KEY=... BRAVE_SEARCH_API_KEY=... \
//!   cargo run -p paigasus-helikon-tools --features web --example web_research`
//!
//! This example is the canonical reference for gating network tools with a
//! `PermissionPolicy`. The policy below allows `WebSearch` and `WebFetch`; swap
//! an `Allow` for an `AskUser`/`Deny` to see a tool blocked.

use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession,
    PermissionDecision, PermissionPolicy, RunContext, TracerHandle,
};
use paigasus_helikon_providers_openai::OpenAiModel;
use paigasus_helikon_tools::{BraveBackend, WebFetchTool, WebSearchTool};

/// Allow the network tools explicitly; escalate everything else to `AskUser`.
/// Because no [`ApprovalHandler`](paigasus_helikon_core::ApprovalHandler) is
/// installed on the [`RunContext`], `AskUser` resolves to `Deny` — a safe
/// default that gates unexpected tool calls without interactive approval.
struct AllowWebTools;

#[async_trait]
impl PermissionPolicy<()> for AllowWebTools {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        tool: &str,
        _args: &serde_json::Value,
    ) -> PermissionDecision {
        match tool {
            "WebSearch" | "WebFetch" => PermissionDecision::Allow,
            _ => PermissionDecision::AskUser {
                prompt: format!("Allow `{tool}`?"),
            },
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let backend = Arc::new(BraveBackend::from_env()?);
    let model = OpenAiModel::chat("gpt-5-mini").build()?;

    let agent = LlmAgent::builder::<()>()
        .name("web-researcher")
        .model(model)
        .instructions(
            "Research the user's question. Use WebSearch to find sources, then \
             WebFetch a result URL to read it. Cite the URLs you used.",
        )
        .tool(WebSearchTool::builder(backend).build())
        // SSRF guard on by default; allow_domains/deny_domains could narrow it.
        .tool(WebFetchTool::builder().build())
        .build();

    // Install the gating policy on the run context. No ApprovalHandler is
    // installed, so AskUser on unexpected tools resolves to Deny (safe default).
    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::<()>::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
    .with_permission_policy(Arc::new(AllowWebTools));

    let input = AgentInput::from_user_text(
        "What is the Hippocrene spring and how does it relate to Mount Helicon?",
    );
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
