# paigasus-helikon

Paigasus AI SDK — codename **Helikon**. A Rust SDK for building AI agents with pluggable providers, runtimes, and tools.

[![CI](https://github.com/SMK1085/paigasus-helikon/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/SMK1085/paigasus-helikon/actions/workflows/ci.yml)
[![MSRV](https://img.shields.io/badge/rust-1.94%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg)](#license)
[![crates.io](https://img.shields.io/crates/v/paigasus-helikon.svg)](https://crates.io/crates/paigasus-helikon)
[![docs.rs](https://docs.rs/paigasus-helikon/badge.svg)](https://docs.rs/paigasus-helikon)

## What it is

`paigasus-helikon` is a Rust SDK for building agentic AI systems. It separates the slow-moving primitives (types, traits, message protocols) from the fast-moving parts (provider SDKs, execution runtimes, tool catalogs), so downstream projects can pick the surface they need without dragging in the rest.

The SDK does not pick a deployment story, a hosting story, or an observability stack for you. Bring your own.

## The codename

In Greek myth, Mount Helicon (Greek: Ἑλικῶν, *Helikōn*) is the home of the Muses. When Pegasus struck the mountainside with his hoof, the **Hippocrene** spring burst forth — the literal source of poetic inspiration that the Muses drew from.

Paigasus is the umbrella; Helikon is the spring. The SDK is the artifact you draw from when building agents on top.

## Install

```bash
cargo add paigasus-helikon --features openai,macros
```

Turn on the features you need — `openai`, `anthropic`, `bedrock`, `gemini`, `litellm`, `mcp`, `tools`, `tools-web`, `tools-os-sandbox`, `tools-microvm`, `sessions-sqlite`, `sessions-postgres`, `sessions-redis`, `runtime-tokio`, `runtime-axum`, `runtime-actix`, `runtime-temporal`, `runtime-agentcore`, `evals`, `macros`. See the [crate roster](https://smk1085.github.io/paigasus-helikon/reference/crates.html) for the full feature → crate map and current published versions.

## Workspace at a glance

Twenty-one crates under `crates/`. Twenty are published to crates.io; one is an internal test harness (`publish = false`); one of the published crates (the CLI) is binary-only, publishing a lib target with no stability guarantees purely so `cargo install` resolves.

- **`paigasus-helikon`** — facade re-exporting `core` plus opt-in sibling crates by feature flag.
- **`paigasus-helikon-core`** — type system, traits, the agent loop, runtime-agnostic primitives.
- **`paigasus-helikon-macros`** — the `#[tool]` attribute and `tools!` proc macros.
- **`paigasus-helikon-providers-openai`**, **`-anthropic`**, **`-bedrock`**, **`-gemini`**, **`-litellm`** — LLM provider adapters.
- **`paigasus-helikon-sessions-sqlite`** — SQLite-backed session persistence.
- **`paigasus-helikon-sessions-postgres`** — PostgreSQL-backed session persistence (JSONB event log, advisory-lock concurrency, aws-lc-rs TLS).
- **`paigasus-helikon-sessions-redis`** — Redis Streams-backed session persistence (atomic Lua append, BYO-`ConnectionManager` for TLS).
- **`paigasus-helikon-runtime-tokio`** — the default ephemeral Tokio runner.
- **`paigasus-helikon-runtime-axum`** — self-hosted HTTP/SSE/WebSocket agent server (`AgentServer` builder, 6 endpoints: one-shot JSON, SSE streaming, async detached, WebSocket event replay, agent list, OpenAPI schema; replayable runs with TTL+count retention).
- **`paigasus-helikon-runtime-actix`** — API-identical actix-web port of `runtime-axum`, for embedding into an existing actix-web service; same routes and wire format. The differences are the mount seam (`configure()` vs `router()`), listener type, entrypoint attribute, one-shot client-disconnect semantics, and the request type its extension traits take (`AuthLayer`/`ContextProvider` receive an `actix_web::HttpRequest` rather than axum request parts).
- **`paigasus-helikon-runtime-temporal`** — durable runner over the official Temporal Rust SDK (`TemporalRunner`; per-model-turn and per-tool-call activities; a run that crashes mid-tool-call resumes from the last completed activity).
- **`paigasus-helikon-runtime-agentcore`** — AWS Bedrock AgentCore container shim (`AgentCoreServer`; all four container protocols — HTTP on 8080 with an optional WebSocket, MCP on 8000, A2A on 9000, AG-UI on 8080; ships a multi-stage Dockerfile and CDK deployment snippet).
- **`paigasus-helikon-tools`** — sandboxed Read/Write/Edit/Bash tools (+ `WebFetch`/`WebSearch` behind `web`; OS-enforced containment behind `os-sandbox`; microVM containment via forkd/Firecracker behind `microvm`, experimental — SMA-437: includes `EgressProxy`, `EgressPolicy`, and `Isolation::Proxied` for domain-filtered egress enforcement).
- **`paigasus-helikon-mcp`** — Model Context Protocol client and server integration.
- **`paigasus-helikon-cli`** — published binary crate: `helikon` and `paigasus-helikon` binaries (`cargo install paigasus-helikon-cli`) with `repl`, `eval run`, and `mcp serve` subcommands driven by an `agents.toml` sidecar; its lib target publishes only so `cargo install` works, and carries no stability guarantee.
- **`paigasus-helikon-evals`** — evaluation harness: JSONL datasets, an `Evaluator` trait with four built-ins (`ExactMatch`, `JsonSchemaConformance`, `LlmJudge`, `ToolUseTrajectory`), a `MockModel` for deterministic replay, and SQLite/Parquet trace sinks.

See the [crate roster](https://smk1085.github.io/paigasus-helikon/reference/crates.html) for each crate's concern, published state, and current version.

## Documentation

The public documentation site is published at <https://smk1085.github.io/paigasus-helikon/> — a guided mdBook covering the [quickstart](https://smk1085.github.io/paigasus-helikon/getting-started/quickstart.html), the core concepts, and the [crate roster](https://smk1085.github.io/paigasus-helikon/reference/crates.html).

To build it locally: `cd docs/book && mdbook serve` (requires `mdbook` and `mdbook-linkcheck` installed via `cargo install`; see [CONTRIBUTING.md](./CONTRIBUTING.md#documentation-site) for exact versions).

The architectural source-of-truth currently lives in Notion (internal): ["Crate Reference"](https://www.notion.so/355830e8fbaa813c92e8c1aa9985fd3f). External readers should treat the Notion link as an artifact pointer rather than a destination — content migrates into the published book as the SDK lands.

Tracked work lives in Linear under the project **Paigasus Helikon** (issues are prefixed `SMA-`).

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for branching, testing, and release workflows. By participating you agree to the [Contributor Covenant Code of Conduct](./CODE_OF_CONDUCT.md). For security disclosures see [SECURITY.md](./SECURITY.md).

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](./LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](./LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
