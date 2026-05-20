# paigasus-helikon

Paigasus AI SDK — codename **Helikon**. A Rust SDK for building AI agents with pluggable providers, runtimes, and tools.

[![CI](https://github.com/SMK1085/paigasus-helikon/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/SMK1085/paigasus-helikon/actions/workflows/ci.yml)
[![MSRV](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg)](#license)
<!-- TODO(post-publish): add crates.io badge once the workspace is published -->
<!-- TODO(post-publish): add docs.rs badge once the workspace is published -->

## What it is

`paigasus-helikon` is a Rust SDK for building agentic AI systems. It separates the slow-moving primitives (types, traits, message protocols) from the fast-moving parts (provider SDKs, execution runtimes, tool catalogs), so downstream projects can pick the surface they need without dragging in the rest.

The SDK does not pick a deployment story, a hosting story, or an observability stack for you. Bring your own.

## The codename

In Greek myth, Mount Helicon (Greek: Ἑλικῶν, *Helikōn*) is the home of the Muses. When Pegasus struck the mountainside with his hoof, the **Hippocrene** spring burst forth — the literal source of poetic inspiration that the Muses drew from.

Paigasus is the umbrella; Helikon is the spring. The SDK is the artifact you draw from when building agents on top.

## Install

```toml
[dependencies]
paigasus-helikon = { version = "0.1", features = ["openai", "anthropic", "mcp", "runtime-tokio"] }
```

> Pre-release: the workspace currently pins `version = "0.0.0"` and is not yet published to crates.io. The `"0.1"` shown above is the planned first published release — replace with the actual published version once available.

## Workspace at a glance

Thirteen crates under `crates/`:

- **`paigasus-helikon`** — facade re-exporting `core` plus opt-in sibling crates by feature flag.
- **`paigasus-helikon-core`** — type system, traits, runtime-agnostic primitives.
- **`paigasus-helikon-cli`** — `helikon` and `paigasus-helikon` binaries.
- **`paigasus-helikon-macros`** — proc-macro crate (currently empty).
- **`paigasus-helikon-providers-openai`**, **`-anthropic`** — LLM provider implementations.
- **`paigasus-helikon-runtime-tokio`**, **`-axum`**, **`-temporal`**, **`-agentcore`** — execution / orchestration runtimes.
- **`paigasus-helikon-tools`** — tool-calling primitives.
- **`paigasus-helikon-mcp`** — Model Context Protocol integration.
- **`paigasus-helikon-evals`** — evaluation harness.

## Documentation

The architectural reference lives in Notion: ["Crate Reference"](https://www.notion.so/355830e8fbaa813c92e8c1aa9985fd3f). An mdBook-hosted equivalent will replace the Notion page once published.

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
