# SMA-304 Bootstrap Cargo Workspace — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lay out the `paigasus-helikon` Cargo workspace so `cargo build --workspace` and `cargo doc --workspace --all-features --no-deps` succeed and subsequent SMA-* issues have a place to land.

**Architecture:** A workspace at the repo root with 13 member crates under `crates/`. Twelve are stubs; the 13th is a facade that uses Cargo features to gate `pub use` re-exports of each provider/runtime/extension crate. No real implementations land in this plan — only the structural skeleton needed to compile.

**Tech Stack:** Rust 2021 edition, MSRV 1.75 (will bump if a pinned dep requires more), Cargo workspace inheritance, GitHub Actions for CI, `proc-macro` crate type for the macros stub, MIT license throughout.

**Spec:** [`docs/superpowers/specs/2026-05-16-sma-304-bootstrap-workspace-design.md`](../specs/2026-05-16-sma-304-bootstrap-workspace-design.md)

**Linear:** [SMA-304](https://linear.app/smaschek/issue/SMA-304/bootstrap-github-repo-and-cargo-workspace-skeleton)

---

## Definition of Done

The plan is complete when all four commands succeed locally on the feature branch:

```
cargo build --workspace
cargo doc --workspace --all-features --no-deps
cargo run -p paigasus-helikon-cli --bin helikon
cargo run -p paigasus-helikon-cli --bin paigasus-helikon
```

…and the CI workflow runs green on a push of the feature branch.

## Conventions used in this plan

- **Commit prefix**: `<type>(<scope>): SMA-304 <message>`. Example: `feat(workspace): SMA-304 add core crate stub`.
- **Branch**: all work happens on `feature/sma-304-bootstrap-github-repo-and-cargo-workspace-skeleton`.
- **No remote pushes** are performed by the executing agent. The final task surfaces a "ready to push" prompt; the user runs `git push` and any PR creation themselves.
- **MSRV failures**: if any `cargo build` step fails with errors mentioning `rust-version`, MSRV, or "requires Rust X.Y", bump `rust-version` in `Cargo.toml`'s `[workspace.package]` to the version cargo demands and re-run. Do not silently downgrade dep pins.

---

### Task 1: Switch to the feature branch

**Files:** none (git operation only).

- [ ] **Step 1: Verify current branch is `main` and clean**

Run: `git -C /Users/smaschek/dev/paigasus/paigasus-helikon status --short --branch`

Expected: `## main` (with possibly `?? CLAUDE.md` from prior session — that's fine, leave it untracked).

- [ ] **Step 2: Create and check out the feature branch**

Run:
```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon checkout -b feature/sma-304-bootstrap-github-repo-and-cargo-workspace-skeleton
```

Expected: `Switched to a new branch 'feature/sma-304-bootstrap-github-repo-and-cargo-workspace-skeleton'`.

- [ ] **Step 3: Confirm**

Run: `git -C /Users/smaschek/dev/paigasus/paigasus-helikon rev-parse --abbrev-ref HEAD`

Expected: `feature/sma-304-bootstrap-github-repo-and-cargo-workspace-skeleton`.

No commit in this task.

---

### Task 2: Repo-level boilerplate

**Files:**
- Create: `.gitignore`
- Create: `.editorconfig`
- Create: `rust-toolchain.toml`
- Create: `README.md`

- [ ] **Step 1: Create `.gitignore`**

```
/target
```

- [ ] **Step 2: Create `.editorconfig`**

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

- [ ] **Step 3: Create `rust-toolchain.toml`**

```toml
[toolchain]
channel    = "stable"
components = ["rustfmt", "clippy"]
```

- [ ] **Step 4: Create `README.md`**

```markdown
# paigasus-helikon

Paigasus AI SDK — codename **Helikon**. A Rust SDK for building AI agents with pluggable providers, runtimes, and tools.

This repository hosts the Cargo workspace. Add the SDK to a downstream project with:

```toml
[dependencies]
paigasus-helikon = { version = "0.1", features = ["openai", "anthropic", "mcp", "runtime-tokio"] }
```

Crates are versioned together. See `crates/` for the workspace layout.

## License

MIT — see [LICENSE](./LICENSE).
```

- [ ] **Step 5: Stage and commit**

```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon add .gitignore .editorconfig rust-toolchain.toml README.md
git -C /Users/smaschek/dev/paigasus/paigasus-helikon commit -m "chore(repo): SMA-304 add gitignore, editorconfig, toolchain pin, and README"
```

---

### Task 3: Workspace `Cargo.toml` skeleton (no internal deps yet)

**Files:**
- Create: `Cargo.toml` (workspace root)

The workspace declares external dep pins now and `members = ["crates/*"]`. Internal crates will be added to `[workspace.dependencies]` in Task 11 (the facade task), once those crate paths actually exist on disk.

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members  = ["crates/*"]

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
```

- [ ] **Step 2: Create empty `crates/` directory marker**

Run: `mkdir -p /Users/smaschek/dev/paigasus/paigasus-helikon/crates`

(Empty directory; `members = ["crates/*"]` matches nothing yet, which Cargo treats as an empty workspace.)

- [ ] **Step 3: Verify `cargo build --workspace` succeeds with no members**

Run from the repo root: `cd /Users/smaschek/dev/paigasus/paigasus-helikon && cargo build --workspace`

Expected: `Finished \`dev\` profile [unoptimized + debuginfo] target(s)` with nothing to compile.

If this fails with an MSRV error, bump `rust-version` per the convention noted at the top of this plan.

- [ ] **Step 4: Stage and commit**

```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon add Cargo.toml Cargo.lock
git -C /Users/smaschek/dev/paigasus/paigasus-helikon commit -m "feat(workspace): SMA-304 add workspace Cargo.toml with shared metadata and dep pins"
```

(`Cargo.lock` is generated by the build and committed per the spec — workspace contains a binary.)

---

### Task 4: `paigasus-helikon-core` stub

**Files:**
- Create: `crates/paigasus-helikon-core/Cargo.toml`
- Create: `crates/paigasus-helikon-core/src/lib.rs`
- Create: `crates/paigasus-helikon-core/README.md`

- [ ] **Step 1: Create `crates/paigasus-helikon-core/Cargo.toml`**

```toml
[package]
name        = "paigasus-helikon-core"
description = "Trait surface and concrete types for the Paigasus Helikon AI SDK."
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

- [ ] **Step 2: Create `crates/paigasus-helikon-core/src/lib.rs`**

```rust
//! `paigasus-helikon-core` — stub. See SMA-304.
```

- [ ] **Step 3: Create `crates/paigasus-helikon-core/README.md`**

```markdown
# paigasus-helikon-core

Trait surface and concrete types for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 4: Verify**

Run from repo root: `cargo build --workspace`

Expected: `Compiling paigasus-helikon-core v0.0.0` then `Finished`.

- [ ] **Step 5: Stage and commit**

```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon add crates/paigasus-helikon-core Cargo.lock
git -C /Users/smaschek/dev/paigasus/paigasus-helikon commit -m "feat(core): SMA-304 add paigasus-helikon-core stub crate"
```

---

### Task 5: `paigasus-helikon-macros` stub (proc-macro crate)

**Files:**
- Create: `crates/paigasus-helikon-macros/Cargo.toml`
- Create: `crates/paigasus-helikon-macros/src/lib.rs`
- Create: `crates/paigasus-helikon-macros/README.md`

- [ ] **Step 1: Create `crates/paigasus-helikon-macros/Cargo.toml`**

Note the `[lib] proc-macro = true` declaration — this is the difference vs other crates. Stub it now so later issues don't have to convert it.

```toml
[package]
name        = "paigasus-helikon-macros"
description = "Proc macros for the Paigasus Helikon AI SDK."
version.workspace      = true
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[lib]
proc-macro = true
```

- [ ] **Step 2: Create `crates/paigasus-helikon-macros/src/lib.rs`**

```rust
//! `paigasus-helikon-macros` — stub. See SMA-304.
```

(An empty proc-macro crate compiles fine; we don't need to export any macros yet.)

- [ ] **Step 3: Create `crates/paigasus-helikon-macros/README.md`**

```markdown
# paigasus-helikon-macros

Proc macros for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 4: Verify**

Run from repo root: `cargo build --workspace`

Expected: `Compiling paigasus-helikon-macros v0.0.0` then `Finished`.

- [ ] **Step 5: Stage and commit**

```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon add crates/paigasus-helikon-macros Cargo.lock
git -C /Users/smaschek/dev/paigasus/paigasus-helikon commit -m "feat(macros): SMA-304 add paigasus-helikon-macros proc-macro stub"
```

---

### Task 6: Provider stubs (`openai`, `anthropic`)

**Files:**
- Create: `crates/paigasus-helikon-providers-openai/Cargo.toml`
- Create: `crates/paigasus-helikon-providers-openai/src/lib.rs`
- Create: `crates/paigasus-helikon-providers-openai/README.md`
- Create: `crates/paigasus-helikon-providers-anthropic/Cargo.toml`
- Create: `crates/paigasus-helikon-providers-anthropic/src/lib.rs`
- Create: `crates/paigasus-helikon-providers-anthropic/README.md`

- [ ] **Step 1: Create `crates/paigasus-helikon-providers-openai/Cargo.toml`**

```toml
[package]
name        = "paigasus-helikon-providers-openai"
description = "OpenAI provider for the Paigasus Helikon AI SDK."
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

- [ ] **Step 2: Create `crates/paigasus-helikon-providers-openai/src/lib.rs`**

```rust
//! `paigasus-helikon-providers-openai` — stub. See SMA-304.
```

- [ ] **Step 3: Create `crates/paigasus-helikon-providers-openai/README.md`**

```markdown
# paigasus-helikon-providers-openai

OpenAI provider for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 4: Create `crates/paigasus-helikon-providers-anthropic/Cargo.toml`**

```toml
[package]
name        = "paigasus-helikon-providers-anthropic"
description = "Anthropic provider for the Paigasus Helikon AI SDK."
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

- [ ] **Step 5: Create `crates/paigasus-helikon-providers-anthropic/src/lib.rs`**

```rust
//! `paigasus-helikon-providers-anthropic` — stub. See SMA-304.
```

- [ ] **Step 6: Create `crates/paigasus-helikon-providers-anthropic/README.md`**

```markdown
# paigasus-helikon-providers-anthropic

Anthropic provider for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 7: Verify**

Run from repo root: `cargo build --workspace`

Expected: both crates compile, then `Finished`.

- [ ] **Step 8: Stage and commit**

```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon add crates/paigasus-helikon-providers-openai crates/paigasus-helikon-providers-anthropic Cargo.lock
git -C /Users/smaschek/dev/paigasus/paigasus-helikon commit -m "feat(providers): SMA-304 add openai and anthropic provider stubs"
```

---

### Task 7: `paigasus-helikon-mcp` and `paigasus-helikon-tools` stubs

**Files:**
- Create: `crates/paigasus-helikon-mcp/Cargo.toml`
- Create: `crates/paigasus-helikon-mcp/src/lib.rs`
- Create: `crates/paigasus-helikon-mcp/README.md`
- Create: `crates/paigasus-helikon-tools/Cargo.toml`
- Create: `crates/paigasus-helikon-tools/src/lib.rs`
- Create: `crates/paigasus-helikon-tools/README.md`

- [ ] **Step 1: Create `crates/paigasus-helikon-mcp/Cargo.toml`**

```toml
[package]
name        = "paigasus-helikon-mcp"
description = "MCP client and server integration for the Paigasus Helikon AI SDK."
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

- [ ] **Step 2: Create `crates/paigasus-helikon-mcp/src/lib.rs`**

```rust
//! `paigasus-helikon-mcp` — stub. See SMA-304.
```

- [ ] **Step 3: Create `crates/paigasus-helikon-mcp/README.md`**

```markdown
# paigasus-helikon-mcp

MCP client and server integration for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 4: Create `crates/paigasus-helikon-tools/Cargo.toml`**

```toml
[package]
name        = "paigasus-helikon-tools"
description = "Sandboxed Read/Write/Bash/WebFetch tools for the Paigasus Helikon AI SDK."
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

- [ ] **Step 5: Create `crates/paigasus-helikon-tools/src/lib.rs`**

```rust
//! `paigasus-helikon-tools` — stub. See SMA-304.
```

- [ ] **Step 6: Create `crates/paigasus-helikon-tools/README.md`**

```markdown
# paigasus-helikon-tools

Sandboxed Read/Write/Bash/WebFetch tools for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 7: Verify**

Run from repo root: `cargo build --workspace`

Expected: both crates compile, then `Finished`.

- [ ] **Step 8: Stage and commit**

```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon add crates/paigasus-helikon-mcp crates/paigasus-helikon-tools Cargo.lock
git -C /Users/smaschek/dev/paigasus/paigasus-helikon commit -m "feat(mcp,tools): SMA-304 add mcp and tools stub crates"
```

---

### Task 8: Runtime stubs (`tokio`, `axum`, `temporal`, `agentcore`)

**Files:** for each of `tokio`, `axum`, `temporal`, `agentcore`:
- Create: `crates/paigasus-helikon-runtime-<name>/Cargo.toml`
- Create: `crates/paigasus-helikon-runtime-<name>/src/lib.rs`
- Create: `crates/paigasus-helikon-runtime-<name>/README.md`

Repeat the pattern below four times, substituting `<name>` and the description.

- [ ] **Step 1: Create `crates/paigasus-helikon-runtime-tokio/Cargo.toml`**

```toml
[package]
name        = "paigasus-helikon-runtime-tokio"
description = "Default ephemeral Tokio runner for the Paigasus Helikon AI SDK."
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

- [ ] **Step 2: Create `crates/paigasus-helikon-runtime-tokio/src/lib.rs`**

```rust
//! `paigasus-helikon-runtime-tokio` — stub. See SMA-304.
```

- [ ] **Step 3: Create `crates/paigasus-helikon-runtime-tokio/README.md`**

```markdown
# paigasus-helikon-runtime-tokio

Default ephemeral Tokio runner for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 4: Create `crates/paigasus-helikon-runtime-axum/Cargo.toml`**

```toml
[package]
name        = "paigasus-helikon-runtime-axum"
description = "Self-hosted Axum runtime for the Paigasus Helikon AI SDK."
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

- [ ] **Step 5: Create `crates/paigasus-helikon-runtime-axum/src/lib.rs`**

```rust
//! `paigasus-helikon-runtime-axum` — stub. See SMA-304.
```

- [ ] **Step 6: Create `crates/paigasus-helikon-runtime-axum/README.md`**

```markdown
# paigasus-helikon-runtime-axum

Self-hosted Axum runtime for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 7: Create `crates/paigasus-helikon-runtime-temporal/Cargo.toml`**

```toml
[package]
name        = "paigasus-helikon-runtime-temporal"
description = "Temporal-backed durable runtime for the Paigasus Helikon AI SDK."
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

- [ ] **Step 8: Create `crates/paigasus-helikon-runtime-temporal/src/lib.rs`**

```rust
//! `paigasus-helikon-runtime-temporal` — stub. See SMA-304.
```

- [ ] **Step 9: Create `crates/paigasus-helikon-runtime-temporal/README.md`**

```markdown
# paigasus-helikon-runtime-temporal

Temporal-backed durable runtime for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 10: Create `crates/paigasus-helikon-runtime-agentcore/Cargo.toml`**

```toml
[package]
name        = "paigasus-helikon-runtime-agentcore"
description = "AWS Bedrock AgentCore runtime for the Paigasus Helikon AI SDK."
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

- [ ] **Step 11: Create `crates/paigasus-helikon-runtime-agentcore/src/lib.rs`**

```rust
//! `paigasus-helikon-runtime-agentcore` — stub. See SMA-304.
```

- [ ] **Step 12: Create `crates/paigasus-helikon-runtime-agentcore/README.md`**

```markdown
# paigasus-helikon-runtime-agentcore

AWS Bedrock AgentCore runtime for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 13: Verify**

Run from repo root: `cargo build --workspace`

Expected: all four runtime crates compile, then `Finished`.

- [ ] **Step 14: Stage and commit**

```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon add crates/paigasus-helikon-runtime-tokio crates/paigasus-helikon-runtime-axum crates/paigasus-helikon-runtime-temporal crates/paigasus-helikon-runtime-agentcore Cargo.lock
git -C /Users/smaschek/dev/paigasus/paigasus-helikon commit -m "feat(runtime): SMA-304 add tokio, axum, temporal, agentcore runtime stubs"
```

---

### Task 9: `paigasus-helikon-evals` stub

**Files:**
- Create: `crates/paigasus-helikon-evals/Cargo.toml`
- Create: `crates/paigasus-helikon-evals/src/lib.rs`
- Create: `crates/paigasus-helikon-evals/README.md`

- [ ] **Step 1: Create `crates/paigasus-helikon-evals/Cargo.toml`**

```toml
[package]
name        = "paigasus-helikon-evals"
description = "Evaluation harness for the Paigasus Helikon AI SDK."
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

- [ ] **Step 2: Create `crates/paigasus-helikon-evals/src/lib.rs`**

```rust
//! `paigasus-helikon-evals` — stub. See SMA-304.
```

- [ ] **Step 3: Create `crates/paigasus-helikon-evals/README.md`**

```markdown
# paigasus-helikon-evals

Evaluation harness for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 4: Verify**

Run from repo root: `cargo build --workspace`

Expected: `Compiling paigasus-helikon-evals v0.0.0` then `Finished`.

- [ ] **Step 5: Stage and commit**

```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon add crates/paigasus-helikon-evals Cargo.lock
git -C /Users/smaschek/dev/paigasus/paigasus-helikon commit -m "feat(evals): SMA-304 add paigasus-helikon-evals stub crate"
```

---

### Task 10: `paigasus-helikon-cli` with two binaries

**Files:**
- Create: `crates/paigasus-helikon-cli/Cargo.toml`
- Create: `crates/paigasus-helikon-cli/src/bin/helikon.rs`
- Create: `crates/paigasus-helikon-cli/src/bin/paigasus_helikon.rs`
- Create: `crates/paigasus-helikon-cli/README.md`

The `autobins = false` setting is required: the binary name `paigasus-helikon` (hyphen) doesn't match the filename `paigasus_helikon.rs` (underscore — hyphens are illegal in Rust filenames). Without it, Cargo's auto-discovery would create a phantom `paigasus_helikon` binary that conflicts with our explicit `[[bin]]` entry.

- [ ] **Step 1: Create `crates/paigasus-helikon-cli/Cargo.toml`**

```toml
[package]
name     = "paigasus-helikon-cli"
description = "CLI binaries (helikon, paigasus-helikon) for the Paigasus Helikon AI SDK."
autobins = false
version.workspace      = true
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[[bin]]
name = "helikon"
path = "src/bin/helikon.rs"

[[bin]]
name = "paigasus-helikon"
path = "src/bin/paigasus_helikon.rs"
```

- [ ] **Step 2: Create `crates/paigasus-helikon-cli/src/bin/helikon.rs`**

```rust
fn main() {
    println!("paigasus-helikon-cli stub (SMA-304)");
}
```

- [ ] **Step 3: Create `crates/paigasus-helikon-cli/src/bin/paigasus_helikon.rs`**

```rust
fn main() {
    println!("paigasus-helikon-cli stub (SMA-304)");
}
```

- [ ] **Step 4: Create `crates/paigasus-helikon-cli/README.md`**

```markdown
# paigasus-helikon-cli

CLI binaries (`helikon`, `paigasus-helikon`) for the Paigasus Helikon AI SDK. Stub — see SMA-304.
```

- [ ] **Step 5: Verify build**

Run from repo root: `cargo build --workspace`

Expected: `Compiling paigasus-helikon-cli v0.0.0` (with two bin units) then `Finished`.

- [ ] **Step 6: Verify both binaries run**

Run from repo root:
```bash
cargo run -p paigasus-helikon-cli --bin helikon
cargo run -p paigasus-helikon-cli --bin paigasus-helikon
```

Expected output for each: `paigasus-helikon-cli stub (SMA-304)`.

- [ ] **Step 7: Stage and commit**

```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon add crates/paigasus-helikon-cli Cargo.lock
git -C /Users/smaschek/dev/paigasus/paigasus-helikon commit -m "feat(cli): SMA-304 add cli crate with helikon and paigasus-helikon binaries"
```

---

### Task 11: `paigasus-helikon` facade + internal deps in workspace

**Files:**
- Modify: `Cargo.toml` (workspace root) — add internal deps to `[workspace.dependencies]`
- Create: `crates/paigasus-helikon/Cargo.toml`
- Create: `crates/paigasus-helikon/src/lib.rs`
- Create: `crates/paigasus-helikon/README.md`

This is the most involved task: the facade declares feature-gated optional deps on every other crate, plus `cfg`-gated `pub use` re-exports. Because internal `[workspace.dependencies]` paths are now resolvable (all crates exist), we add them in this task.

- [ ] **Step 1: Edit workspace `Cargo.toml` — add internal deps to `[workspace.dependencies]`**

Append the following lines under the existing `[workspace.dependencies]` block (after `async-trait`):

```toml

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

- [ ] **Step 2: Create `crates/paigasus-helikon/Cargo.toml`**

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

- [ ] **Step 3: Create `crates/paigasus-helikon/README.md`**

```markdown
# paigasus-helikon

Facade crate for the Paigasus Helikon AI SDK. Re-exports `paigasus-helikon-core` plus feature-gated providers, runtimes, and extensions.

## Usage

```toml
[dependencies]
paigasus-helikon = { version = "0.1", features = ["openai", "anthropic", "mcp", "runtime-tokio"] }
```

See [SMA-304](https://linear.app/smaschek/issue/SMA-304) for the bootstrap status.
```

- [ ] **Step 4: Create `crates/paigasus-helikon/src/lib.rs`**

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

- [ ] **Step 5: Verify build with no features**

Run from repo root: `cargo build --workspace`

Expected: `Compiling paigasus-helikon v0.0.0` then `Finished`. With `default = []`, only `paigasus-helikon-core` is pulled in by the facade.

- [ ] **Step 6: Verify build with all features**

Run from repo root: `cargo build --workspace --all-features`

Expected: every optional dep is pulled in and compiles, then `Finished`.

- [ ] **Step 7: Verify `cargo doc --workspace --all-features --no-deps` succeeds**

Run from repo root:
```bash
cargo doc --workspace --all-features --no-deps
```

Expected: `Finished` with no errors. (Warnings about empty doc-comments are acceptable for stubs.)

- [ ] **Step 8: Stage and commit**

```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon add Cargo.toml crates/paigasus-helikon Cargo.lock
git -C /Users/smaschek/dev/paigasus/paigasus-helikon commit -m "feat(facade): SMA-304 add paigasus-helikon facade with feature-gated re-exports"
```

---

### Task 12: CI workflow

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Create `.github/workflows/ci.yml`**

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

- [ ] **Step 2: Verify the workflow file is valid YAML**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('/Users/smaschek/dev/paigasus/paigasus-helikon/.github/workflows/ci.yml'))" && echo OK`

Expected: `OK`. (No GitHub-side validation is possible without pushing.)

- [ ] **Step 3: Stage and commit**

```bash
git -C /Users/smaschek/dev/paigasus/paigasus-helikon add .github/workflows/ci.yml
git -C /Users/smaschek/dev/paigasus/paigasus-helikon commit -m "ci: SMA-304 add minimal build + doc workflow"
```

---

### Task 13: Final verification and handoff

**Files:** none (verification + reporting only).

- [ ] **Step 1: Run all four Definition-of-Done commands**

Run from repo root, in this order:

```bash
cargo build --workspace
cargo doc --workspace --all-features --no-deps
cargo run -p paigasus-helikon-cli --bin helikon
cargo run -p paigasus-helikon-cli --bin paigasus-helikon
```

Expected: all four succeed. The two `cargo run` commands print `paigasus-helikon-cli stub (SMA-304)`.

- [ ] **Step 2: Confirm git state is clean**

Run: `git -C /Users/smaschek/dev/paigasus/paigasus-helikon status --short --branch`

Expected: `## feature/sma-304-bootstrap-github-repo-and-cargo-workspace-skeleton` with no `M` or `??` lines (other than possibly `?? CLAUDE.md` left over from before this branch).

- [ ] **Step 3: Show the commit log for the branch**

Run: `git -C /Users/smaschek/dev/paigasus/paigasus-helikon log main..HEAD --oneline`

Expected: roughly 11 commits, one per implementation task (Tasks 2 through 12).

- [ ] **Step 4: Report ready-to-push status to the user**

Print a summary message that includes:
- The exact branch name (`feature/sma-304-bootstrap-github-repo-and-cargo-workspace-skeleton`).
- The commit count and a one-line `git log --oneline` listing.
- The output of all four Definition-of-Done commands (succeeded).
- A reminder that pushing and PR creation are user-driven; suggest the commands but do **not** execute them:
  ```bash
  git push -u origin feature/sma-304-bootstrap-github-repo-and-cargo-workspace-skeleton
  gh pr create --fill
  ```
- A note that **after the PR is merged**, SMA-304's status should be moved to "Done" in Linear (the executing agent should not auto-close it).

No commit in this task.

---

## Risks recap (from spec, for the executing agent)

1. **MIT-only**, not `Apache-2.0 OR MIT`. SMA-304 has been updated to reflect this. Don't add `LICENSE-APACHE`.
2. **MSRV 1.75 may not survive `opentelemetry 0.27` or `rmcp 0.16+`.** If `cargo build` fails citing rust-version, bump `rust-version` in `[workspace.package]` to whatever cargo demands and retry. Surface the new value in the final report.
3. **No remote pushes from the agent.** Task 13 reports ready-to-push but stops short of `git push` or PR creation.
4. **Acceptance criterion #3** (Notion/Linear "Resources" section) is a manual housekeeping step, not part of this plan.
