# Crate overview

The workspace is **21 crates** under `crates/`, all named `paigasus-helikon-*` (plus the `paigasus-helikon` facade itself). This page is the version-bearing map: one row per crate, what it owns, whether it is published, and how the crates depend on each other.

For orientation — how to pick crates and add them to your `Cargo.toml` — see [workspace layout](../getting-started/workspace-layout.md). For the rendered rustdoc, see [API docs](./api-docs.md).

## Dependency direction

- `paigasus-helikon-core` is the root: it owns the trait surface, the agent loop, the event stream, and the carrier types. It depends on no other workspace crate.
- The provider, session, tool, MCP, and runtime crates each depend on `core` and on nothing else in the workspace (`-tools` carries a path-only dev-dep on `-providers-openai` for an example; it is stripped from the published manifest).
- `paigasus-helikon-macros` is a proc-macro crate; its `#[tool]` expansion targets `core` types in the consumer's crate.
- `paigasus-helikon` is the **facade**: it re-exports `core` unconditionally and the sibling crates behind Cargo features. Application crates normally depend on the facade alone and turn on the features they need.
- `paigasus-helikon-cli` consumes `core` and the sibling crates it needs (`-evals`, `-runtime-tokio`, `-providers-openai`, `-providers-anthropic`, `-mcp`) directly — not the facade. It publishes to crates.io at `0.1.0` as a binary crate; its lib target is internal (`missing_docs` opted out) and carries no stability guarantee, publishing only so `cargo install paigasus-helikon-cli` resolves.

## Crate table

Versions below are **current as of 2026-07-06** and move every release — read each crate's `Cargo.toml` (or the root `[workspace.dependencies]` pins) for the live numbers, and the [crates.io page](https://crates.io/crates/paigasus-helikon) / docs.rs for what is actually published.

| Crate | Concern | State | Version |
| --- | --- | --- | --- |
| [`paigasus-helikon-core`](https://docs.rs/paigasus-helikon-core) | Trait surface, agent loop, event stream, carrier types — the dependency root | published | `0.5.4` |
| [`paigasus-helikon`](https://docs.rs/paigasus-helikon) | Facade — re-exports `core` always, siblings behind features | published | `0.4.14` |
| [`paigasus-helikon-macros`](https://docs.rs/paigasus-helikon-macros) | `#[tool]` attribute and `tools!` proc macros | published | `0.2.2` |
| [`paigasus-helikon-providers-openai`](https://docs.rs/paigasus-helikon-providers-openai) | OpenAI model adapter (`OpenAiModel`) | published | `0.2.9` |
| [`paigasus-helikon-providers-anthropic`](https://docs.rs/paigasus-helikon-providers-anthropic) | Anthropic model adapter (`AnthropicModel`) | published | `0.1.10` |
| [`paigasus-helikon-providers-bedrock`](https://docs.rs/paigasus-helikon-providers-bedrock) | Amazon Bedrock Converse API model adapter (`BedrockModel`) | published | `0.1.0` |
| [`paigasus-helikon-providers-gemini`](https://docs.rs/paigasus-helikon-providers-gemini) | Google Gemini model adapter (`GeminiModel`; Developer API + Vertex AI) | published | `0.1.0` |
| [`paigasus-helikon-providers-litellm`](https://docs.rs/paigasus-helikon-providers-litellm) | LiteLLM proxy adapter (`LiteLlmModel`; OpenAI-compatible gateway) | published | `0.1.0` |
| [`paigasus-helikon-sessions-sqlite`](https://docs.rs/paigasus-helikon-sessions-sqlite) | SQLite-backed `Session` backend | published | `0.1.11` |
| [`paigasus-helikon-sessions-postgres`](https://docs.rs/paigasus-helikon-sessions-postgres) | PostgreSQL-backed `Session` backend (`PostgresSession`) | published | `0.1.0` |
| [`paigasus-helikon-sessions-redis`](https://docs.rs/paigasus-helikon-sessions-redis) | Redis Streams-backed `Session` backend (`RedisSession`) | published | `0.1.0` |
| [`paigasus-helikon-runtime-tokio`](https://docs.rs/paigasus-helikon-runtime-tokio) | Default ephemeral Tokio runner | published | `0.1.9` |
| [`paigasus-helikon-runtime-axum`](https://docs.rs/paigasus-helikon-runtime-axum) | Self-hosted Axum HTTP/SSE/WebSocket agent server (`AgentServer` builder, 6 endpoints, replayable runs) | published | `0.1.0` |
| [`paigasus-helikon-runtime-actix`](https://docs.rs/paigasus-helikon-runtime-actix) | Self-hosted actix-web HTTP/SSE/WebSocket agent server (same public surface as `runtime-axum`; embed into an existing actix-web app) | published | `0.1.0` |
| [`paigasus-helikon-runtime-temporal`](https://docs.rs/paigasus-helikon-runtime-temporal) | Durable Temporal-backed runner (`TemporalRunner`; crash-resume via Temporal history replay) | published | `0.3.0` |
| [`paigasus-helikon-runtime-agentcore`](https://docs.rs/paigasus-helikon-runtime-agentcore) | AWS Bedrock AgentCore container shim (`AgentCoreServer`; HTTP + MCP protocol contract) | published | `0.1.0` |
| [`paigasus-helikon-mcp`](https://docs.rs/paigasus-helikon-mcp) | MCP integration — `rmcp` client and server wrappers | published | `0.1.3` |
| [`paigasus-helikon-tools`](https://docs.rs/paigasus-helikon-tools) | Sandboxed Read/Write/Edit/Bash tools (+ `WebFetch`/`WebSearch` behind `web`) | published | `0.1.5` |
| [`paigasus-helikon-evals`](https://docs.rs/paigasus-helikon-evals) | Evaluation harness — datasets, evaluators, `MockModel`, SQLite/Parquet trace sinks | published | `0.1.0` |
| [`paigasus-helikon-cli`](https://docs.rs/paigasus-helikon-cli) | `helikon` / `paigasus-helikon` CLI binaries; lib target is internal, no stability guarantee | published (binary crate) | `0.1.0` |
| `paigasus-helikon-sessions-testkit` | Shared `Session` conformance test harness (internal — never published) | internal — `publish = false` | `0.0.0` |

Every crate above the `-sessions-testkit` row publishes to crates.io — the last two stubs (`-evals`, `-cli`) ascended to real implementations in SMA-332/SMA-333, following the four-remaining-crates ascend before them (`-runtime-axum`, `-runtime-temporal`, `-runtime-agentcore`). `paigasus-helikon-sessions-testkit` is the sole `publish = false` crate under `crates/`, and it is an intentional internal test harness rather than a stub awaiting an ascend.

The workspace has one further member outside `crates/`: `paigasus-helikon-runtime-http-conformance` (under `tests/runtime-http-conformance/`, `version = "0.0.0"`, `publish = false`) — an internal axum⇔actix wire-format conformance suite exercising both HTTP runtimes against the same test cases. Like `-sessions-testkit`, it is an intentional non-published test harness, not a stub.

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
