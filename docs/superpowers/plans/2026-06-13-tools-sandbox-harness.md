# Tools Sandbox Harness (SMA-328) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the filesystem + process subset of `paigasus-helikon-tools` — a shared `cap-std`-backed `Sandbox` plus `ReadTool`, `WriteTool`, `EditTool`, and `BashTool` — and ascend the crate from its `0.0.0` stub to `0.1.0`.

**Architecture:** A `Sandbox` opens a directory as a `cap_std::fs::Dir` capability handle that the OS confines (rejects `..`, absolute paths, symlink escapes). Each tool is a small struct generic over the agent context `Ctx` via `PhantomData` (the `McpTool` pattern), implementing core's `Tool<Ctx>` trait. Synchronous `cap-std` calls run inside `tokio::task::spawn_blocking`. Boundary violations surface as a new `ToolError::Denied { reason }` in core; `BashTool` is soft-confined (cwd-pinned) and documented as **not** a security sandbox.

**Tech Stack:** Rust (edition 2021, MSRV 1.85), `cap-std`, `tokio`, `async-trait`, `serde` + `schemars`, `thiserror`/`anyhow`, `tempfile` (dev). Integrates with `paigasus-helikon-core` `0.5.0`.

**Spec:** `docs/superpowers/specs/2026-06-13-tools-sandbox-harness-design.md`

---

## File Structure

- **Modify** `crates/paigasus-helikon-core/src/tool.rs` — add `ToolError::Denied { reason }` variant.
- **Modify** `Cargo.toml` (root) — add `cap-std`, `tempfile`, `futures-util` to `[workspace.dependencies]` (if absent).
- **Modify** `crates/paigasus-helikon-tools/Cargo.toml` — deps + ascend `0.0.0`→`0.1.0`, remove `publish = false`.
- **Create** `crates/paigasus-helikon-tools/src/lib.rs` — crate docs (incl. loud Bash warning) + module decls + re-exports.
- **Create** `crates/paigasus-helikon-tools/src/sandbox.rs` — `Sandbox`, `SandboxError`, path-component guard.
- **Create** `crates/paigasus-helikon-tools/src/read.rs` — `ReadTool`.
- **Create** `crates/paigasus-helikon-tools/src/write.rs` — `WriteTool`.
- **Create** `crates/paigasus-helikon-tools/src/edit.rs` — `EditTool`.
- **Create** `crates/paigasus-helikon-tools/src/bash.rs` — `BashTool`, `BashToolBuilder`.
- **Create** `crates/paigasus-helikon-tools/tests/sandbox.rs` — containment + tool unit tests.
- **Create** `crates/paigasus-helikon-tools/tests/bash.rs` — Bash behavior tests.
- **Create** `crates/paigasus-helikon-tools/tests/common/mod.rs` — `ScriptedModel` + test helpers.
- **Create** `crates/paigasus-helikon-tools/tests/sandbox_navigation.rs` — `#[cfg(unix)]` AC test.
- **Create** `crates/paigasus-helikon-tools/examples/explore_sandbox.rs` — real-model demo with a `PermissionPolicy`.
- **Modify** `release-plz.toml`, facade `Cargo.toml` + `CHANGELOG`, core `CHANGELOG` + workspace pin — the ascend.

> **Conventions to honor throughout:** run `cargo fmt --all` + `cargo clippy --workspace --all-features --all-targets -- -D warnings` before every commit (the pre-commit hook is a no-op; pre-push enforces these). Every `pub` item needs a `///` doc comment (`missing_docs = "warn"` + `-D warnings`). Commits are signed via a 1Password SSH key — if a commit fails with "failed to fill whole buffer", ask the user to unlock their vault; do not bypass signing. Stage explicit paths — never `git add -A` (`.env`/`.claude` are not gitignored).

---

## Task 1: Add `ToolError::Denied` to core

**Files:**
- Modify: `crates/paigasus-helikon-core/src/tool.rs` (the `ToolError` enum, ~line 253)
- Test: `crates/paigasus-helikon-core/src/tool.rs` (inline `#[cfg(test)]` module, or add one)

- [ ] **Step 1: Write the failing test**

Add (or extend) a test module at the bottom of `crates/paigasus-helikon-core/src/tool.rs`:

```rust
#[cfg(test)]
mod denied_variant_tests {
    use super::ToolError;

    #[test]
    fn denied_displays_reason() {
        let e = ToolError::Denied {
            reason: "path escapes the sandbox root".to_owned(),
        };
        assert_eq!(e.to_string(), "operation denied: path escapes the sandbox root");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-core denied_displays_reason`
Expected: FAIL to compile — `no variant named `Denied` found for enum `ToolError``.

- [ ] **Step 3: Add the variant**

In the `ToolError` enum in `crates/paigasus-helikon-core/src/tool.rs`, add this variant (keep `Other` last):

```rust
    /// The tool refused the operation: either a hard safety-boundary violation
    /// (a path outside the sandbox root, a non-UTF-8 read) or an unsatisfiable
    /// precondition (an ambiguous edit target, an allow/deny-blocked command).
    /// Distinct from a [`crate::PermissionPolicy`] denial, which the runner
    /// resolves before `invoke` is ever called. Not recoverable.
    #[error("operation denied: {reason}")]
    Denied {
        /// Human-readable denial reason, surfaced to the model.
        reason: String,
    },
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p paigasus-helikon-core denied_displays_reason`
Expected: PASS.

- [ ] **Step 5: Confirm the workspace still builds and the runner needs no change**

Run: `cargo build -p paigasus-helikon-core`
Expected: builds clean. (The runner at `agent.rs:590-595` maps every `ToolError` via `e.to_string()` with no per-variant match, so the new variant compiles and surfaces to the model exactly like `Other`.)

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/tool.rs
git commit -m "feat(core): SMA-328 add ToolError::Denied variant"
```

---

## Task 2: Scaffold the tools crate (deps + empty lib that builds)

**Files:**
- Modify: `Cargo.toml` (root) — `[workspace.dependencies]`
- Modify: `crates/paigasus-helikon-tools/Cargo.toml`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs`

- [ ] **Step 1: Ensure workspace dependency pins exist**

Open the root `Cargo.toml` `[workspace.dependencies]` table. Confirm each of these is present; **add any that are missing** (most already exist for core):

```toml
cap-std = "4"
tempfile = "3"
futures-util = "0.3"
```

(`async-trait`, `serde`, `serde_json`, `schemars`, `tokio`, `thiserror`, `anyhow` already exist — do not duplicate. If `tempfile`/`futures-util` already exist, leave them.)

- [ ] **Step 2: Write the tools crate Cargo.toml**

Replace `crates/paigasus-helikon-tools/Cargo.toml` with:

```toml
[package]
name        = "paigasus-helikon-tools"
description = "Sandboxed Read/Write/Edit/Bash tools for the Paigasus Helikon AI SDK."
version                = "0.1.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[dependencies]
paigasus-helikon-core = { workspace = true }
async-trait           = { workspace = true }
serde                 = { workspace = true, features = ["derive"] }
serde_json            = { workspace = true }
schemars              = { workspace = true }
thiserror             = { workspace = true }
anyhow                = { workspace = true }
cap-std               = { workspace = true }
tokio                 = { workspace = true, features = ["process", "time", "rt", "macros"] }

[dev-dependencies]
tokio        = { workspace = true, features = ["rt-multi-thread", "macros", "time"] }
tempfile     = { workspace = true }
futures-util = { workspace = true }
# Path-only (version-less) internal dev-dep for the example — stripped from the
# published manifest; examples are not built during `cargo publish --verify`.
paigasus-helikon-providers-openai = { path = "../paigasus-helikon-providers-openai" }

[lints]
workspace = true
```

> If `cargo build` later complains that a workspace `tokio` feature is unified-off, add the missing feature to the root `tokio` pin's `features` instead of pinning a version here.

- [ ] **Step 3: Write a minimal lib.rs (crate docs only, no modules yet)**

Replace `crates/paigasus-helikon-tools/src/lib.rs` with:

```rust
//! Sandboxed filesystem and process tools for the Paigasus Helikon AI SDK.
//!
//! This crate provides agent [`Tool`](paigasus_helikon_core::Tool)s that
//! operate inside a [`Sandbox`] — a directory opened as an OS-confined
//! capability (`cap-std`), so [`ReadTool`], [`WriteTool`], and [`EditTool`]
//! cannot escape it via `..`, absolute paths, or symlinks.
//!
//! # Security note on [`BashTool`]
//!
//! [`BashTool`] is a **cwd-pinned shell, NOT a security sandbox.** The
//! `cap-std` containment that jails the filesystem tools does **not** extend to
//! a spawned child process: a command can read and write anything this process
//! can — absolute paths, `..`, `~`, and the network. In
//! [`PermissionMode::Default`](paigasus_helikon_core::PermissionMode) with no
//! [`PermissionPolicy`](paigasus_helikon_core::PermissionPolicy) installed, the
//! control layer is permissive, so `BashTool` runs **ungated**. Pair it with a
//! `PermissionPolicy` or `DenyRule::tool("Bash")` for real control.
```

(The `[`ReadTool`]` etc. intra-doc links resolve once the modules exist in later tasks; rustdoc only fails on these under `-D warnings` at the `docs` job, which runs in Task 10 after everything is wired.)

- [ ] **Step 4: Verify the crate builds**

Run: `cargo build -p paigasus-helikon-tools`
Expected: builds clean (empty lib).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add Cargo.toml crates/paigasus-helikon-tools/Cargo.toml crates/paigasus-helikon-tools/src/lib.rs
git commit -m "chore(tools): SMA-328 scaffold crate deps and lib docs"
```

---

## Task 3: The `Sandbox` primitive

**Files:**
- Create: `crates/paigasus-helikon-tools/src/sandbox.rs`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs` (declare module + re-export)
- Test: `crates/paigasus-helikon-tools/tests/sandbox.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/paigasus-helikon-tools/tests/sandbox.rs`:

```rust
use paigasus_helikon_tools::{Sandbox, SandboxError};

#[test]
fn open_succeeds_on_existing_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::open(tmp.path()).expect("open sandbox");
    assert_eq!(sandbox.root(), tmp.path().canonicalize().unwrap());
}

#[test]
fn open_fails_on_missing_dir() {
    let err = Sandbox::open("/no/such/dir/anywhere-xyz").unwrap_err();
    assert!(matches!(err, SandboxError::Open { .. }));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-tools --test sandbox open_succeeds_on_existing_dir`
Expected: FAIL to compile — unresolved import `paigasus_helikon_tools::Sandbox`.

- [ ] **Step 3: Implement `sandbox.rs`**

Create `crates/paigasus-helikon-tools/src/sandbox.rs`:

```rust
//! The capability-confined [`Sandbox`] shared by the filesystem tools.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cap_std::ambient_authority;
use cap_std::fs::Dir;

/// A directory opened as an OS-confined capability. Filesystem operations
/// performed through this handle are resolved relative to the root and cannot
/// escape it (`..`, absolute paths, and escaping symlinks are rejected).
///
/// Cheap to clone (it is `Arc`-backed); share one `Sandbox` across many tools.
#[derive(Clone)]
pub struct Sandbox {
    inner: Arc<SandboxInner>,
}

struct SandboxInner {
    root: PathBuf,
    dir: Dir,
}

impl Sandbox {
    /// Open `root` as a capability-confined sandbox.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, SandboxError> {
        let root = root.as_ref();
        let dir = Dir::open_ambient_dir(root, ambient_authority()).map_err(|source| {
            SandboxError::Open {
                path: root.to_path_buf(),
                source,
            }
        })?;
        let canonical = root.canonicalize().map_err(|source| SandboxError::Open {
            path: root.to_path_buf(),
            source,
        })?;
        Ok(Self {
            inner: Arc::new(SandboxInner {
                root: canonical,
                dir,
            }),
        })
    }

    /// The canonical sandbox root on the host filesystem (diagnostics / cwd).
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// The underlying capability directory handle.
    pub(crate) fn dir(&self) -> &Dir {
        &self.inner.dir
    }
}

/// Reject a tool-supplied path that is absolute or contains a `..`/root/prefix
/// component before it reaches the capability layer. The `cap-std` `Dir` is the
/// backstop for symlink escapes; this is the deterministic front gate.
pub(crate) fn guard_relative(path: &str) -> Result<&Path, String> {
    let p = Path::new(path);
    if p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!("path escapes the sandbox root: {path}"));
    }
    Ok(p)
}

/// Errors from constructing a [`Sandbox`]. In-`invoke` boundary violations use
/// [`paigasus_helikon_core::ToolError::Denied`], not this type.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SandboxError {
    /// The sandbox root could not be opened (missing, not a directory, perms).
    #[error("cannot open sandbox root {path}: {source}")]
    Open {
        /// The path that failed to open.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}
```

- [ ] **Step 4: Wire the module into lib.rs**

In `crates/paigasus-helikon-tools/src/lib.rs`, append after the doc block:

```rust

mod sandbox;

pub use sandbox::{Sandbox, SandboxError};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools --test sandbox`
Expected: both tests PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/sandbox.rs crates/paigasus-helikon-tools/src/lib.rs crates/paigasus-helikon-tools/tests/sandbox.rs
git commit -m "feat(tools): SMA-328 add cap-std Sandbox primitive"
```

---

## Task 4: `ReadTool`

**Files:**
- Create: `crates/paigasus-helikon-tools/src/read.rs`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs`
- Test: `crates/paigasus-helikon-tools/tests/sandbox.rs` (extend)

- [ ] **Step 1: Write the failing tests**

Append to `crates/paigasus-helikon-tools/tests/sandbox.rs`:

```rust
use paigasus_helikon_core::{
    CancellationToken, HookRegistry, MemorySession, RunContext, Tool, ToolContext, ToolEffect,
    ToolError, TracerHandle,
};
use std::sync::Arc;

/// Build a `ToolContext<()>` for calling a tool's `invoke` directly.
fn tool_ctx() -> ToolContext<()> {
    let run_ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );
    run_ctx.to_tool_context()
}

#[tokio::test]
async fn read_returns_file_content() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "hello sandbox").unwrap();
    let sandbox = Sandbox::open(tmp.path()).unwrap();
    let tool: ReadTool = ReadTool::new(sandbox);

    assert_eq!(tool.name(), "Read");
    assert_eq!(tool.effect(), ToolEffect::ReadOnly);

    let out = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "notes.txt" }))
        .await
        .unwrap();
    assert_eq!(out.content["content"], "hello sandbox");
}

#[tokio::test]
async fn read_rejects_parent_escape() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());

    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "../secret" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn read_rejects_absolute_path() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());

    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "/etc/passwd" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn read_non_utf8_is_denied() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("bad.bin"), [0xff, 0xfe, 0x00]).unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());

    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "bad.bin" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn read_missing_file_is_other() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());

    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "nope.txt" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Other(_)));
}

#[cfg(unix)]
#[tokio::test]
async fn read_rejects_escaping_symlink() {
    use paigasus_helikon_tools::ReadTool;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        tmp.path().join("link.txt"),
    )
    .unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());

    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "link.txt" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-tools --test sandbox read_`
Expected: FAIL to compile — `ReadTool` unresolved.

- [ ] **Step 3: Implement `read.rs`**

Create `crates/paigasus-helikon-tools/src/read.rs`:

```rust
//! [`ReadTool`] — read a UTF-8 text file from the sandbox.

use std::io;
use std::marker::PhantomData;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

use crate::sandbox::{guard_relative, Sandbox};

/// Arguments for [`ReadTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadArgs {
    /// Path to read, relative to the sandbox root.
    path: String,
    /// 1-based first line to return (inclusive). Omit to start at the top.
    #[serde(default)]
    offset: Option<u64>,
    /// Maximum number of lines to return. Omit for the whole file.
    #[serde(default)]
    limit: Option<u64>,
}

/// Read a UTF-8 text file relative to the sandbox root, optionally windowed by
/// line. Read-only; allowed under `Plan` mode.
pub struct ReadTool<Ctx = ()> {
    sandbox: Sandbox,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> ReadTool<Ctx> {
    /// Construct a `ReadTool` over `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            schema: serde_json::to_value(schemars::schema_for!(ReadArgs))
                .expect("ReadArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for ReadTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file relative to the sandbox root. Optional `offset` \
         and `limit` select a 1-based line window."
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn invoke(&self, _ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: ReadArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
            schema_errors: vec![e.to_string()],
        })?;
        let rel = guard_relative(&args.path)
            .map_err(|reason| ToolError::Denied { reason })?
            .to_path_buf();

        let sandbox = self.sandbox.clone();
        let path_for_msg = args.path.clone();
        let text = tokio::task::spawn_blocking(move || sandbox.dir().read_to_string(rel))
            .await
            .map_err(|e| ToolError::Other(e.into()))?
            .map_err(|e| map_read_error(&path_for_msg, e))?;

        let content = window(&text, args.offset, args.limit);
        Ok(ToolOutput::new(serde_json::json!({ "content": content })))
    }
}

/// Map a `cap-std` read error to the right `ToolError` variant.
fn map_read_error(path: &str, e: io::Error) -> ToolError {
    match e.kind() {
        io::ErrorKind::NotFound => ToolError::Other(anyhow::anyhow!("no such file: {path}")),
        io::ErrorKind::InvalidData => ToolError::Denied {
            reason: format!("file is not valid UTF-8: {path}"),
        },
        // Anything else from a path op (incl. cap-std's symlink/escape
        // rejection) is treated as a denial of the operation.
        _ => ToolError::Denied {
            reason: format!("cannot read {path}: {e}"),
        },
    }
}

/// Apply the 1-based `offset`/`limit` line window.
fn window(text: &str, offset: Option<u64>, limit: Option<u64>) -> String {
    if offset.is_none() && limit.is_none() {
        return text.to_owned();
    }
    let start = offset.unwrap_or(1).saturating_sub(1) as usize;
    let lines: Vec<&str> = text.lines().collect();
    let slice: Vec<&str> = match limit {
        Some(n) => lines.into_iter().skip(start).take(n as usize).collect(),
        None => lines.into_iter().skip(start).collect(),
    };
    slice.join("\n")
}
```

- [ ] **Step 4: Wire into lib.rs**

In `crates/paigasus-helikon-tools/src/lib.rs`, add `mod read;` next to `mod sandbox;` and extend the re-export:

```rust
mod read;
mod sandbox;

pub use read::ReadTool;
pub use sandbox::{Sandbox, SandboxError};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools --test sandbox read_`
Expected: all `read_*` tests PASS (incl. the `#[cfg(unix)]` symlink test on macOS/Linux).

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/read.rs crates/paigasus-helikon-tools/src/lib.rs crates/paigasus-helikon-tools/tests/sandbox.rs
git commit -m "feat(tools): SMA-328 add ReadTool"
```

---

## Task 5: `WriteTool`

**Files:**
- Create: `crates/paigasus-helikon-tools/src/write.rs`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs`
- Test: `crates/paigasus-helikon-tools/tests/sandbox.rs` (extend)

- [ ] **Step 1: Write the failing tests**

Append to `crates/paigasus-helikon-tools/tests/sandbox.rs`:

```rust
#[tokio::test]
async fn write_creates_file_and_parents() {
    use paigasus_helikon_tools::WriteTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: WriteTool = WriteTool::new(Sandbox::open(tmp.path()).unwrap());
    assert_eq!(tool.effect(), ToolEffect::Write);

    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "sub/dir/out.txt", "content": "data" }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["bytes_written"], 4);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("sub/dir/out.txt")).unwrap(),
        "data"
    );
}

#[tokio::test]
async fn write_rejects_escape() {
    use paigasus_helikon_tools::WriteTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: WriteTool = WriteTool::new(Sandbox::open(tmp.path()).unwrap());

    let err = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "../evil.txt", "content": "x" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-tools --test sandbox write_`
Expected: FAIL to compile — `WriteTool` unresolved.

- [ ] **Step 3: Implement `write.rs`**

Create `crates/paigasus-helikon-tools/src/write.rs`:

```rust
//! [`WriteTool`] — create or overwrite a file inside the sandbox.

use std::marker::PhantomData;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

use crate::sandbox::{guard_relative, Sandbox};

/// Arguments for [`WriteTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WriteArgs {
    /// Path to write, relative to the sandbox root.
    path: String,
    /// Full file contents (overwrites any existing file).
    content: String,
}

/// Create or overwrite a file relative to the sandbox root, creating parent
/// directories inside the sandbox as needed.
pub struct WriteTool<Ctx = ()> {
    sandbox: Sandbox,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> WriteTool<Ctx> {
    /// Construct a `WriteTool` over `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            schema: serde_json::to_value(schemars::schema_for!(WriteArgs))
                .expect("WriteArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for WriteTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        "Create or overwrite a file relative to the sandbox root. Parent \
         directories inside the sandbox are created as needed."
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Write
    }

    async fn invoke(&self, _ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: WriteArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
            schema_errors: vec![e.to_string()],
        })?;
        let rel = guard_relative(&args.path)
            .map_err(|reason| ToolError::Denied { reason })?
            .to_path_buf();

        let sandbox = self.sandbox.clone();
        let content = args.content;
        let bytes = content.len();
        let rel_for_msg = args.path.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let dir = sandbox.dir();
            if let Some(parent) = rel.parent() {
                if !parent.as_os_str().is_empty() {
                    dir.create_dir_all(parent)?;
                }
            }
            dir.write(&rel, content.as_bytes())
        })
        .await
        .map_err(|e| ToolError::Other(e.into()))?
        .map_err(|e| ToolError::Denied {
            reason: format!("cannot write {rel_for_msg}: {e}"),
        })?;

        Ok(ToolOutput::new(
            serde_json::json!({ "path": args.path, "bytes_written": bytes }),
        ))
    }
}
```

- [ ] **Step 4: Wire into lib.rs**

Add `mod write;` and `pub use write::WriteTool;` to `crates/paigasus-helikon-tools/src/lib.rs`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools --test sandbox write_`
Expected: both `write_*` tests PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/write.rs crates/paigasus-helikon-tools/src/lib.rs crates/paigasus-helikon-tools/tests/sandbox.rs
git commit -m "feat(tools): SMA-328 add WriteTool"
```

---

## Task 6: `EditTool`

**Files:**
- Create: `crates/paigasus-helikon-tools/src/edit.rs`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs`
- Test: `crates/paigasus-helikon-tools/tests/sandbox.rs` (extend)

- [ ] **Step 1: Write the failing tests**

Append to `crates/paigasus-helikon-tools/tests/sandbox.rs`:

```rust
#[tokio::test]
async fn edit_replaces_unique_string() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "alpha beta gamma").unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());

    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "f.txt", "old_string": "beta", "new_string": "BETA" }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["replacements"], 1);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "alpha BETA gamma"
    );
}

#[tokio::test]
async fn edit_not_found_is_denied() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "alpha").unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());

    let err = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "f.txt", "old_string": "zzz", "new_string": "x" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn edit_non_unique_without_replace_all_is_denied() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "x x x").unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());

    let err = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "f.txt", "old_string": "x", "new_string": "y" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn edit_replace_all_replaces_every_occurrence() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "x x x").unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());

    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({
                "path": "f.txt", "old_string": "x", "new_string": "y", "replace_all": true
            }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["replacements"], 3);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "y y y"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-tools --test sandbox edit_`
Expected: FAIL to compile — `EditTool` unresolved.

- [ ] **Step 3: Implement `edit.rs`**

Create `crates/paigasus-helikon-tools/src/edit.rs`:

```rust
//! [`EditTool`] — exact string replacement inside a sandbox file.

use std::marker::PhantomData;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

use crate::sandbox::{guard_relative, Sandbox};

/// Arguments for [`EditTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EditArgs {
    /// Path to edit, relative to the sandbox root.
    path: String,
    /// The exact text to replace. Must occur in the file.
    old_string: String,
    /// The replacement text.
    new_string: String,
    /// Replace every occurrence. When false (default), `old_string` must be
    /// unique or the edit is refused.
    #[serde(default)]
    replace_all: bool,
}

/// Replace an exact string in a sandbox file. Refuses (does not guess) when
/// `old_string` is missing or ambiguous.
pub struct EditTool<Ctx = ()> {
    sandbox: Sandbox,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> EditTool<Ctx> {
    /// Construct an `EditTool` over `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            schema: serde_json::to_value(schemars::schema_for!(EditArgs))
                .expect("EditArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for EditTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string in a sandbox file. `old_string` must occur in \
         the file, and must be unique unless `replace_all` is true."
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Write
    }

    async fn invoke(&self, _ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: EditArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
            schema_errors: vec![e.to_string()],
        })?;
        let rel = guard_relative(&args.path)
            .map_err(|reason| ToolError::Denied { reason })?
            .to_path_buf();

        let sandbox = self.sandbox.clone();
        let EditArgs {
            path,
            old_string,
            new_string,
            replace_all,
        } = args;

        let result = tokio::task::spawn_blocking(move || -> Result<(String, usize), ToolError> {
            let dir = sandbox.dir();
            let original = dir
                .read_to_string(&rel)
                .map_err(|e| ToolError::Denied {
                    reason: format!("cannot read {path} for edit: {e}"),
                })?;
            let count = original.matches(&old_string).count();
            if count == 0 {
                return Err(ToolError::Denied {
                    reason: format!("old_string not found in {path}"),
                });
            }
            if count > 1 && !replace_all {
                return Err(ToolError::Denied {
                    reason: "old_string is not unique; pass replace_all or add context".to_owned(),
                });
            }
            let updated = if replace_all {
                original.replace(&old_string, &new_string)
            } else {
                original.replacen(&old_string, &new_string, 1)
            };
            dir.write(&rel, updated.as_bytes())
                .map_err(|e| ToolError::Denied {
                    reason: format!("cannot write {path}: {e}"),
                })?;
            Ok((path, count))
        })
        .await
        .map_err(|e| ToolError::Other(e.into()))??;

        let (path, count) = result;
        Ok(ToolOutput::new(
            serde_json::json!({ "path": path, "replacements": count }),
        ))
    }
}
```

- [ ] **Step 4: Wire into lib.rs**

Add `mod edit;` and `pub use edit::EditTool;` to `crates/paigasus-helikon-tools/src/lib.rs`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools --test sandbox edit_`
Expected: all four `edit_*` tests PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/edit.rs crates/paigasus-helikon-tools/src/lib.rs crates/paigasus-helikon-tools/tests/sandbox.rs
git commit -m "feat(tools): SMA-328 add EditTool"
```

---

## Task 7: `BashTool` + `BashToolBuilder`

**Files:**
- Create: `crates/paigasus-helikon-tools/src/bash.rs`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs`
- Test: `crates/paigasus-helikon-tools/tests/bash.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/paigasus-helikon-tools/tests/bash.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;

use paigasus_helikon_core::{
    CancellationToken, HookRegistry, MemorySession, RunContext, Tool, ToolContext, ToolEffect,
    ToolError, TracerHandle,
};
use paigasus_helikon_tools::{BashTool, Sandbox};

fn tool_ctx() -> ToolContext<()> {
    let run_ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );
    run_ctx.to_tool_context()
}

#[cfg(unix)]
#[tokio::test]
async fn bash_runs_in_sandbox_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("marker.txt"), "x").unwrap();
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap()).build();
    assert_eq!(tool.name(), "Bash");
    assert_eq!(tool.effect(), ToolEffect::SideEffect);

    let out = tool
        .invoke(&tool_ctx(), serde_json::json!({ "command": "ls" }))
        .await
        .unwrap();
    assert!(out.content["stdout"].as_str().unwrap().contains("marker.txt"));
    assert_eq!(out.content["exit_code"], 0);
    assert_eq!(out.content["timed_out"], false);
}

#[cfg(unix)]
#[tokio::test]
async fn bash_times_out() {
    let tmp = tempfile::tempdir().unwrap();
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(Duration::from_millis(200))
        .build();

    let out = tool
        .invoke(&tool_ctx(), serde_json::json!({ "command": "sleep 5" }))
        .await
        .unwrap();
    assert_eq!(out.content["timed_out"], true);
}

#[cfg(unix)]
#[tokio::test]
async fn bash_truncates_output() {
    let tmp = tempfile::tempdir().unwrap();
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap())
        .max_output_bytes(16)
        .build();

    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "command": "printf 'abcdefghijklmnopqrstuvwxyz'" }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["truncated"], true);
    assert!(out.content["stdout"].as_str().unwrap().len() <= 16);
}

#[cfg(unix)]
#[tokio::test]
async fn bash_denies_blocked_command() {
    let tmp = tempfile::tempdir().unwrap();
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap())
        .deny_commands(["rm"])
        .build();

    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "command": "rm -rf /" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-tools --test bash`
Expected: FAIL to compile — `BashTool` unresolved.

- [ ] **Step 3: Implement `bash.rs`**

Create `crates/paigasus-helikon-tools/src/bash.rs`:

```rust
//! [`BashTool`] — a cwd-pinned shell. **NOT a security sandbox** (see the
//! crate-level docs): a spawned command can read/write anything this process
//! can. Gate it with a `PermissionPolicy` or `DenyRule::tool("Bash")`.

use std::marker::PhantomData;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::sandbox::Sandbox;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_OUTPUT: usize = 1 << 20; // 1 MiB

/// Arguments for [`BashTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BashArgs {
    /// The shell command to run (`sh -c <command>` on unix, `cmd /C` on Windows).
    command: String,
}

/// Builder for [`BashTool`].
pub struct BashToolBuilder {
    sandbox: Sandbox,
    timeout: Duration,
    env_allowlist: Vec<String>,
    max_output_bytes: usize,
    deny_commands: Vec<String>,
    allow_commands: Option<Vec<String>>,
}

impl BashToolBuilder {
    /// Maximum wall-clock duration before the command's process group is killed.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Environment variable names to pass through (the rest are dropped).
    pub fn env_allowlist<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.env_allowlist = names.into_iter().map(Into::into).collect();
        self
    }

    /// Truncate captured stdout/stderr to this many bytes each.
    pub fn max_output_bytes(mut self, n: usize) -> Self {
        self.max_output_bytes = n;
        self
    }

    /// Refuse any command whose first whitespace-delimited token matches one of
    /// these names.
    pub fn deny_commands<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.deny_commands = names.into_iter().map(Into::into).collect();
        self
    }

    /// If set, allow ONLY commands whose first token is in this list.
    pub fn allow_commands<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow_commands = Some(names.into_iter().map(Into::into).collect());
        self
    }

    /// Finish building.
    pub fn build<Ctx>(self) -> BashTool<Ctx> {
        BashTool {
            sandbox: self.sandbox,
            timeout: self.timeout,
            env_allowlist: self.env_allowlist,
            max_output_bytes: self.max_output_bytes,
            deny_commands: self.deny_commands,
            allow_commands: self.allow_commands,
            schema: serde_json::to_value(schemars::schema_for!(BashArgs))
                .expect("BashArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

/// A cwd-pinned shell tool. See the crate-level security note: this is **not**
/// a security boundary.
pub struct BashTool<Ctx = ()> {
    sandbox: Sandbox,
    timeout: Duration,
    env_allowlist: Vec<String>,
    max_output_bytes: usize,
    deny_commands: Vec<String>,
    allow_commands: Option<Vec<String>>,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl BashTool<()> {
    /// Start building a `BashTool` over `sandbox` (cwd = `sandbox.root()`),
    /// with a 30s timeout, a `["PATH", "HOME"]` env allowlist, and a 1 MiB
    /// output cap.
    pub fn builder(sandbox: Sandbox) -> BashToolBuilder {
        BashToolBuilder {
            sandbox,
            timeout: DEFAULT_TIMEOUT,
            env_allowlist: vec!["PATH".to_owned(), "HOME".to_owned()],
            max_output_bytes: DEFAULT_MAX_OUTPUT,
            deny_commands: Vec::new(),
            allow_commands: None,
        }
    }
}

impl<Ctx> BashTool<Ctx> {
    fn check_command_allowed(&self, command: &str) -> Result<(), ToolError> {
        let first = command.split_whitespace().next().unwrap_or("");
        if self.deny_commands.iter().any(|d| d == first) {
            return Err(ToolError::Denied {
                reason: format!("command `{first}` is blocked by the deny list"),
            });
        }
        if let Some(allow) = &self.allow_commands {
            if !allow.iter().any(|a| a == first) {
                return Err(ToolError::Denied {
                    reason: format!("command `{first}` is not in the allow list"),
                });
            }
        }
        Ok(())
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for BashTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Run a shell command with the working directory pinned to the sandbox \
         root. WARNING: this is NOT a security sandbox — the command can read/ \
         write anything this process can (absolute paths, the network). It is \
         ungated unless a PermissionPolicy or a DenyRule for `Bash` is installed."
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::SideEffect
    }

    async fn invoke(&self, _ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: BashArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
            schema_errors: vec![e.to_string()],
        })?;
        self.check_command_allowed(&args.command)?;

        let mut cmd = build_command(&args.command);
        cmd.current_dir(self.sandbox.root())
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in &self.env_allowlist {
            if let Ok(val) = std::env::var(name) {
                cmd.env(name, val);
            }
        }
        #[cfg(unix)]
        {
            // New process group so a timeout can kill the whole group.
            cmd.process_group(0);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::Other(anyhow::anyhow!("failed to spawn shell: {e}")))?;
        let mut stdout_pipe = child.stdout.take().expect("piped stdout");
        let mut stderr_pipe = child.stderr.take().expect("piped stderr");

        let cap = self.max_output_bytes;
        let read_out = read_capped(&mut stdout_pipe, cap);
        let read_err = read_capped(&mut stderr_pipe, cap);

        let timed_out;
        let status;
        match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(s) => {
                timed_out = false;
                status = s.map_err(|e| ToolError::Other(e.into()))?;
            }
            Err(_) => {
                timed_out = true;
                let _ = child.start_kill();
                status = child.wait().await.map_err(|e| ToolError::Other(e.into()))?;
            }
        }
        let (stdout, out_trunc) = read_out.await;
        let (stderr, err_trunc) = read_err.await;

        Ok(ToolOutput::new(serde_json::json!({
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": status.code(),
            "timed_out": timed_out,
            "truncated": out_trunc || err_trunc,
        })))
    }
}

/// Build the platform shell command without yet setting cwd/env.
fn build_command(command: &str) -> tokio::process::Command {
    #[cfg(unix)]
    {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    }
    #[cfg(windows)]
    {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    }
}

/// Read up to `cap` bytes from `pipe` as lossy UTF-8; the bool is `true` if the
/// output was truncated at the cap.
async fn read_capped<R>(pipe: &mut R, cap: usize) -> (String, bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    // Read a little past the cap so we can detect truncation, then trim.
    let _ = pipe.take((cap as u64) + 1).read_to_end(&mut buf).await;
    let truncated = buf.len() > cap;
    buf.truncate(cap);
    (String::from_utf8_lossy(&buf).into_owned(), truncated)
}
```

- [ ] **Step 4: Wire into lib.rs**

Add `mod bash;` and extend the re-export so it reads:

```rust
mod bash;
mod edit;
mod read;
mod sandbox;
mod write;

pub use bash::{BashTool, BashToolBuilder};
pub use edit::EditTool;
pub use read::ReadTool;
pub use sandbox::{Sandbox, SandboxError};
pub use write::WriteTool;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools --test bash`
Expected: all `#[cfg(unix)]` Bash tests PASS (on macOS/Linux). On Windows they are compiled out.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/bash.rs crates/paigasus-helikon-tools/src/lib.rs crates/paigasus-helikon-tools/tests/bash.rs
git commit -m "feat(tools): SMA-328 add BashTool with soft confinement"
```

---

## Task 8: `ScriptedModel` + sandbox-navigation AC test

**Files:**
- Create: `crates/paigasus-helikon-tools/tests/common/mod.rs`
- Create: `crates/paigasus-helikon-tools/tests/sandbox_navigation.rs`

- [ ] **Step 1: Write the `ScriptedModel` test double**

Create `crates/paigasus-helikon-tools/tests/common/mod.rs`:

```rust
//! Shared test helpers: a deterministic `Model` that replays scripted events.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::stream;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};
use futures_core::stream::BoxStream;

/// A `Model` that returns one pre-scripted `Vec<ModelEvent>` per `invoke` call,
/// in order. Ignores the request — deterministic, no network.
pub struct ScriptedModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl ScriptedModel {
    /// Construct from one script (event vec) per expected turn.
    pub fn new(scripts: Vec<Vec<ModelEvent>>) -> Self {
        Self {
            scripts: Mutex::new(VecDeque::from(scripts)),
        }
    }
}

#[async_trait]
impl Model for ScriptedModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ModelError::Other(anyhow::anyhow!("no more scripted responses")))?;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}
```

> If the compiler reports `futures_core` is not a dependency, add `futures-core = "0.3"` to the root `[workspace.dependencies]` and `futures-core = { workspace = true }` to the tools crate `[dev-dependencies]`. (`futures-util` re-exports the `BoxStream` alias as `futures_util::stream::BoxStream` — if so, import it from there instead and drop the `futures_core` line.)

- [ ] **Step 2: Write the failing AC test**

Create `crates/paigasus-helikon-tools/tests/sandbox_navigation.rs`:

```rust
#![cfg(unix)]

mod common;

use std::sync::Arc;

use common::ScriptedModel;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, FinishReason, HookRegistry, Item, LlmAgent,
    MemorySession, ModelEvent, RunContext, TracerHandle,
};
use paigasus_helikon_tools::{BashTool, ReadTool, Sandbox};

#[tokio::test]
async fn agent_navigates_sandbox_and_reports_contents() {
    // Sandbox with one known file.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "hello sandbox").unwrap();
    let sandbox = Sandbox::open(tmp.path()).unwrap();

    // Script: turn 0 -> Bash `ls`; turn 1 -> Read `notes.txt`; turn 2 -> answer.
    let model = ScriptedModel::new(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "c1".into(),
                name: Some("Bash".into()),
                args_delta: "{\"command\":\"ls\"}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "c2".into(),
                name: Some("Read".into()),
                args_delta: "{\"path\":\"notes.txt\"}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            ModelEvent::TokenDelta {
                text: "The sandbox contains notes.txt which says: hello sandbox".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);

    let agent = LlmAgent::builder::<()>()
        .name("sandbox-explorer")
        .model(model)
        .instructions("Use the tools to inspect the sandbox, then answer.")
        .tool(ReadTool::<()>::new(sandbox.clone()))
        .tool(BashTool::<()>::builder(sandbox).build())
        .build();

    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );

    let mut stream = agent
        .run(ctx, AgentInput::from_user_text("What's in the sandbox?"))
        .await
        .expect("run starts");

    // Collect every event so we can prove the tools actually ran.
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    // The final assistant text mentions the file.
    let answered = events.iter().any(|e| matches!(
        e,
        AgentEvent::TokenDelta { text } if text.contains("hello sandbox")
    ));
    assert!(answered, "agent should answer with the file contents");

    // Prove ReadTool genuinely read the file: a ToolResult item carries its
    // real output ("hello sandbox"), which is NOT hard-coded into a tool call.
    let read_happened = events.iter().any(|e| match e {
        AgentEvent::ItemDone { item: Item::ToolResult { content, .. } } => {
            format!("{content:?}").contains("hello sandbox")
        }
        _ => false,
    });
    assert!(read_happened, "ReadTool should have returned the file's bytes");
}
```

- [ ] **Step 3: Run the test to verify it fails, then passes**

Run: `cargo test -p paigasus-helikon-tools --test sandbox_navigation`
Expected first run: may FAIL to compile if `AgentEvent`'s variant names differ from `TokenDelta`/`ItemDone`/`Item::ToolResult`. **If so**, run `cargo doc -p paigasus-helikon-core --open` or grep `crates/paigasus-helikon-core/src/agent.rs` for `enum AgentEvent` and `enum Item`, and adjust the two `matches!` arms to the real variant names (the assertion intent — "final text contains the string" and "a tool result contains the read bytes" — stays the same). Then re-run.
Expected after fix: PASS.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/tests/common/mod.rs crates/paigasus-helikon-tools/tests/sandbox_navigation.rs
git commit -m "test(tools): SMA-328 add scripted-model sandbox navigation AC test"
```

---

## Task 9: Real-model example with a `PermissionPolicy`

**Files:**
- Create: `crates/paigasus-helikon-tools/examples/explore_sandbox.rs`

- [ ] **Step 1: Write the example**

Create `crates/paigasus-helikon-tools/examples/explore_sandbox.rs`:

```rust
//! Real-model demo: an agent explores a sandbox with the FS + Bash tools,
//! with the `Bash` tool gated by a `PermissionPolicy`.
//!
//! Run with a key: `OPENAI_API_KEY=... cargo run -p paigasus-helikon-tools \
//!   --example explore_sandbox`

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession,
    PermissionDecision, PermissionPolicy, RunContext, TracerHandle,
};
use paigasus_helikon_providers_openai::OpenAiModel;
use paigasus_helikon_tools::{BashTool, EditTool, ReadTool, Sandbox, WriteTool};

/// Allow everything except `Bash`, which must be explicitly approved by a human
/// (here we simply deny it to model a safe default).
struct GateBash;

#[async_trait]
impl PermissionPolicy<()> for GateBash {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        tool: &str,
        _args: &serde_json::Value,
    ) -> PermissionDecision {
        if tool == "Bash" {
            PermissionDecision::AskUser {
                prompt: "Allow the agent to run a shell command?".to_owned(),
            }
        } else {
            PermissionDecision::Allow
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Use the current directory as the sandbox root for the demo.
    let sandbox = Sandbox::open(".")?;
    let model = OpenAiModel::chat("gpt-5-mini").build()?;

    let agent = LlmAgent::builder::<()>()
        .name("sandbox-explorer")
        .model(model)
        .instructions(
            "You can inspect the sandbox with Read/Write/Edit/Bash. Answer the \
             user's question about its contents concisely.",
        )
        .tool(ReadTool::<()>::new(sandbox.clone()))
        .tool(WriteTool::<()>::new(sandbox.clone()))
        .tool(EditTool::<()>::new(sandbox.clone()))
        .tool(BashTool::<()>::builder(sandbox).build())
        .build();

    // Install the policy on the run context (no approval handler installed, so
    // an AskUser on Bash resolves to Deny — a safe default).
    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
    .with_permission_policy(Arc::new(GateBash));

    let input = AgentInput::from_user_text("List the files here and summarize what this project is.");
    let mut stream = agent.run(ctx, input).await?;
    let mut stdout = std::io::stdout();
    use std::io::Write as _;
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TokenDelta { text } => {
                print!("{text}");
                stdout.flush()?;
            }
            AgentEvent::RunFailed { error } => anyhow::bail!("run failed: {error}"),
            _ => {}
        }
    }
    println!();
    Ok(())
}
```

- [ ] **Step 2: Verify the example compiles**

Run: `cargo build -p paigasus-helikon-tools --example explore_sandbox`
Expected: compiles. (If `AgentEvent::RunFailed`/`TokenDelta` variant names differ, fix them per the `agent.rs` enum — same note as Task 8 Step 3. If `OpenAiModel::chat` signature differs, check `providers-openai/src/lib.rs` doc example.)

> Do NOT run the example in CI; it needs a live `OPENAI_API_KEY`. Compiling it is enough for the gate.

- [ ] **Step 3: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/examples/explore_sandbox.rs
git commit -m "docs(tools): SMA-328 add real-model explore_sandbox example"
```

---

## Task 10: Ascend the crate + release mechanics + full CI gate

**Files:**
- Modify: `release-plz.toml`
- Modify: `crates/paigasus-helikon-core/CHANGELOG.md`, root `Cargo.toml` (`[workspace.dependencies]` core pin)
- Modify: `crates/paigasus-helikon-core/Cargo.toml` (version)
- Modify: `crates/paigasus-helikon/Cargo.toml` (version) + `crates/paigasus-helikon/CHANGELOG.md`
- Modify: `crates/paigasus-helikon-tools/CHANGELOG.md` (create)
- Possibly modify: `deny.toml`

- [ ] **Step 1: Bump core's version (the same-PR core API caveat)**

Because the tools crate consumes `ToolError::Denied` (added to core in this PR), bump core so the published tarball verifies against a fresh registry core. In `crates/paigasus-helikon-core/Cargo.toml`: `version = "0.5.0"` → `version = "0.5.1"`. In the root `Cargo.toml` `[workspace.dependencies]`, bump the `paigasus-helikon-core` pin to `0.5.1`. Add a `## 0.5.1` entry to `crates/paigasus-helikon-core/CHANGELOG.md` noting "add `ToolError::Denied` variant".

- [ ] **Step 2: Confirm the tools crate is in released state**

`crates/paigasus-helikon-tools/Cargo.toml` should already read `version = "0.1.0"` with no `publish = false` (done in Task 2). Create `crates/paigasus-helikon-tools/CHANGELOG.md`:

```markdown
# Changelog

## 0.1.0

- Initial release: `Sandbox` (cap-std) + `ReadTool`, `WriteTool`, `EditTool`, `BashTool`.
```

- [ ] **Step 3: Remove the `-tools` release block from release-plz.toml**

In `release-plz.toml`, delete the `[[package]]` block whose `name = "paigasus-helikon-tools"` (the one with `release = false`). Verify the block is gone:

Run: `grep -n "paigasus-helikon-tools" release-plz.toml`
Expected: no `release = false` block remains for `-tools`.

- [ ] **Step 4: Bump the facade (the facade-drift caveat)**

The facade already declares the optional dep + `tools` feature + `pub use ... as tools` (verified: `crates/paigasus-helikon/Cargo.toml:20,35`, `src/lib.rs:36`) — no wiring changes. Just bump it so it republishes with current sibling reqs: in `crates/paigasus-helikon/Cargo.toml`, `version = "0.3.5"` → `"0.3.6"`, bump its `[workspace.dependencies]` self-pin to `0.3.6`, and add a `## 0.3.6` CHANGELOG entry ("track `paigasus-helikon-tools` 0.1.0").

- [ ] **Step 5: Confirm cargo-deny passes (cap-std licenses)**

Run: `cargo deny check licenses`
Expected: PASS. `cap-std`/`rustix` are `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT`, and `deny.toml` already allows `Apache-2.0` + `MIT`, so the OR fallback satisfies it. **Only if it fails**, add the exact license string it reports to the `[licenses] allow` list in `deny.toml` and re-run.

- [ ] **Step 6: Run the full local CI gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```
Expected: all PASS. Fix any `missing_docs` or intra-doc-link warnings in the tools crate surfaced by the `doc` step (this is where the lib.rs intra-doc links from Task 2 are first enforced).

- [ ] **Step 7: Commit the ascend**

```bash
git add release-plz.toml Cargo.toml \
  crates/paigasus-helikon-core/Cargo.toml crates/paigasus-helikon-core/CHANGELOG.md \
  crates/paigasus-helikon/Cargo.toml crates/paigasus-helikon/CHANGELOG.md \
  crates/paigasus-helikon-tools/CHANGELOG.md deny.toml
git commit -m "chore(release): SMA-328 lift stage-1 gates for tools"
```

> `deny.toml` is only included if Step 5 required a change; otherwise drop it from the `git add`.

- [ ] **Step 8: Push and open the PR**

```bash
git push -u origin feature/sma-328-paigasus-helikon-tools-sandboxed-readwritebashwebfetch
```

Open the PR with a title that satisfies both `pr-title.yml` rules (full Conventional Commits prefix + lowercase subject after the `SMA-###`):

`feat(tools): SMA-328 add sandboxed Read/Write/Edit/Bash harness`

PR body should note: ascends `-tools` 0.0.0→0.1.0; bumps core 0.5.0→0.5.1 (new `ToolError::Denied`) and facade 0.3.5→0.3.6 (drift fix); web tools deferred to SMA-412.

---

## Self-Review notes (against the spec)

- **Spec §2 (two enforcement layers):** tools never call `PermissionPolicy`; only `guard_relative` + cap-std + Bash allow/deny enforce tool-intrinsic invariants. ✓ (Tasks 4–7)
- **Spec §2/§7 (`ToolError::Denied`):** added in Task 1; used for escapes, non-UTF-8, edit preconditions, blocked commands; missing-file → `Other`; bad args → `InvalidArgs`. ✓
- **Spec §5 (cap-std + spawn_blocking):** Sandbox holds `Arc<SandboxInner>`; FS ops run in `spawn_blocking` with a cloned `Sandbox`. ✓ (Tasks 3–6)
- **Spec §6 (four tools, effects, schemas):** Read=ReadOnly, Write/Edit=Write, Bash=SideEffect; schemars-derived schemas; offset/limit on Read; cmd/C on Windows + unix process-group kill. ✓
- **Spec §6.4 / H1 (Bash framing):** crate-level + `description()` security note; example models a `PermissionPolicy`. ✓ (Tasks 2, 7, 9)
- **Spec §8 (tests + demo):** adversarial containment (`../`, absolute, symlink), per-tool behavior, `#[cfg(unix)]` ScriptedModel nav test, real-model example. ✓ (Tasks 4–9)
- **Spec §10 (5-step ascend + facade):** Task 10 covers core bump + pin, tools version, release-plz block, facade bump, deny check. ✓
- **Open verification points flagged inline** (not placeholders): exact `AgentEvent`/`Item` variant names (Task 8/9), `futures_core` vs `futures_util` `BoxStream` path (Task 8), `OpenAiModel::chat` signature (Task 9), tokio feature unification (Task 2). Each has a concrete "grep X / check Y, adjust" instruction.
```

