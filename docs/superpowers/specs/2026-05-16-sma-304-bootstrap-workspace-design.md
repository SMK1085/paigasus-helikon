# SMA-304 — Bootstrap Cargo workspace skeleton

**Linear issue**: [SMA-304](https://linear.app/smaschek/issue/SMA-304/bootstrap-github-repo-and-cargo-workspace-skeleton)
**Status**: design approved 2026-05-16
**Branch**: `feature/sma-304-bootstrap-github-repo-and-cargo-workspace-skeleton`

## 1. Goal & non-goals

**Goal.** Deliver a Cargo workspace such that `cargo build --workspace` and `cargo doc --workspace --all-features --no-deps` succeed locally and in CI, with a directory structure and metadata that subsequent SMA-* issues can drop into without reshaping.

**Non-goals.** No real implementations of any trait, type, provider, runtime, or tool. No public API surface beyond the facade's feature-gate stubs. No tests beyond what `cargo build` and `cargo doc` already enforce.

## 2. Final repository layout

```
paigasus-helikon/
├── .editorconfig
├── .github/workflows/ci.yml
├── .gitignore
├── CLAUDE.md                          (existing; expanded post-bootstrap)
├── Cargo.lock                         (committed — workspace has a binary)
├── Cargo.toml                         (workspace root)
├── LICENSE                            (existing MIT, untouched)
├── README.md                          (workspace-level, minimal)
├── rust-toolchain.toml
└── crates/
    ├── paigasus-helikon/              (facade)
    ├── paigasus-helikon-core/
    ├── paigasus-helikon-macros/       (proc-macro crate)
    ├── paigasus-helikon-providers-openai/
    ├── paigasus-helikon-providers-anthropic/
    ├── paigasus-helikon-mcp/
    ├── paigasus-helikon-tools/
    ├── paigasus-helikon-runtime-tokio/
    ├── paigasus-helikon-runtime-axum/
    ├── paigasus-helikon-runtime-temporal/
    ├── paigasus-helikon-runtime-agentcore/
    ├── paigasus-helikon-evals/
    └── paigasus-helikon-cli/          (binary: helikon, paigasus-helikon)
```

Each `crates/<name>/` contains a `Cargo.toml` (with workspace inheritance), a `src/lib.rs` (or `src/bin/*.rs` for the CLI), and a one-line `README.md`.

## 3. Workspace `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version       = "0.0.0"
edition       = "2021"
rust-version  = "1.75"
authors       = ["Sven Maschek"]
license       = "MIT"
repository    = "https://github.com/SMK1085/paigasus-helikon"
homepage      = "https://github.com/SMK1085/paigasus-helikon"
keywords      = ["ai", "llm", "agents", "sdk", "openai"]
categories    = ["api-bindings", "asynchronous"]

[workspace.dependencies]
# Third-party (pinned per SMA-304)
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"
schemars      = "1"
tokio         = { version = "1", features = ["full"] }
tracing       = "0.1"
opentelemetry = "0.27"
rmcp          = "0.16"
thiserror     = "2"
anyhow        = "1"
async-trait   = "0.1"

# Internal — single source of truth so members depend via `workspace = true`
paigasus-helikon-core                = { path = "crates/paigasus-helikon-core",                version = "0.0.0" }
paigasus-helikon-macros              = { path = "crates/paigasus-helikon-macros",              version = "0.0.0" }
paigasus-helikon-providers-openai    = { path = "crates/paigasus-helikon-providers-openai",    version = "0.0.0" }
paigasus-helikon-providers-anthropic = { path = "crates/paigasus-helikon-providers-anthropic", version = "0.0.0" }
paigasus-helikon-mcp                 = { path = "crates/paigasus-helikon-mcp",                 version = "0.0.0" }
paigasus-helikon-tools               = { path = "crates/paigasus-helikon-tools",               version = "0.0.0" }
paigasus-helikon-runtime-tokio       = { path = "crates/paigasus-helikon-runtime-tokio",       version = "0.0.0" }
paigasus-helikon-runtime-axum        = { path = "crates/paigasus-helikon-runtime-axum",        version = "0.0.0" }
paigasus-helikon-runtime-temporal    = { path = "crates/paigasus-helikon-runtime-temporal",    version = "0.0.0" }
paigasus-helikon-runtime-agentcore   = { path = "crates/paigasus-helikon-runtime-agentcore",   version = "0.0.0" }
paigasus-helikon-evals               = { path = "crates/paigasus-helikon-evals",               version = "0.0.0" }
```

## 4. Facade crate (`crates/paigasus-helikon/`)

`Cargo.toml`:

```toml
[package]
name        = "paigasus-helikon"
description = "Paigasus AI SDK — facade crate. Re-exports core plus feature-gated providers and runtimes."
version.workspace      = true
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[dependencies]
paigasus-helikon-core                = { workspace = true }
paigasus-helikon-macros              = { workspace = true, optional = true }
paigasus-helikon-providers-openai    = { workspace = true, optional = true }
paigasus-helikon-providers-anthropic = { workspace = true, optional = true }
paigasus-helikon-mcp                 = { workspace = true, optional = true }
paigasus-helikon-tools               = { workspace = true, optional = true }
paigasus-helikon-runtime-tokio       = { workspace = true, optional = true }
paigasus-helikon-runtime-axum        = { workspace = true, optional = true }
paigasus-helikon-runtime-temporal    = { workspace = true, optional = true }
paigasus-helikon-runtime-agentcore   = { workspace = true, optional = true }
paigasus-helikon-evals               = { workspace = true, optional = true }

[features]
default            = []
macros             = ["dep:paigasus-helikon-macros"]
openai             = ["dep:paigasus-helikon-providers-openai"]
anthropic          = ["dep:paigasus-helikon-providers-anthropic"]
mcp                = ["dep:paigasus-helikon-mcp"]
tools              = ["dep:paigasus-helikon-tools"]
evals              = ["dep:paigasus-helikon-evals"]
runtime-tokio      = ["dep:paigasus-helikon-runtime-tokio"]
runtime-axum       = ["dep:paigasus-helikon-runtime-axum"]
runtime-temporal   = ["dep:paigasus-helikon-runtime-temporal"]
runtime-agentcore  = ["dep:paigasus-helikon-runtime-agentcore"]
```

`src/lib.rs`:

```rust
#![doc = include_str!("../README.md")]

pub use paigasus_helikon_core as core;

#[cfg(feature = "macros")]            pub use paigasus_helikon_macros as macros;
#[cfg(feature = "openai")]            pub use paigasus_helikon_providers_openai as openai;
#[cfg(feature = "anthropic")]         pub use paigasus_helikon_providers_anthropic as anthropic;
#[cfg(feature = "mcp")]               pub use paigasus_helikon_mcp as mcp;
#[cfg(feature = "tools")]             pub use paigasus_helikon_tools as tools;
#[cfg(feature = "evals")]             pub use paigasus_helikon_evals as evals;
#[cfg(feature = "runtime-tokio")]     pub use paigasus_helikon_runtime_tokio as runtime_tokio;
#[cfg(feature = "runtime-axum")]      pub use paigasus_helikon_runtime_axum as runtime_axum;
#[cfg(feature = "runtime-temporal")]  pub use paigasus_helikon_runtime_temporal as runtime_temporal;
#[cfg(feature = "runtime-agentcore")] pub use paigasus_helikon_runtime_agentcore as runtime_agentcore;
```

## 5. Member crate stubs

For every non-facade, non-CLI, non-macros crate, the `Cargo.toml` is identical-modulo-name-and-description:

```toml
[package]
name        = "<name>"
description = "<one-line purpose from Crate Reference>"
version.workspace      = true
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true
```

`src/lib.rs` for each:

```rust
//! <crate name> — stub. See SMA-304.
```

### 5.1 `paigasus-helikon-macros` (proc-macro)

Add to its `Cargo.toml`:

```toml
[lib]
proc-macro = true
```

Stub `src/lib.rs` is empty (proc-macro crates don't need to export anything to compile).

### 5.2 `paigasus-helikon-cli` (binary)

```toml
[package]
name     = "paigasus-helikon-cli"
autobins = false   # rely solely on explicit [[bin]] entries below
# ... shared workspace fields ...

[[bin]]
name = "helikon"
path = "src/bin/helikon.rs"

[[bin]]
name = "paigasus-helikon"
path = "src/bin/paigasus_helikon.rs"
```

`autobins = false` is required because the binary name `paigasus-helikon` (hyphen) doesn't match the filename `paigasus_helikon.rs` (underscore — hyphens aren't valid in Rust filenames). Without it, Cargo's auto-discovery would also produce a binary named `paigasus_helikon` and conflict.

Each `src/bin/*.rs` is:

```rust
fn main() {
    println!("paigasus-helikon-cli stub (SMA-304)");
}
```

No `src/lib.rs` is needed.

### 5.3 Per-crate descriptions (from the Crate Reference)

| Crate | Description |
|---|---|
| `paigasus-helikon-core` | Trait surface and concrete types for the Paigasus Helikon AI SDK. |
| `paigasus-helikon-macros` | Proc macros for the Paigasus Helikon AI SDK. |
| `paigasus-helikon-providers-openai` | OpenAI provider for the Paigasus Helikon AI SDK. |
| `paigasus-helikon-providers-anthropic` | Anthropic provider for the Paigasus Helikon AI SDK. |
| `paigasus-helikon-mcp` | MCP client and server integration for the Paigasus Helikon AI SDK. |
| `paigasus-helikon-tools` | Sandboxed Read/Write/Bash/WebFetch tools for the Paigasus Helikon AI SDK. |
| `paigasus-helikon-runtime-tokio` | Default ephemeral Tokio runner for the Paigasus Helikon AI SDK. |
| `paigasus-helikon-runtime-axum` | Self-hosted Axum runtime for the Paigasus Helikon AI SDK. |
| `paigasus-helikon-runtime-temporal` | Temporal-backed durable runtime for the Paigasus Helikon AI SDK. |
| `paigasus-helikon-runtime-agentcore` | AWS Bedrock AgentCore runtime for the Paigasus Helikon AI SDK. |
| `paigasus-helikon-evals` | Evaluation harness for the Paigasus Helikon AI SDK. |
| `paigasus-helikon-cli` | CLI binaries (`helikon`, `paigasus-helikon`) for the Paigasus Helikon AI SDK. |

## 6. Toolchain & ignore files

`rust-toolchain.toml`:

```toml
[toolchain]
channel    = "stable"
components = ["rustfmt", "clippy"]
```

`.gitignore`:

```
/target
```

`Cargo.lock` is committed (workspace contains a binary).

`.editorconfig`:

```
root = true

[*]
charset = utf-8
end_of_line = lf
insert_final_newline = true
trim_trailing_whitespace = true
indent_style = space
indent_size = 4

[*.{md,yml,yaml,toml}]
indent_size = 2
```

## 7. CI workflow

`.github/workflows/ci.yml`:

```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo build --workspace
      - run: cargo doc --workspace --all-features --no-deps
        env:
          RUSTDOCFLAGS: "-D warnings"
```

`--no-deps` keeps doc generation focused on workspace crates. `RUSTDOCFLAGS=-D warnings` makes broken intra-doc links fail CI as soon as real code lands.

## 8. Verification (post-implementation)

The following must all succeed before SMA-304 can close:

```
cargo build --workspace
cargo doc --workspace --all-features --no-deps
cargo run -p paigasus-helikon-cli --bin helikon
cargo run -p paigasus-helikon-cli --bin paigasus-helikon
```

Plus: the CI workflow must run green on the feature branch before merge.

## 9. Risks & deviations from SMA-304

1. **License**: SMA-304's task list says `Apache-2.0 OR MIT`. Per direct user instruction on 2026-05-16, the project is MIT-only. SMA-304's description has been updated to match. `LICENSE` (MIT) is left untouched.
2. **MSRV 1.75 vs pinned deps**: `opentelemetry 0.27` and `rmcp 0.16+` may require a newer MSRV. If `cargo build` fails on MSRV grounds, the actual required version is surfaced and `rust-version` is bumped — no silent bumps.
3. **Branch**: implementation goes on `feature/sma-304-bootstrap-github-repo-and-cargo-workspace-skeleton` (Linear's pre-computed branch name), not `main`.
4. **`paigasus-helikon-macros` declared as proc-macro from day one**: The Crate Reference is unambiguous about its purpose. Stubbing it as `proc-macro = true` now avoids a back-revision the moment the first macro lands. An empty proc-macro crate compiles fine.
5. **Acceptance criterion #3 ("Repo URL captured in the project's Resources")**: Linear/Notion housekeeping, not a code task. Handled as a manual step at the end of execution.
