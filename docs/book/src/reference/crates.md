# Crate overview

The workspace is **21 crates** under `crates/`, all named `paigasus-helikon-*` (plus the `paigasus-helikon` facade itself), plus three further internal, non-published Cargo workspace members outside `crates/` (see below). This page is the ownership map: one row per crate, what it owns, whether it is published, and how the crates depend on each other.

For orientation — how to pick crates and add them to your `Cargo.toml` — see [workspace layout](../getting-started/workspace-layout.md). For the rendered rustdoc, see [API docs](./api-docs.md).

## Dependency direction

- `paigasus-helikon-core` is the root: it owns the trait surface, the agent loop, the event stream, and the carrier types. It depends on no other workspace crate.
- The provider, session, tool, MCP, and runtime crates each depend on `core` and on nothing else in the workspace (`-tools` carries a path-only dev-dep on `-providers-openai` for an example; it is stripped from the published manifest).
- `paigasus-helikon-macros` is a proc-macro crate; its `#[tool]` expansion targets `core` types in the consumer's crate.
- `paigasus-helikon` is the **facade**: it re-exports `core` unconditionally and the sibling crates behind Cargo features. Application crates normally depend on the facade alone and turn on the features they need.
- `paigasus-helikon-cli` consumes `core` and the sibling crates it needs (`-evals`, `-runtime-tokio`, `-providers-openai`, `-providers-anthropic`, `-mcp`) directly — not the facade. It publishes to crates.io as a binary crate; its lib target is internal (`missing_docs` opted out) and carries no stability guarantee, publishing only so `cargo install paigasus-helikon-cli` resolves.

## Crate table

This table deliberately carries no version numbers. Each crate name links to its [docs.rs](https://docs.rs) page, which always shows the current published version; for the in-tree version, read that crate's `Cargo.toml`. Versions move every release, so any number mirrored here would be wrong within days.

| Crate | Concern | State |
| --- | --- | --- |
| [`paigasus-helikon-core`](https://docs.rs/paigasus-helikon-core) | Trait surface, agent loop, event stream, carrier types — the dependency root | published |
| [`paigasus-helikon`](https://docs.rs/paigasus-helikon) | Facade — re-exports `core` always, siblings behind features | published |
| [`paigasus-helikon-macros`](https://docs.rs/paigasus-helikon-macros) | `#[tool]` attribute and `tools!` proc macros | published |
| [`paigasus-helikon-providers-openai`](https://docs.rs/paigasus-helikon-providers-openai) | OpenAI model adapter (`OpenAiModel`) | published |
| [`paigasus-helikon-providers-anthropic`](https://docs.rs/paigasus-helikon-providers-anthropic) | Anthropic model adapter (`AnthropicModel`) | published |
| [`paigasus-helikon-providers-bedrock`](https://docs.rs/paigasus-helikon-providers-bedrock) | Amazon Bedrock Converse API model adapter (`BedrockModel`) | published |
| [`paigasus-helikon-providers-gemini`](https://docs.rs/paigasus-helikon-providers-gemini) | Google Gemini model adapter (`GeminiModel`; Developer API + Vertex AI) | published |
| [`paigasus-helikon-providers-litellm`](https://docs.rs/paigasus-helikon-providers-litellm) | LiteLLM proxy adapter (`LiteLlmModel`; OpenAI-compatible gateway) | published |
| [`paigasus-helikon-sessions-sqlite`](https://docs.rs/paigasus-helikon-sessions-sqlite) | SQLite-backed `Session` backend | published |
| [`paigasus-helikon-sessions-postgres`](https://docs.rs/paigasus-helikon-sessions-postgres) | PostgreSQL-backed `Session` backend (`PostgresSession`) | published |
| [`paigasus-helikon-sessions-redis`](https://docs.rs/paigasus-helikon-sessions-redis) | Redis Streams-backed `Session` backend (`RedisSession`) | published |
| [`paigasus-helikon-runtime-tokio`](https://docs.rs/paigasus-helikon-runtime-tokio) | Default ephemeral Tokio runner | published |
| [`paigasus-helikon-runtime-axum`](https://docs.rs/paigasus-helikon-runtime-axum) | Self-hosted Axum HTTP/SSE/WebSocket agent server (`AgentServer` builder, 6 endpoints, replayable runs) | published |
| [`paigasus-helikon-runtime-actix`](https://docs.rs/paigasus-helikon-runtime-actix) | Self-hosted actix-web HTTP/SSE/WebSocket agent server (same public surface as `runtime-axum`; embed into an existing actix-web app) | published |
| [`paigasus-helikon-runtime-temporal`](https://docs.rs/paigasus-helikon-runtime-temporal) | Durable Temporal-backed runner (`TemporalRunner`; crash-resume via Temporal history replay) | published |
| [`paigasus-helikon-runtime-agentcore`](https://docs.rs/paigasus-helikon-runtime-agentcore) | AWS Bedrock AgentCore container shim (`AgentCoreServer`; HTTP + MCP protocol contract) | published |
| [`paigasus-helikon-mcp`](https://docs.rs/paigasus-helikon-mcp) | MCP integration — `rmcp` client and server wrappers | published |
| [`paigasus-helikon-tools`](https://docs.rs/paigasus-helikon-tools) | Sandboxed Read/Write/Edit/Bash tools (+ `WebFetch`/`WebSearch` behind `web`) | published |
| [`paigasus-helikon-evals`](https://docs.rs/paigasus-helikon-evals) | Evaluation harness — datasets, evaluators, `MockModel`, SQLite/Parquet trace sinks | published |
| [`paigasus-helikon-cli`](https://docs.rs/paigasus-helikon-cli) | `helikon` / `paigasus-helikon` CLI binaries; lib target is internal, no stability guarantee | published (binary crate) |
| `paigasus-helikon-sessions-testkit` | Shared `Session` conformance test harness (internal — never published) | internal — `publish = false` |

Every crate above the `-sessions-testkit` row publishes to crates.io — the last two stubs (`-evals`, `-cli`) ascended to real implementations in SMA-332/SMA-333, following the four-remaining-crates ascend before them (`-runtime-axum`, `-runtime-temporal`, `-runtime-agentcore`). `paigasus-helikon-sessions-testkit` is the sole `publish = false` crate under `crates/`, and it is an intentional internal test harness rather than a stub awaiting an ascend.

The workspace has three further members outside `crates/`:

- `paigasus-helikon-runtime-http-conformance` (under `tests/runtime-http-conformance/`, `version = "0.0.0"`, `publish = false`) — an internal axum⇔actix wire-format conformance suite exercising both HTTP runtimes against the same test cases.
- `paigasus-helikon-workspace-lints` (under `tests/workspace-lints/`, `version = "0.0.0"`, `publish = false`) — an internal workspace-wide source-lint suite; its current lint asserts that no `tracing` macro passes `target`/`parent` with `=` instead of `:` (which would silently record a field and leave the event on its module-path target).
- `paigasus-helikon-provider-stream-conformance` (under `tests/provider-stream-conformance/`, `version = "0.0.0"`, `publish = false`) — the cross-provider `Model::invoke` stream event-ordering conformance suite, driving every provider translator over a paced HTTP server.

Like `-sessions-testkit`, all three are intentional non-published test harnesses, not stubs.

## Facade feature → re-export map

Add the facade and turn on the features you need. Each feature gates one sibling crate behind a module on `paigasus_helikon::`:

| Feature | Re-export | Crate pulled in |
| --- | --- | --- |
| *(always on)* | `paigasus_helikon::core` | `paigasus-helikon-core` |
| `macros` | `paigasus_helikon::macros`, `paigasus_helikon::tool`, `paigasus_helikon::tools` | `paigasus-helikon-macros` |
| `openai` *(alias `providers-openai`)* | `paigasus_helikon::openai` | `paigasus-helikon-providers-openai` |
| `anthropic` | `paigasus_helikon::anthropic` | `paigasus-helikon-providers-anthropic` |
| `bedrock` | `paigasus_helikon::bedrock` | `paigasus-helikon-providers-bedrock` |
| `gemini` | `paigasus_helikon::gemini` | `paigasus-helikon-providers-gemini` |
| `litellm` | `paigasus_helikon::litellm` | `paigasus-helikon-providers-litellm` |
| `mcp` | `paigasus_helikon::mcp` | `paigasus-helikon-mcp` |
| `tools` | `paigasus_helikon::tools` | `paigasus-helikon-tools` |
| `tools-web` | adds `WebFetch`/`WebSearch` | enables `paigasus-helikon-tools/web` |
| `sessions-sqlite` | `paigasus_helikon::sessions_sqlite` | `paigasus-helikon-sessions-sqlite` |
| `sessions-postgres` | `paigasus_helikon::sessions_postgres` | `paigasus-helikon-sessions-postgres` |
| `sessions-redis` | `paigasus_helikon::sessions_redis` | `paigasus-helikon-sessions-redis` |
| `runtime-tokio` | `paigasus_helikon::runtime_tokio` | `paigasus-helikon-runtime-tokio` |
| `runtime-axum` | `paigasus_helikon::runtime_axum` | `paigasus-helikon-runtime-axum` |
| `runtime-actix` | `paigasus_helikon::runtime_actix` | `paigasus-helikon-runtime-actix` |
| `runtime-temporal` | `paigasus_helikon::runtime_temporal` | `paigasus-helikon-runtime-temporal` |
| `runtime-agentcore` | `paigasus_helikon::runtime_agentcore` | `paigasus-helikon-runtime-agentcore` |
| `evals` | `paigasus_helikon::evals` | `paigasus-helikon-evals` |

Feature names are kebab-case (`tools-web`, `runtime-tokio`); the re-export module aliases are snake-case (`runtime_tokio`, `sessions_sqlite`).

Two distinct items share the path `paigasus_helikon::tools`. With the `macros` feature it is the `tools!` macro; with the `tools` feature it is the sandboxed-tools crate module. They live in different namespaces, so Rust resolves them by use site (a `tools!(...)` macro call vs. a `tools::` path) — but be explicit about which you mean.

The facade also exposes the `paigasus_helikon::schema::strict()` function, the JSON-Schema strict-mode normalizer (`fn strict(value: &Value) -> Value`), independent of any feature.
