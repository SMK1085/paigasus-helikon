# Introduction

`paigasus-helikon` is a Rust SDK for building agentic AI systems. It separates slow-moving primitives (types, traits, message protocols) from fast-moving parts (provider SDKs, execution runtimes, tool catalogs), so downstream projects can pick the surface they need without dragging in the rest.

The SDK does not pick a deployment story, a hosting story, or an observability stack for you. Bring your own.

## What's here

The single-facade crate `paigasus-helikon` re-exports `paigasus-helikon-core` unconditionally and pulls in sibling crates behind kebab-case Cargo features. The shipped surface covers an end-to-end single-agent loop plus the first multi-agent pieces:

- **Agent loop.** `LlmAgent::builder` assembles an `Agent`; `agent.run(ctx, input)` returns an event stream you collect with `RunResultStreaming`. Core carrier types (`RunContext`, `AgentInput`, `HookRegistry`, `MemorySession`, `CancellationToken`, `TracerHandle`) live in `paigasus_helikon::core`.
- **Providers.** `paigasus_helikon::openai::OpenAiModel` (feature `openai`) and `paigasus_helikon::anthropic::AnthropicModel` (feature `anthropic`). Switching providers is a one-line `Model` swap on the builder.
- **Tools.** The `#[tool]` attribute and `tools!` macro (feature `macros`) turn an `async fn` into a registered, JSON-Schema-typed tool. The `tools` feature adds sandboxed Read/Write/Edit/Bash tools; `tools-web` adds the `WebFetch`/`WebSearch` network tools.
- **Sessions.** `MemorySession` ships in `core`; `paigasus_helikon::sessions_sqlite` (feature `sessions-sqlite`) is a persistent SQLite `Session` backend.
- **Runtime.** `paigasus_helikon::runtime_tokio` (feature `runtime-tokio`) is the default ephemeral Tokio runner. `paigasus_helikon::runtime_axum` (feature `runtime-axum`) is the self-hosted HTTP/SSE/WebSocket agent server built on axum — see [Axum Server Runtime](concepts/axum-server.md).
- **Multi-agent and MCP.** `Handoff::to(..)` wires triage-style delegation between agents; `paigasus_helikon::mcp` (feature `mcp`) is an rmcp-based MCP client/server wrapper. Guardrails, hooks, and permissions hang off the `HookRegistry` carried in every `RunContext`.

Fourteen crates are published to [crates.io](https://crates.io/crates/paigasus-helikon) with rustdoc on [docs.rs](https://docs.rs/paigasus-helikon).

A minimal run:

```rust
use paigasus_helikon::core::{
    Agent, AgentInput, LlmAgent, RunContext, RunResultStreaming,
};
use paigasus_helikon::openai::OpenAiModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = OpenAiModel::chat("gpt-5-mini").build()?;

    let agent = LlmAgent::builder::<()>()
        .name("budget-assistant")
        .model(model)
        .instructions("You are a budgeting assistant. ...")
        .build();

    let ctx: RunContext<()> = RunContext::ephemeral(());

    let input = AgentInput::from_user_text("How am I doing on my dining budget this month?");

    let stream = agent.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream).collect().await?;

    println!("{}", result.final_output);
    Ok(())
}
```

Start at [Quickstart](getting-started/quickstart.md), then read [Core primitives](concepts/core-primitives.md) for the trait surface.

## What's not yet here

Three crates are still `0.0.0` stubs, not implemented: `paigasus-helikon-evals` (evaluation harness) and the alternative runtimes `paigasus-helikon-runtime-temporal` and `paigasus-helikon-runtime-agentcore`. Their facade features exist but the crates are empty.

Tracked work lives in Linear under the project **Paigasus Helikon** (issues are prefixed `SMA-`).
