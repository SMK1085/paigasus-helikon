# API docs

Per-item Rust API documentation is published on [docs.rs](https://docs.rs). This book covers concepts and worked examples; docs.rs is the source of truth for every type, trait, method, and feature flag. For a higher-level map of which crate owns which concern, see [Crate overview](./crates.md).

## Published crates

- [`paigasus-helikon`](https://docs.rs/paigasus-helikon) — the facade; re-exports `core` and the feature-gated siblings.
- [`paigasus-helikon-core`](https://docs.rs/paigasus-helikon-core) — trait surface, agent loop, event stream, carrier types (the dependency root).
- [`paigasus-helikon-macros`](https://docs.rs/paigasus-helikon-macros) — the `#[tool]` attribute and `tools!` proc macros.
- [`paigasus-helikon-providers-openai`](https://docs.rs/paigasus-helikon-providers-openai) — OpenAI model adapter.
- [`paigasus-helikon-providers-anthropic`](https://docs.rs/paigasus-helikon-providers-anthropic) — Anthropic model adapter.
- [`paigasus-helikon-sessions-sqlite`](https://docs.rs/paigasus-helikon-sessions-sqlite) — SQLite `Session` backend.
- [`paigasus-helikon-runtime-tokio`](https://docs.rs/paigasus-helikon-runtime-tokio) — ephemeral Tokio runner.
- [`paigasus-helikon-runtime-axum`](https://docs.rs/paigasus-helikon-runtime-axum) — self-hosted HTTP/SSE/WebSocket agent server (`AgentServer` builder, 6 endpoints, replayable runs). See [Axum Server Runtime](../concepts/axum-server.md).
- [`paigasus-helikon-runtime-temporal`](https://docs.rs/paigasus-helikon-runtime-temporal) — durable Temporal-backed runner (`TemporalRunner`, crash-resume via Temporal history replay). See [Runtimes](../concepts/runtimes.md).
- [`paigasus-helikon-runtime-agentcore`](https://docs.rs/paigasus-helikon-runtime-agentcore) — AWS Bedrock AgentCore container shim (`AgentCoreServer`, HTTP + MCP protocol contract). See [Runtimes](../concepts/runtimes.md).
- [`paigasus-helikon-mcp`](https://docs.rs/paigasus-helikon-mcp) — `rmcp`-based MCP client/server wrapper.
- [`paigasus-helikon-tools`](https://docs.rs/paigasus-helikon-tools) — sandboxed `Read`/`Write`/`Edit`/`Bash` tools (plus `WebFetch`/`WebSearch` behind the `web` feature).
- [`paigasus-helikon-evals`](https://docs.rs/paigasus-helikon-evals) — evaluation harness: JSONL datasets, the `Evaluator` trait with four built-ins, `MockModel`, and SQLite/Parquet trace sinks. See [Observability & Evaluation](../concepts/observability-evaluation.md).
- [`paigasus-helikon-cli`](https://docs.rs/paigasus-helikon-cli) — `helikon` / `paigasus-helikon` CLI binaries; publishes a lib target purely so `cargo install paigasus-helikon-cli` resolves, but that lib is internal and carries no stability guarantee. The binaries are documented in the [CLI reference](./cli.md) rather than docs.rs.

Most users depend only on the `paigasus-helikon` facade and enable the features they need; the facade docs link out to each sibling. Crate versions move every release — see [Crate overview](./crates.md) for the current numbers.

## Publish status

All 18 non-internal crates now publish to crates.io — the last two stubs (`paigasus-helikon-evals`, `paigasus-helikon-cli`) ascended to real implementations in SMA-332/SMA-333, following `-runtime-axum`, `-runtime-temporal`, and `-runtime-agentcore` before them. The lone exception is `paigasus-helikon-sessions-testkit`, an internal `Session` conformance test harness that is `publish = false` by design, not a stub awaiting an ascend.

## Building locally

```bash
cargo doc --workspace --all-features --no-deps --open
```
