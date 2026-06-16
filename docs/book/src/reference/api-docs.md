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
- [`paigasus-helikon-mcp`](https://docs.rs/paigasus-helikon-mcp) — `rmcp`-based MCP client/server wrapper.
- [`paigasus-helikon-tools`](https://docs.rs/paigasus-helikon-tools) — sandboxed `Read`/`Write`/`Edit`/`Bash` tools (plus `WebFetch`/`WebSearch` behind the `web` feature).

Most users depend only on the `paigasus-helikon` facade and enable the features they need; the facade docs link out to each sibling. Crate versions move every release — see [Crate overview](./crates.md) for the current numbers.

## Not yet published

`paigasus-helikon-evals`, `paigasus-helikon-runtime-axum`, `paigasus-helikon-runtime-temporal`, and `paigasus-helikon-runtime-agentcore` are `0.0.0` name-claim stubs with no implementation yet, so they have no live docs.rs pages. `paigasus-helikon-cli` is a binary and is never published as a library.

## Building locally

```bash
cargo doc --workspace --all-features --no-deps --open
```
