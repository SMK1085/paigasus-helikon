# Workspace layout

Paigasus Helikon is a Cargo workspace of **21 crates** under `crates/`, all in the
`paigasus-helikon-*` namespace. As a consumer you rarely depend on more than two of them directly: the
`paigasus-helikon-core` trait crate, or — more commonly — the `paigasus-helikon` **facade**,
which re-exports `core` plus the optional sibling crates behind Cargo features.

This page is about **how to depend** on the SDK. For the per-crate version and ownership table,
see [Crates reference](../reference/crates.md).

## The facade

`paigasus-helikon` re-exports `paigasus-helikon-core` **unconditionally** as
`paigasus_helikon::core`, and each optional sibling crate behind a kebab-case feature. With no
features you still get the full core surface: the traits (`Agent`, `Model`, `Tool`, `Session`)
plus the carrier and impl types (`LlmAgent`, `RunContext`, the event stream). Features add the
concrete adapters and runtimes on top.

Feature names are kebab-case in `Cargo.toml`; the re-export module aliases are snake-case.

| Feature | Re-exported as | Surface |
| --- | --- | --- |
| *(always on)* | `paigasus_helikon::core` | `paigasus-helikon-core` — traits, agent loop, event stream, carrier types |
| `macros` | `paigasus_helikon::macros`, plus `paigasus_helikon::tool` and `paigasus_helikon::tools` | `#[tool]` attribute + `tools!` macro |
| `openai` *(alias `providers-openai`)* | `paigasus_helikon::openai` *(also `providers_openai`)* | `OpenAiModel` adapter |
| `anthropic` | `paigasus_helikon::anthropic` | `AnthropicModel` adapter |
| `litellm` | `paigasus_helikon::litellm` | `LiteLlmModel` adapter — LiteLLM proxy (OpenAI-compatible gateway) |
| `mcp` | `paigasus_helikon::mcp` | rmcp-based MCP client/server |
| `tools` | `paigasus_helikon::tools` | sandboxed Read/Write/Edit/Bash tools |
| `tools-web` | *(extends `tools`)* | adds the WebFetch / WebSearch network tools |
| `runtime-tokio` | `paigasus_helikon::runtime_tokio` | ephemeral Tokio runner |
| `runtime-axum` | `paigasus_helikon::runtime_axum` | self-hosted HTTP/SSE/WebSocket agent server — see [Axum Server Runtime](../concepts/axum-server.md) |
| `runtime-temporal` | `paigasus_helikon::runtime_temporal` | durable Temporal-backed runner — see [Runtimes](../concepts/runtimes.md) |
| `runtime-agentcore` | `paigasus_helikon::runtime_agentcore` | AWS Bedrock AgentCore container shim — see [Runtimes](../concepts/runtimes.md) |
| `sessions-sqlite` | `paigasus_helikon::sessions_sqlite` | SQLite `Session` backend |
| `evals` | `paigasus_helikon::evals` | evaluation harness — datasets, evaluators, `MockModel`, SQLite/Parquet trace sinks |

In addition, `paigasus_helikon::schema::strict` re-exports the JSON-Schema strict-mode normalizer
from `core`, regardless of features.

### `tools` name clash

`paigasus_helikon::tools` resolves to **two different items** when both `macros` and `tools` are
enabled: the `tools!` function-like macro (from `macros`) and the sandboxed-tools **module**
(from the `tools` crate). They live in separate namespaces, so Rust disambiguates by use site — a
macro invocation `tools![...]` versus a path `tools::SomeTool`. Be explicit about which you mean.

## Published vs stub crates

Twenty crates carry real implementations and are published on crates.io / docs.rs:

- `paigasus-helikon-core` — the dependency root (traits, agent loop, carrier types)
- `paigasus-helikon` — the facade
- `paigasus-helikon-macros` — `#[tool]` + `tools!`
- `paigasus-helikon-providers-openai`
- `paigasus-helikon-providers-anthropic`
- `paigasus-helikon-providers-bedrock`
- `paigasus-helikon-providers-gemini`
- `paigasus-helikon-providers-litellm`
- `paigasus-helikon-sessions-sqlite`
- `paigasus-helikon-sessions-postgres`
- `paigasus-helikon-sessions-redis`
- `paigasus-helikon-runtime-tokio`
- `paigasus-helikon-runtime-axum` — self-hosted HTTP/SSE/WebSocket agent server (see [Axum Server Runtime](../concepts/axum-server.md))
- `paigasus-helikon-runtime-actix` — self-hosted actix-web HTTP/SSE/WebSocket agent server (same public surface as `runtime-axum`)
- `paigasus-helikon-runtime-temporal` — durable Temporal-backed runner (see [Runtimes](../concepts/runtimes.md))
- `paigasus-helikon-runtime-agentcore` — AWS Bedrock AgentCore container shim (see [Runtimes](../concepts/runtimes.md))
- `paigasus-helikon-mcp`
- `paigasus-helikon-tools`
- `paigasus-helikon-evals` — evaluation harness: datasets, evaluators, `MockModel`, SQLite/Parquet trace sinks
- `paigasus-helikon-cli` — published binary crate (`helikon` / `paigasus-helikon` binaries); its lib target is internal and carries no stability guarantee, publishing only so `cargo install paigasus-helikon-cli` resolves

Zero name-claim stubs remain in the workspace. The one crate that rounds out the
workspace's 21 without publishing is `paigasus-helikon-sessions-testkit`, an internal
`Session` conformance test harness (`publish = false` by design).

## Picking your surface

**Depend on `core` alone** when you only need the trait definitions — for example a crate that
implements its own `Model` or `Tool<Ctx>` and doesn't pull in any provider:

```toml
[dependencies]
paigasus-helikon-core = "0.5"
```

**Depend on the facade** for everything else, selecting only the features you use. A typical
single-agent app with OpenAI, the tool macros, and SQLite-backed sessions:

```toml
[dependencies]
paigasus-helikon = { version = "0.4", features = ["openai", "macros", "sessions-sqlite"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
anyhow = "1"
serde = { version = "1", features = ["derive"] }
schemars = "1"
```

That pulls `paigasus-helikon-providers-openai`, `paigasus-helikon-macros`, and
`paigasus-helikon-sessions-sqlite` transitively; everything else stays out of your dependency
graph. Imports then come from the facade's re-export modules:

```rust
use paigasus_helikon::core::{Agent, AgentInput, LlmAgent, MemorySession, RunContext};
use paigasus_helikon::openai::OpenAiModel;
use paigasus_helikon::{tool, tools};
```

Swap providers by swapping one feature and one import: `anthropic` /
`paigasus_helikon::anthropic::AnthropicModel` instead of `openai` / `OpenAiModel`. Add `tools`
(or `tools-web`) for the sandboxed file/shell tools, `mcp` for MCP servers, and `runtime-tokio`
for the runner boundary.

## Next steps

- [Quickstart](./quickstart.md) — a complete first agent.
- [Core primitives](../concepts/core-primitives.md) — the seven traits `core` defines.
- [Model providers](../concepts/model-providers.md) — `OpenAiModel` and `AnthropicModel`.
- [Crates reference](../reference/crates.md) — per-crate versions and ownership.
