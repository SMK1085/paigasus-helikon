# paigasus-helikon

The facade crate of the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. Most applications depend on this crate alone and turn on the features they need.

It re-exports [`paigasus-helikon-core`](https://crates.io/crates/paigasus-helikon-core) unconditionally as `paigasus_helikon::core`, and the provider, runtime, tool, and MCP sibling crates behind Cargo features.

## Install

```bash
cargo add paigasus-helikon --features openai,macros
```

Each feature gates one sibling crate behind a module on `paigasus_helikon::`:

| Feature | Re-export(s) | Crate pulled in |
| --- | --- | --- |
| *(always on)* | `core` | `paigasus-helikon-core` |
| `macros` | `macros` module + `tool` / `tools` macros | `paigasus-helikon-macros` |
| `openai` *(alias `providers-openai`)* | `openai` | `paigasus-helikon-providers-openai` |
| `anthropic` | `anthropic` | `paigasus-helikon-providers-anthropic` |
| `mcp` | `mcp` | `paigasus-helikon-mcp` |
| `tools` | `tools` | `paigasus-helikon-tools` |
| `tools-web` | adds `WebFetch` / `WebSearch` | `paigasus-helikon-tools/web` |
| `tools-os-sandbox` | adds `OsSandboxBackend` (Linux + macOS) | `paigasus-helikon-tools/os-sandbox` |
| `tools-microvm` | adds `ForkdBackend`, `EgressProxy`, `EgressPolicy`, `Isolation::Proxied` (microVM + domain-filtered egress via forkd/Firecracker; experimental — SMA-437) | `paigasus-helikon-tools/microvm` |
| `sessions-sqlite` | `sessions_sqlite` | `paigasus-helikon-sessions-sqlite` |
| `runtime-tokio` | `runtime_tokio` | `paigasus-helikon-runtime-tokio` |

Feature names are kebab-case; the module aliases are snake-case. The `evals`, `runtime-axum`, `runtime-temporal`, and `runtime-agentcore` features exist but gate not-yet-implemented stub crates — don't enable them yet. The `paigasus_helikon::schema::strict()` JSON-Schema normalizer is available regardless of features.

When using the `mcp` feature, `McpServerHandle` (from `paigasus_helikon::mcp`) implements `ToolSource<Ctx>` from core. Register MCP server handles directly on the builder with `.mcp_servers([...])` and finalize with `.build_resolved().await?` — no need to convert to a `Vec<Arc<dyn Tool<Ctx>>>` manually. See the [MCP integration guide](https://smk1085.github.io/paigasus-helikon/concepts/mcp-integration.html) for details.

## Example

A minimal agent against OpenAI (enable `openai`). This crate's `README.md` is its rustdoc front page, so the example is marked `ignore` (it reads `OPENAI_API_KEY` and calls the network); the same program ships compile-checked as `examples/budget_assistant_openai.rs`:

```ignore
use std::sync::Arc;

use paigasus_helikon::core::{
    Agent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession, RunContext,
    RunResultStreaming, TracerHandle,
};
use paigasus_helikon::openai::OpenAiModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = OpenAiModel::chat("gpt-5-mini").build()?; // reads OPENAI_API_KEY

    let agent = LlmAgent::builder::<()>()
        .name("assistant")
        .model(model)
        .instructions("You are a helpful assistant.")
        .build();

    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::<()>::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );

    let stream = agent.run(ctx, AgentInput::from_user_text("Hello!")).await?;
    let result = RunResultStreaming::new(stream).collect().await?;
    println!("{}", result.final_output);
    Ok(())
}
```

See the [quickstart](https://smk1085.github.io/paigasus-helikon/getting-started/quickstart.html) for the full tool-calling walkthrough, and the [`examples/`](https://github.com/SMK1085/paigasus-helikon/tree/main/crates/paigasus-helikon/examples) directory for runnable programs (`budget_assistant_openai`, `budget_assistant_anthropic`, `multi_agent_triage`, `streaming_console`, `structured_output`, `langfuse_tracing`).

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/)
- [Crate roster](https://smk1085.github.io/paigasus-helikon/reference/crates.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
