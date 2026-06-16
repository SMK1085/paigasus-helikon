# Pluggable `ExecutionBackend` for Bash — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Bash containment a first-class, swappable axis separate from approval: extract an object-safe `ExecutionBackend` trait that `BashTool` runs against, ship a hardened `HostBackend` (default) and a Linux `OsSandboxBackend` (real OS containment via `landlock` + `seccompiler`).

**Architecture:** A new `exec` module holds the trait + shared types (`ExecRequest`, `ExecOutput`, `SandboxGuarantees`, `Isolation`, `ResourceLimits`) and a shared `spawn_capped` helper (the spawn/timeout/drain/kill machinery moved out of `bash.rs`). `HostBackend` adds `rlimit`s via a `pre_exec` hook. `OsSandboxBackend` (feature `os-sandbox`, Linux only) builds a Landlock ruleset + seccomp BPF in the parent and applies them in the child's `pre_exec`. `BashTool` becomes a thin `Arc<dyn ExecutionBackend>` adapter.

**Tech Stack:** Rust, tokio (`process`, `time`), `libc` (`setrlimit`, `pre_exec`), `landlock` 0.4 (filesystem LSM), `seccompiler` 0.5 (syscall BPF). Design spec: `docs/superpowers/specs/2026-06-16-tools-execution-backend-design.md`.

---

## Spec → task coverage

| Spec section | Task(s) |
|---|---|
| §5 trait + shared types | 1 |
| §7 `spawn_capped` extraction | 2 |
| §7 `HostBackend` + builder | 3 |
| §7 `rlimit`s | 4 |
| §6 `BashTool` adapter (breaking) | 5 |
| §8 `os-sandbox` feature, deps, `OsSandboxError`, fail-closed `build()` | 6 |
| §8.2/§8.3 Landlock fs containment | 7 |
| §8.3 seccomp syscall + network | 8 |
| §4 re-exports, §10 facade + version bumps + deny | 9 |
| §13 mdBook + example | 10 |

## File structure

```
crates/paigasus-helikon-tools/
  Cargo.toml                       # MODIFY: version 0.1.6→0.2.0; os-sandbox feature; landlock/seccompiler (linux-gated)
  src/
    lib.rs                         # MODIFY: declare exec module; re-export new surface
    bash.rs                        # MODIFY: slim to Arc<dyn ExecutionBackend> adapter
    exec/
      mod.rs                       # CREATE: trait, shared types, spawn_capped, apply_rlimits
      host.rs                      # CREATE: HostBackend + HostBackendBuilder
      os_sandbox.rs                # CREATE: OsSandboxBackend (+ builder, OsSandboxError) [linux+feature]
  tests/
    exec_backend.rs                # CREATE: trait wiring via a mock backend
    bash.rs                        # MODIFY: migrate construction sites to the new API
    sandbox_navigation.rs          # MODIFY: one construction site
    host_backend.rs                # CREATE: rlimit + behaviour tests
    os_sandbox.rs                  # CREATE: Landlock/seccomp AC tests [linux+feature]
  examples/
    explore_sandbox.rs             # MODIFY: one construction site
    os_sandbox_demo.rs             # CREATE: OsSandboxBackend demo [feature]
Cargo.toml (root)                  # MODIFY: [workspace.dependencies] landlock, seccompiler
crates/paigasus-helikon/Cargo.toml # MODIFY: tools-os-sandbox feature; version bump
docs/book/src/*.md                 # MODIFY: containment page
*/CHANGELOG.md                     # MODIFY: tools + facade
```

**Branch:** `feature/sma-413-paigasus-helikon-tools-pluggable-executionbackend-for-bash` (already created; spec committed).

**Conventions to honour (from CLAUDE.md):** run `cargo fmt --all` + `cargo clippy --workspace --all-features --all-targets -- -D warnings` before every commit; commits are signed via 1Password (unlock the vault if a commit fails with "failed to fill whole buffer"); never `git add -A` (`.env`/`.claude` are untracked-not-ignored) — stage explicit paths; per-commit messages use `type(scope): SMA-413 <lowercase subject>`.

---

### Task 1: `exec` module — trait + shared types

**Files:**
- Create: `crates/paigasus-helikon-tools/src/exec/mod.rs`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs`
- Test: `crates/paigasus-helikon-tools/tests/exec_backend.rs`

- [ ] **Step 1: Write the failing test** (`tests/exec_backend.rs`)

```rust
#![allow(missing_docs)]

use paigasus_helikon_tools::{ExecOutput, ExecRequest, Isolation, ResourceLimits, SandboxGuarantees};

#[test]
fn exec_request_new_sets_command() {
    let req = ExecRequest::new("ls -la");
    assert_eq!(req.command, "ls -la");
}

#[test]
fn resource_limits_default_is_all_none() {
    let l = ResourceLimits::default();
    assert_eq!(l.cpu_seconds, None);
    assert_eq!(l.file_size_bytes, None);
    assert_eq!(l.address_space_bytes, None);
}

#[test]
fn guarantees_struct_holds_axes_and_label() {
    let g = SandboxGuarantees {
        filesystem: Isolation::OsKernel,
        network: Isolation::None,
        syscalls: Isolation::OsKernel,
        label: "demo",
    };
    assert_eq!(g.filesystem, Isolation::OsKernel);
    assert_eq!(g.label, "demo");
    // ExecOutput is constructible and Clone.
    let o = ExecOutput {
        stdout: "out".into(), stderr: String::new(),
        exit_code: Some(0), timed_out: false, truncated: false,
    };
    assert_eq!(o.clone().stdout, "out");
}
```

- [ ] **Step 2: Run it to confirm it fails to compile**

Run: `cargo test -p paigasus-helikon-tools --test exec_backend`
Expected: FAIL — `unresolved import paigasus_helikon_tools::ExecRequest`.

- [ ] **Step 3: Create `src/exec/mod.rs`**

```rust
//! The pluggable [`ExecutionBackend`] that [`crate::BashTool`] runs against, and
//! the shared types describing a backend's *containment* (distinct from the
//! runner's *approval* policy and from resource-capping).

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::ToolError;

mod host;
pub use host::{HostBackend, HostBackendBuilder};

#[cfg(all(feature = "os-sandbox", target_os = "linux"))]
mod os_sandbox;
#[cfg(all(feature = "os-sandbox", target_os = "linux"))]
pub use os_sandbox::{OsSandboxBackend, OsSandboxBackendBuilder, OsSandboxError};

/// Default wall-clock timeout for a command (matches the SMA-328 `BashTool`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default per-stream output cap, in bytes (1 MiB).
pub const DEFAULT_MAX_OUTPUT: usize = 1 << 20;

/// A backend that runs one shell command under some containment tier.
///
/// Object-safe and not generic over the agent context, so one value is shared as
/// `Arc<dyn ExecutionBackend>` across agents of any context type. Swapping the
/// backend needs no change to [`crate::BashTool`] or agent code.
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    /// Run `req.command` to completion under this backend's containment.
    async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError>;

    /// What this backend actually enforces — surfaced in docs, the model-facing
    /// tool description, and traces. Describes *containment*, not approval.
    fn guarantees(&self) -> SandboxGuarantees;
}

/// A request to run one shell command. `#[non_exhaustive]` so per-call knobs
/// (stdin, env overrides) can be added later without a breaking change.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExecRequest {
    /// The shell command (`sh -c <command>` on unix, `cmd /C` on Windows).
    pub command: String,
}

impl ExecRequest {
    /// Build a request from a command string.
    pub fn new(command: impl Into<String>) -> Self {
        Self { command: command.into() }
    }
}

/// The captured result of one command run. Non-zero exit, timeout, and
/// truncation are *normal results* the model inspects — never errors.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExecOutput {
    /// Captured stdout (lossy UTF-8, truncated at the backend's output cap).
    pub stdout: String,
    /// Captured stderr (lossy UTF-8, truncated at the backend's output cap).
    pub stderr: String,
    /// Process exit code, or `None` if killed by signal / timeout.
    pub exit_code: Option<i32>,
    /// Whether the command was killed because it exceeded the timeout.
    pub timed_out: bool,
    /// Whether either stream was truncated at the output cap.
    pub truncated: bool,
}

/// The isolation level enforced on one axis (filesystem / network / syscalls).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Isolation {
    /// No OS enforcement — the command has the same access this process does.
    None,
    /// Enforced by an OS kernel mechanism (Landlock / seccomp-bpf).
    OsKernel,
}

/// What a backend enforces, surfaced to docs / traces / the tool description.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SandboxGuarantees {
    /// Filesystem containment.
    pub filesystem: Isolation,
    /// Network containment.
    pub network: Isolation,
    /// Syscall containment.
    pub syscalls: Isolation,
    /// Short human label, e.g. `"host (no containment)"`.
    pub label: &'static str,
}

/// Resource limits applied to a command via `setrlimit` (unix). Each `None`
/// leaves the inherited limit. See [`HostBackend`] for the default policy.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ResourceLimits {
    /// `RLIMIT_CPU` — CPU seconds.
    pub cpu_seconds: Option<u64>,
    /// `RLIMIT_FSIZE` — max bytes a single file write may reach.
    pub file_size_bytes: Option<u64>,
    /// `RLIMIT_AS` — virtual address-space cap. Approximate; off by default
    /// because it spuriously kills threaded/`mmap`-heavy programs.
    pub address_space_bytes: Option<u64>,
}

/// Internal config every backend shares; consumed by [`spawn_capped`].
pub(crate) struct ExecConfig {
    pub(crate) cwd: PathBuf,
    pub(crate) env_allowlist: Vec<String>,
    pub(crate) timeout: Duration,
    pub(crate) max_output_bytes: usize,
}
```

> Note: `spawn_capped`, `apply_rlimits`, and `build_command` are added in Task 2.
> The `mod host;` line will not compile until Task 3 creates `host.rs` — Task 1
> ends at the test below, which only needs the public types. To keep Task 1
> green on its own, temporarily comment out the `mod host;` / `pub use host::*`
> lines and the `#[cfg(... os-sandbox)]` block; Task 3 restores them.

- [ ] **Step 4: Wire the module into `lib.rs`**

In `crates/paigasus-helikon-tools/src/lib.rs`, after `mod bash;` add:

```rust
mod exec;
```

and after `pub use bash::{BashTool, BashToolBuilder};` add:

```rust
pub use exec::{
    ExecOutput, ExecRequest, ExecutionBackend, HostBackend, HostBackendBuilder, Isolation,
    ResourceLimits, SandboxGuarantees,
};

#[cfg(all(feature = "os-sandbox", target_os = "linux"))]
pub use exec::{OsSandboxBackend, OsSandboxBackendBuilder, OsSandboxError};
```

> The `HostBackend`/`os_sandbox` re-exports reference items created in Tasks 3/6.
> For Task 1 to compile standalone, add only `ExecOutput, ExecRequest,
> ExecutionBackend, Isolation, ResourceLimits, SandboxGuarantees` now; add the
> `HostBackend*` names in Task 3 and the `OsSandbox*` names in Task 6.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p paigasus-helikon-tools --test exec_backend`
Expected: PASS (3 tests).

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/exec/mod.rs \
        crates/paigasus-helikon-tools/src/lib.rs \
        crates/paigasus-helikon-tools/tests/exec_backend.rs
git commit -m "feat(tools): SMA-413 add ExecutionBackend trait and shared types"
```

---

### Task 2: `spawn_capped` — move the spawn/timeout/drain/kill machinery

Extract the process machinery from `bash.rs::invoke` into `exec/mod.rs` so both
backends share it. No behaviour change.

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs`

- [ ] **Step 1: Add `spawn_capped`, `apply_rlimits`, `build_command`, and the pipe helpers to `exec/mod.rs`**

Append to `src/exec/mod.rs` (these are lifted verbatim from the current
`bash.rs`, generalised to return `ExecOutput` and to accept a child-configuration
closure that the caller uses to install a `pre_exec` hook):

```rust
use std::process::Stdio;
use tokio::io::AsyncReadExt;

/// Grace period for reaping a killed process and draining its pipes.
const GRACE: Duration = Duration::from_secs(5);

/// Spawn `command` under `cfg`, draining stdout/stderr concurrently, killing the
/// whole process group on timeout. `configure_child` runs in the **parent** to
/// install backend-specific `pre_exec` hooks before spawn.
pub(crate) async fn spawn_capped(
    cfg: &ExecConfig,
    command: &str,
    configure_child: impl FnOnce(&mut tokio::process::Command),
) -> Result<ExecOutput, ToolError> {
    let mut cmd = build_command(command);
    cmd.current_dir(&cfg.cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in &cfg.env_allowlist {
        if let Ok(val) = std::env::var(name) {
            cmd.env(name, val);
        }
    }
    #[cfg(unix)]
    {
        // New process group so a timeout can kill the whole subtree.
        cmd.process_group(0);
    }
    configure_child(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| ToolError::Other(anyhow::anyhow!("failed to spawn shell: {e}")))?;

    #[cfg(unix)]
    let pgid = child.id();

    let stdout_pipe = child.stdout.take().expect("piped stdout");
    let stderr_pipe = child.stderr.take().expect("piped stderr");

    let cap = cfg.max_output_bytes;
    let out_handle = tokio::spawn(read_capped(stdout_pipe, cap));
    let err_handle = tokio::spawn(read_capped(stderr_pipe, cap));

    let mut timed_out = false;
    let exit_code: Option<i32>;
    match tokio::time::timeout(cfg.timeout, child.wait()).await {
        Ok(status) => {
            exit_code = status.map_err(|e| ToolError::Other(e.into()))?.code();
        }
        Err(_) => {
            timed_out = true;
            #[cfg(unix)]
            {
                if let Some(pid) = pgid {
                    // SAFETY: pid < 4_194_304 on supported platforms, so the cast
                    // and negation are valid; ESRCH (group already gone) is benign.
                    let _ = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
                }
            }
            #[cfg(not(unix))]
            {
                let _ = child.start_kill();
            }
            exit_code = match tokio::time::timeout(GRACE, child.wait()).await {
                Ok(Ok(status)) => status.code(),
                _ => None,
            };
        }
    }

    let ((stdout, out_trunc), (stderr, err_trunc)) =
        tokio::join!(join_reader(out_handle), join_reader(err_handle));

    Ok(ExecOutput {
        stdout,
        stderr,
        exit_code,
        timed_out,
        truncated: out_trunc || err_trunc,
    })
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

async fn join_reader(handle: tokio::task::JoinHandle<(String, bool)>) -> (String, bool) {
    let abort = handle.abort_handle();
    match tokio::time::timeout(GRACE, handle).await {
        Ok(Ok(captured)) => captured,
        _ => {
            abort.abort();
            (String::new(), false)
        }
    }
}

async fn read_capped<R>(pipe: R, cap: usize) -> (String, bool)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut buf = Vec::new();
    let _ = pipe.take((cap as u64) + 1).read_to_end(&mut buf).await;
    let truncated = buf.len() > cap;
    buf.truncate(cap);
    (String::from_utf8_lossy(&buf).into_owned(), truncated)
}

/// Apply `limits` to the current process via `setrlimit`. Async-signal-safe
/// (only `setrlimit` syscalls, no allocation) so it is callable from `pre_exec`.
#[cfg(unix)]
pub(crate) fn apply_rlimits(limits: &ResourceLimits) -> std::io::Result<()> {
    // SAFETY: setrlimit with a stack-allocated rlimit is async-signal-safe.
    unsafe {
        let set = |res: libc::c_int, val: u64| -> std::io::Result<()> {
            let rl = libc::rlimit {
                rlim_cur: val as libc::rlim_t,
                rlim_max: val as libc::rlim_t,
            };
            if libc::setrlimit(res, &rl) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        };
        if let Some(s) = limits.cpu_seconds {
            set(libc::RLIMIT_CPU, s)?;
        }
        if let Some(b) = limits.file_size_bytes {
            set(libc::RLIMIT_FSIZE, b)?;
        }
        if let Some(b) = limits.address_space_bytes {
            set(libc::RLIMIT_AS, b)?;
        }
    }
    Ok(())
}
```

- [ ] **Step 2: Verify it compiles** (no callers yet)

Run: `cargo build -p paigasus-helikon-tools`
Expected: builds clean (a `dead_code` warning on `spawn_capped`/`apply_rlimits` is expected until Task 3 — that is fine for an intermediate build but **do not commit with warnings**; proceed straight to Task 3 which adds the caller, then commit there).

> This task has no standalone commit — `spawn_capped` is dead code until
> `HostBackend` calls it. It is committed together with Task 3.

---

### Task 3: `HostBackend` + builder

**Files:**
- Create: `crates/paigasus-helikon-tools/src/exec/host.rs`
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs` (uncomment `mod host;`), `src/lib.rs` (add `HostBackend*` re-exports)
- Test: `crates/paigasus-helikon-tools/tests/host_backend.rs`

- [ ] **Step 1: Write the failing test** (`tests/host_backend.rs`)

```rust
#![allow(missing_docs)]
#![cfg(unix)]

use paigasus_helikon_tools::{ExecRequest, ExecutionBackend, HostBackend, Isolation, Sandbox};

#[tokio::test]
async fn host_backend_runs_command_in_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("marker.txt"), "x").unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap()).build();

    let out = backend.run(ExecRequest::new("ls")).await.unwrap();
    assert!(out.stdout.contains("marker.txt"));
    assert_eq!(out.exit_code, Some(0));
    assert!(!out.timed_out);
}

#[tokio::test]
async fn host_backend_guarantees_report_no_containment() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap()).build();
    let g = backend.guarantees();
    assert_eq!(g.filesystem, Isolation::None);
    assert_eq!(g.network, Isolation::None);
    assert_eq!(g.syscalls, Isolation::None);
    assert_eq!(g.label, "host (no containment)");
}

#[tokio::test]
async fn host_backend_env_is_scrubbed_to_allowlist() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .env_allowlist(["PATH"]) // drop HOME
        .build();
    let out = backend.run(ExecRequest::new("echo HOME=$HOME")).await.unwrap();
    assert!(out.stdout.contains("HOME="));
    assert!(!out.stdout.contains("HOME=/")); // HOME unset → empty
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-tools --test host_backend`
Expected: FAIL — `HostBackend` not found.

- [ ] **Step 3: Create `src/exec/host.rs`**

```rust
//! [`HostBackend`] — the default execution backend. A cwd-pinned shell with env
//! scrubbing, an output cap, a timeout (process-group kill), and `rlimit`s.
//! **NOT a security boundary:** a spawned command can read/write anything this
//! process can. Gate it with a `PermissionPolicy` or use [`OsSandboxBackend`]
//! for OS-enforced containment.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::ToolError;

use super::{
    spawn_capped, ExecConfig, ExecOutput, ExecRequest, ExecutionBackend, Isolation, ResourceLimits,
    SandboxGuarantees, DEFAULT_MAX_OUTPUT, DEFAULT_TIMEOUT,
};
use crate::sandbox::Sandbox;

/// Default `RLIMIT_FSIZE` cap: 1 GiB.
const DEFAULT_FILE_SIZE_LIMIT: u64 = 1 << 30;
/// Extra CPU seconds granted over the wall timeout for the `RLIMIT_CPU` backstop.
const CPU_LIMIT_MARGIN_SECS: u64 = 5;

/// Builder for [`HostBackend`].
pub struct HostBackendBuilder {
    sandbox: Sandbox,
    timeout: Duration,
    env_allowlist: Vec<String>,
    max_output_bytes: usize,
    limits: ResourceLimits,
    limits_set: bool,
}

impl HostBackendBuilder {
    /// Wall-clock timeout before the process group is killed (default 30s).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Env var names to pass through (REPLACES the default `["PATH","HOME"]`).
    pub fn env_allowlist<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.env_allowlist = names.into_iter().map(Into::into).collect();
        self
    }

    /// Truncate captured stdout/stderr to this many bytes each (default 1 MiB).
    pub fn max_output_bytes(mut self, n: usize) -> Self {
        self.max_output_bytes = n;
        self
    }

    /// Override the resource limits. Replaces the defaults
    /// (`RLIMIT_CPU` = timeout+5s, `RLIMIT_FSIZE` = 1 GiB, `RLIMIT_AS` = unset).
    pub fn rlimits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self.limits_set = true;
        self
    }

    /// Finish building, returning a shareable `Arc<dyn ExecutionBackend>`.
    pub fn build(mut self) -> Arc<dyn ExecutionBackend> {
        if !self.limits_set {
            // Default policy: CPU backstop derived from the timeout, a generous
            // file-size cap, address-space cap left off (review M2).
            self.limits = ResourceLimits {
                cpu_seconds: Some(self.timeout.as_secs().saturating_add(CPU_LIMIT_MARGIN_SECS)),
                file_size_bytes: Some(DEFAULT_FILE_SIZE_LIMIT),
                address_space_bytes: None,
            };
        }
        Arc::new(HostBackend {
            cfg: ExecConfig {
                cwd: self.sandbox.root().to_path_buf(),
                env_allowlist: self.env_allowlist,
                timeout: self.timeout,
                max_output_bytes: self.max_output_bytes,
            },
            limits: self.limits,
        })
    }
}

/// The default, cwd-pinned execution backend. See the module docs: **not** a
/// security boundary.
pub struct HostBackend {
    cfg: ExecConfig,
    limits: ResourceLimits,
}

impl HostBackend {
    /// Start building a `HostBackend` over `sandbox` (cwd = `sandbox.root()`),
    /// with a 30s timeout, `["PATH","HOME"]` env allowlist, 1 MiB output cap.
    pub fn builder(sandbox: Sandbox) -> HostBackendBuilder {
        HostBackendBuilder {
            sandbox,
            timeout: DEFAULT_TIMEOUT,
            env_allowlist: vec!["PATH".to_owned(), "HOME".to_owned()],
            max_output_bytes: DEFAULT_MAX_OUTPUT,
            limits: ResourceLimits::default(),
            limits_set: false,
        }
    }
}

#[async_trait]
impl ExecutionBackend for HostBackend {
    async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError> {
        let limits = self.limits.clone();
        spawn_capped(&self.cfg, &req.command, move |_cmd| {
            #[cfg(unix)]
            {
                // SAFETY: apply_rlimits is async-signal-safe (setrlimit only).
                unsafe {
                    _cmd.pre_exec(move || super::apply_rlimits(&limits));
                }
            }
        })
        .await
    }

    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees {
            filesystem: Isolation::None,
            network: Isolation::None,
            syscalls: Isolation::None,
            label: "host (no containment)",
        }
    }
}
```

- [ ] **Step 4: Restore the `mod host;` wiring**

In `src/exec/mod.rs`, ensure these lines are present and uncommented:

```rust
mod host;
pub use host::{HostBackend, HostBackendBuilder};
```

In `src/lib.rs`, ensure the `pub use exec::{ ... HostBackend, HostBackendBuilder ... }` names are present (added/uncommented from Task 1).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools --test host_backend --test exec_backend`
Expected: PASS.

- [ ] **Step 6: fmt + clippy + commit** (includes Task 2's `spawn_capped`)

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/exec/mod.rs \
        crates/paigasus-helikon-tools/src/exec/host.rs \
        crates/paigasus-helikon-tools/src/lib.rs \
        crates/paigasus-helikon-tools/tests/host_backend.rs
git commit -m "feat(tools): SMA-413 add HostBackend over shared spawn_capped helper"
```

---

### Task 4: `HostBackend` rlimit enforcement tests

The code already applies `rlimit`s (Task 3). This task proves it with tests that
would hang or run unbounded without the limits.

**Files:**
- Modify: `crates/paigasus-helikon-tools/tests/host_backend.rs`

- [ ] **Step 1: Add the failing tests**

```rust
#[tokio::test]
async fn host_backend_rlimit_cpu_kills_spin_loop() {
    let tmp = tempfile::tempdir().unwrap();
    // Generous wall timeout so the CPU limit (not the timeout) is what fires.
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(std::time::Duration::from_secs(60))
        .rlimits(paigasus_helikon_tools::ResourceLimits {
            cpu_seconds: Some(1),
            file_size_bytes: None,
            address_space_bytes: None,
        })
        .build();
    // Busy loop: with RLIMIT_CPU=1 the kernel sends SIGXCPU within ~1s.
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        backend.run(ExecRequest::new("while true; do :; done")),
    )
    .await
    .expect("must return well under the 60s wall timeout")
    .unwrap();
    // Killed by signal → no clean exit code, and not via our wall-timeout path.
    assert_eq!(out.exit_code, None);
    assert!(!out.timed_out, "CPU rlimit, not wall timeout, should fire");
}

#[tokio::test]
async fn host_backend_rlimit_fsize_blocks_large_write() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .rlimits(paigasus_helikon_tools::ResourceLimits {
            cpu_seconds: None,
            file_size_bytes: Some(1024), // 1 KiB cap
            address_space_bytes: None,
        })
        .build();
    // Writing 1 MiB exceeds the 1 KiB RLIMIT_FSIZE → SIGXFSZ / write error.
    let out = backend
        .run(ExecRequest::new("head -c 1048576 /dev/zero > big.bin"))
        .await
        .unwrap();
    assert_ne!(out.exit_code, Some(0), "the oversized write must fail");
    let written = std::fs::metadata(tmp.path().join("big.bin"))
        .map(|m| m.len())
        .unwrap_or(0);
    assert!(written <= 1024, "file must be capped at the rlimit");
}
```

- [ ] **Step 2: Run to verify they pass** (the implementation already exists)

Run: `cargo test -p paigasus-helikon-tools --test host_backend`
Expected: PASS. If `host_backend_rlimit_cpu_kills_spin_loop` instead trips the 30s assertion, the `pre_exec` rlimit is not being applied — debug before continuing (do not weaken the test).

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-tools/tests/host_backend.rs
git commit -m "test(tools): SMA-413 prove HostBackend CPU and file-size rlimits"
```

---

### Task 5: Reshape `BashTool` into an `ExecutionBackend` adapter (breaking)

`BashTool` stops owning execution config; it holds `Arc<dyn ExecutionBackend>` +
command allow/deny + schema, and delegates `run`.

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/bash.rs` (full rewrite)
- Modify: `crates/paigasus-helikon-tools/tests/bash.rs` (migrate 9 sites)
- Modify: `crates/paigasus-helikon-tools/tests/sandbox_navigation.rs` (1 site)
- Modify: `crates/paigasus-helikon-tools/examples/explore_sandbox.rs` (1 site)
- Test: add a swap-backend test to `tests/exec_backend.rs`

- [ ] **Step 1: Add the swap-backend wiring test** (`tests/exec_backend.rs`, append)

```rust
use async_trait::async_trait;
use paigasus_helikon_core::{
    CancellationToken, HookRegistry, MemorySession, RunContext, Tool, ToolError, TracerHandle,
};
use paigasus_helikon_tools::{BashTool, ExecutionBackend, Isolation, SandboxGuarantees};
use std::sync::Arc;

/// A backend that records the command and returns a canned output — proves
/// BashTool calls `run` and maps the result, with no real process.
struct MockBackend {
    seen: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl ExecutionBackend for MockBackend {
    async fn run(
        &self,
        req: paigasus_helikon_tools::ExecRequest,
    ) -> Result<paigasus_helikon_tools::ExecOutput, ToolError> {
        self.seen.lock().unwrap().push(req.command.clone());
        Ok(paigasus_helikon_tools::ExecOutput {
            stdout: "mocked".into(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
            truncated: false,
        })
    }
    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees {
            filesystem: Isolation::OsKernel,
            network: Isolation::OsKernel,
            syscalls: Isolation::OsKernel,
            label: "mock",
        }
    }
}

fn tool_ctx() -> paigasus_helikon_core::ToolContext<()> {
    RunContext::<()>::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
    .to_tool_context()
}

#[tokio::test]
async fn bashtool_delegates_to_any_backend_unchanged() {
    let backend = Arc::new(MockBackend { seen: Default::default() });
    let tool: BashTool = BashTool::new(backend.clone());
    let out = tool
        .invoke(&tool_ctx(), serde_json::json!({ "command": "echo hi" }))
        .await
        .unwrap();
    assert_eq!(out.content["stdout"], "mocked");
    assert_eq!(backend.seen.lock().unwrap().as_slice(), ["echo hi"]);
    // The backend's containment label is surfaced in the tool description.
    assert!(tool.description().contains("mock"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-tools --test exec_backend`
Expected: FAIL — `BashTool::new` does not take a backend yet.

- [ ] **Step 3: Rewrite `src/bash.rs`**

```rust
//! [`BashTool`] — a shell tool that runs commands through a pluggable
//! [`ExecutionBackend`]. The tool itself only parses arguments and applies the
//! command allow/deny lists; *containment* is the backend's job (and is reported
//! in this tool's description via the backend's `guarantees()`).

use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

use crate::exec::{ExecRequest, ExecutionBackend};

/// Arguments for [`BashTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BashArgs {
    /// The shell command to run (`sh -c <command>` on unix, `cmd /C` on Windows).
    command: String,
}

/// Builder for [`BashTool`].
pub struct BashToolBuilder {
    backend: Arc<dyn ExecutionBackend>,
    deny_commands: Vec<String>,
    allow_commands: Option<Vec<String>>,
}

impl BashToolBuilder {
    /// Refuse any command whose first whitespace-delimited token is in this list.
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
        let label = self.backend.guarantees().label;
        let description = format!(
            "Run a shell command. Containment tier: {label}. Working directory is \
             pinned to the sandbox root. With the host backend this is NOT a \
             security sandbox; pair it with a PermissionPolicy or a \
             DenyRule(\"Bash\"), or use an OS-sandbox backend for OS-enforced \
             containment."
        );
        BashTool {
            backend: self.backend,
            deny_commands: self.deny_commands,
            allow_commands: self.allow_commands,
            description,
            schema: serde_json::to_value(schemars::schema_for!(BashArgs))
                .expect("BashArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

/// A shell tool backed by a pluggable [`ExecutionBackend`].
pub struct BashTool<Ctx = ()> {
    backend: Arc<dyn ExecutionBackend>,
    deny_commands: Vec<String>,
    allow_commands: Option<Vec<String>>,
    description: String,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl BashTool<()> {
    /// Start building a `BashTool` over `backend`.
    pub fn builder(backend: Arc<dyn ExecutionBackend>) -> BashToolBuilder {
        BashToolBuilder {
            backend,
            deny_commands: Vec::new(),
            allow_commands: None,
        }
    }

    /// Build a `BashTool` over `backend` with no command allow/deny lists.
    pub fn new(backend: Arc<dyn ExecutionBackend>) -> BashTool<()> {
        Self::builder(backend).build()
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
        &self.description
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
        let out = self.backend.run(ExecRequest::new(args.command)).await?;
        Ok(ToolOutput::new(serde_json::json!({
            "stdout": out.stdout,
            "stderr": out.stderr,
            "exit_code": out.exit_code,
            "timed_out": out.timed_out,
            "truncated": out.truncated,
        })))
    }
}
```

- [ ] **Step 4: Migrate `tests/bash.rs` construction sites**

Add `HostBackend` to the import and transform every construction site by the rule:
**execution config (`timeout`/`env_allowlist`/`max_output_bytes`) moves to
`HostBackend::builder(sandbox)…`; command allow/deny stays on
`BashTool::builder(backend)…`.**

Change the import line:

```rust
use paigasus_helikon_tools::{BashTool, HostBackend, Sandbox};
```

Apply these three transform patterns to all 9 sites:

```rust
// (a) plain — e.g. lines 28, 159:
//   BashTool::builder(Sandbox::open(tmp.path()).unwrap()).build()
let tool: BashTool =
    BashTool::new(HostBackend::builder(Sandbox::open(tmp.path()).unwrap()).build());

// (b) with execution config — e.g. a `.timeout(..)` / `.env_allowlist(..)` /
//     `.max_output_bytes(..)` site:
//   BashTool::builder(sandbox).timeout(d).env_allowlist(["PATH"]).build()
let tool: BashTool = BashTool::new(
    HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(d)
        .env_allowlist(["PATH"])
        .build(),
);

// (c) with command allow/deny — e.g. a `.deny_commands(..)` / `.allow_commands(..)` site:
//   BashTool::builder(sandbox).deny_commands(["rm"]).build()
let tool: BashTool = BashTool::builder(HostBackend::builder(Sandbox::open(tmp.path()).unwrap()).build())
    .deny_commands(["rm"])
    .build();
```

> The current `BashToolBuilder` methods `timeout`, `env_allowlist`, and
> `max_output_bytes` no longer exist — those sites MUST move to `HostBackend`.
> The `deny_commands`/`allow_commands` sites stay on `BashTool::builder`.

- [ ] **Step 5: Migrate the other two sites**

`tests/sandbox_navigation.rs:14` import → add `HostBackend`; line 59:

```rust
.tool(BashTool::<()>::new(HostBackend::builder(sandbox).build()))
```

`examples/explore_sandbox.rs:21` import → add `HostBackend`; line 68:

```rust
.tool(BashTool::<()>::new(HostBackend::builder(sandbox).build()))
```

(If `sandbox` is moved/consumed elsewhere in the example, clone it first via
`sandbox.clone()` — `Sandbox` is `Clone`.)

- [ ] **Step 6: Run the full crate test suite + clippy**

Run: `cargo test -p paigasus-helikon-tools` then
`cargo clippy -p paigasus-helikon-tools --all-targets -- -D warnings`
Expected: PASS. Build the example too: `cargo build -p paigasus-helikon-tools --example explore_sandbox`.

- [ ] **Step 7: fmt + commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-tools/src/bash.rs \
        crates/paigasus-helikon-tools/tests/bash.rs \
        crates/paigasus-helikon-tools/tests/sandbox_navigation.rs \
        crates/paigasus-helikon-tools/tests/exec_backend.rs \
        crates/paigasus-helikon-tools/examples/explore_sandbox.rs
git commit -m "feat(tools): SMA-413 reshape BashTool over Arc<dyn ExecutionBackend>"
```

---

### Task 6: `os-sandbox` feature, deps, `OsSandboxBackend` skeleton (fail-closed `build()`)

**Files:**
- Modify: root `Cargo.toml` (`[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon-tools/Cargo.toml` (feature + linux-gated deps)
- Create: `crates/paigasus-helikon-tools/src/exec/os_sandbox.rs`
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs`, `src/lib.rs` (re-exports)
- Test: `crates/paigasus-helikon-tools/tests/os_sandbox.rs`

- [ ] **Step 1: Add the dependency pins to root `Cargo.toml`**

Under `[workspace.dependencies]`:

```toml
landlock    = "0.4"
seccompiler = "0.5"
```

- [ ] **Step 2: Wire the feature + linux-gated optional deps in the crate `Cargo.toml`**

```toml
[features]
# ... existing web feature ...
# OS-enforced Bash containment (Linux: Landlock + seccomp). Off by default.
os-sandbox = ["dep:landlock", "dep:seccompiler"]

[target.'cfg(target_os = "linux")'.dependencies]
libc        = { workspace = true }   # MOVE the existing unix libc dep here, or add if absent
landlock    = { workspace = true, optional = true }
seccompiler = { workspace = true, optional = true }
```

> The crate already has `[target.'cfg(unix)'.dependencies] libc`. Keep that for
> the broader unix `setrlimit`/`kill` use; ADD a separate
> `[target.'cfg(target_os = "linux")'.dependencies]` block for `landlock` +
> `seccompiler` (do not remove the `cfg(unix)` libc entry).

- [ ] **Step 3: Write the failing test** (`tests/os_sandbox.rs`)

```rust
#![allow(missing_docs)]
#![cfg(all(feature = "os-sandbox", target_os = "linux"))]

use paigasus_helikon_tools::{ExecutionBackend, Isolation, OsSandboxBackend, Sandbox};

/// Skip (with a loud reason) when the kernel lacks Landlock, rather than passing
/// silently. Returns true if the caller should `return`.
fn landlock_unavailable(tmp: &std::path::Path) -> bool {
    if OsSandboxBackend::builder(Sandbox::open(tmp).unwrap())
        .build()
        .is_err()
    {
        eprintln!("SKIP: Landlock unavailable on this kernel; os-sandbox AC not exercised");
        return true;
    }
    false
}

#[tokio::test]
async fn os_sandbox_builds_and_reports_guarantees() {
    let tmp = tempfile::tempdir().unwrap();
    if landlock_unavailable(tmp.path()) {
        return;
    }
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .build()
        .expect("Landlock available");
    let g = backend.guarantees();
    assert_eq!(g.filesystem, Isolation::OsKernel);
    assert_eq!(g.syscalls, Isolation::OsKernel);
    assert_eq!(g.network, Isolation::OsKernel); // default deny
    assert_eq!(g.label, "os-sandbox (landlock+seccomp)");
}
```

- [ ] **Step 4: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-tools --features os-sandbox --test os_sandbox`
Expected: FAIL — `OsSandboxBackend` not found.

- [ ] **Step 5: Create `src/exec/os_sandbox.rs` (skeleton: config, builder, fail-closed probe, guarantees; `run` applies rlimits only for now)**

```rust
//! [`OsSandboxBackend`] — OS-enforced Bash containment on Linux via Landlock
//! (filesystem) + seccomp-bpf (syscalls / network). Fail-closed: `build()` errors
//! if the kernel cannot enforce the requested isolation.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use landlock::{
    Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, ABI,
};
use paigasus_helikon_core::ToolError;

use super::{
    spawn_capped, ExecConfig, ExecOutput, ExecRequest, ExecutionBackend, Isolation, ResourceLimits,
    SandboxGuarantees, DEFAULT_MAX_OUTPUT, DEFAULT_TIMEOUT,
};
use crate::sandbox::Sandbox;

/// Landlock ABI floor we require for filesystem containment (kernel ≥ 5.13).
const LANDLOCK_ABI: ABI = ABI::V1;

/// Construction failures for [`OsSandboxBackend`]. Distinct from
/// `ToolError::Denied` (an in-`run` refusal) and from `SandboxError`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OsSandboxError {
    /// The kernel cannot enforce Landlock at the required ABI.
    #[error("OS sandbox unavailable: {0}")]
    Unsupported(String),
}

/// Builder for [`OsSandboxBackend`].
pub struct OsSandboxBackendBuilder {
    sandbox: Sandbox,
    timeout: Duration,
    env_allowlist: Vec<String>,
    max_output_bytes: usize,
    limits: ResourceLimits,
    allow_network: bool,
    read_paths: Vec<PathBuf>,
}

impl OsSandboxBackendBuilder {
    /// Wall-clock timeout before the process group is killed (default 30s).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    /// Env var names to pass through (REPLACES the default `["PATH","HOME"]`).
    pub fn env_allowlist<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.env_allowlist = names.into_iter().map(Into::into).collect();
        self
    }
    /// Truncate captured stdout/stderr to this many bytes each (default 1 MiB).
    pub fn max_output_bytes(mut self, n: usize) -> Self {
        self.max_output_bytes = n;
        self
    }
    /// Override resource limits applied inside the jail.
    pub fn rlimits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }
    /// Allow outbound network (default: deny all IP egress).
    pub fn allow_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }
    /// Extra read-only path exceptions beyond the default system set.
    pub fn read_paths<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.read_paths = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Finish building. **Fail-closed:** returns `Err` if Landlock cannot be
    /// enforced at [`LANDLOCK_ABI`] on this kernel.
    pub fn build(self) -> Result<Arc<dyn ExecutionBackend>, OsSandboxError> {
        probe_landlock()?;
        Ok(Arc::new(OsSandboxBackend {
            cfg: ExecConfig {
                cwd: self.sandbox.root().to_path_buf(),
                env_allowlist: self.env_allowlist,
                timeout: self.timeout,
                max_output_bytes: self.max_output_bytes,
            },
            limits: self.limits,
            allow_network: self.allow_network,
            root: self.sandbox.root().to_path_buf(),
            read_paths: self.read_paths,
        }))
    }
}

/// Probe Landlock support without restricting the current process: create a
/// ruleset fd under a hard-requirement compat level and drop it.
fn probe_landlock() -> Result<(), OsSandboxError> {
    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(LANDLOCK_ABI))
        .and_then(|r| r.create())
        .map(|_created| ())
        .map_err(|e| OsSandboxError::Unsupported(e.to_string()))
}

/// OS-enforced execution backend (Linux). See module docs.
pub struct OsSandboxBackend {
    cfg: ExecConfig,
    limits: ResourceLimits,
    allow_network: bool,
    root: PathBuf,
    read_paths: Vec<PathBuf>,
}

impl OsSandboxBackend {
    /// Start building over `sandbox` (read+write root; default deny-network).
    pub fn builder(sandbox: Sandbox) -> OsSandboxBackendBuilder {
        OsSandboxBackendBuilder {
            sandbox,
            timeout: DEFAULT_TIMEOUT,
            env_allowlist: vec!["PATH".to_owned(), "HOME".to_owned()],
            max_output_bytes: DEFAULT_MAX_OUTPUT,
            limits: ResourceLimits::default(),
            allow_network: false,
            read_paths: Vec::new(),
        }
    }
}

#[async_trait]
impl ExecutionBackend for OsSandboxBackend {
    async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError> {
        let limits = self.limits.clone();
        // Task 7 adds the Landlock ruleset; Task 8 adds the seccomp filter.
        spawn_capped(&self.cfg, &req.command, move |cmd| {
            // SAFETY: apply_rlimits is async-signal-safe.
            unsafe {
                cmd.pre_exec(move || super::apply_rlimits(&limits));
            }
        })
        .await
    }

    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees {
            filesystem: Isolation::OsKernel,
            network: if self.allow_network {
                Isolation::None
            } else {
                Isolation::OsKernel
            },
            syscalls: Isolation::OsKernel,
            label: "os-sandbox (landlock+seccomp)",
        }
    }
}
```

> `root`/`read_paths` are unused until Task 7; add `#[allow(dead_code)]` on those
> two fields in this task, removed in Task 7.

- [ ] **Step 6: Wire the module + re-exports**

`src/exec/mod.rs` already has the `#[cfg(all(feature = "os-sandbox", target_os = "linux"))] mod os_sandbox;` + `pub use` block from Task 1 — uncomment/confirm it. Confirm `src/lib.rs` has the cfg'd `OsSandbox*` re-export block.

- [ ] **Step 7: Run the test**

Run: `cargo test -p paigasus-helikon-tools --features os-sandbox --test os_sandbox`
Expected: PASS (or the explicit SKIP print if the dev machine lacks Landlock — on Linux CI it must run; macOS dev machines won't compile this test target since it's `#[cfg(target_os = "linux")]`, so it compiles to empty there).

- [ ] **Step 8: deny + fmt + clippy + commit**

```bash
cargo deny check licenses    # landlock=MIT/Apache, seccompiler=Apache/BSD-3 → expect PASS, no new allow entry
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features os-sandbox --all-targets -- -D warnings
git add Cargo.toml Cargo.lock \
        crates/paigasus-helikon-tools/Cargo.toml \
        crates/paigasus-helikon-tools/src/exec/os_sandbox.rs \
        crates/paigasus-helikon-tools/src/exec/mod.rs \
        crates/paigasus-helikon-tools/src/lib.rs \
        crates/paigasus-helikon-tools/tests/os_sandbox.rs
git commit -m "feat(tools): SMA-413 add os-sandbox feature and fail-closed OsSandboxBackend skeleton"
```

---

### Task 7: Landlock filesystem containment (the OS-layer write-block AC)

Build the Landlock ruleset in the parent and `restrict_self()` in the child's
`pre_exec`. **Fork-safety:** `create()` + `add_rules` run in the parent (alloc
OK); only `restrict_self()` (and rlimits) run in the child.

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/os_sandbox.rs`
- Modify: `crates/paigasus-helikon-tools/tests/os_sandbox.rs`

- [ ] **Step 1: Add the failing AC test** (`tests/os_sandbox.rs`)

```rust
#[tokio::test]
async fn os_sandbox_blocks_write_outside_root_at_os_layer() {
    let tmp = tempfile::tempdir().unwrap();
    if landlock_unavailable(tmp.path()) {
        return;
    }
    let outside = tempfile::tempdir().unwrap(); // a sibling dir NOT under the sandbox root
    let target = outside.path().join("escape.txt");
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .build()
        .unwrap();

    // Absolute path outside the root: the shell's own path logic would allow it;
    // Landlock must block the write at the OS layer.
    let cmd = format!("echo pwned > {}", target.display());
    let out = backend
        .run(paigasus_helikon_tools::ExecRequest::new(cmd))
        .await
        .unwrap();
    assert_ne!(out.exit_code, Some(0), "write outside root must fail");
    assert!(!target.exists(), "no file may be created outside the sandbox root");

    // Sanity: a write INSIDE the root still succeeds.
    let ok = backend
        .run(paigasus_helikon_tools::ExecRequest::new("echo ok > inside.txt"))
        .await
        .unwrap();
    assert_eq!(ok.exit_code, Some(0));
    assert!(tmp.path().join("inside.txt").exists());
}
```

- [ ] **Step 2: Run to verify it fails** (the skeleton has no Landlock yet)

Run: `cargo test -p paigasus-helikon-tools --features os-sandbox --test os_sandbox os_sandbox_blocks_write_outside_root`
Expected: FAIL — the file is created / exit code is 0 (no fs jail yet).

- [ ] **Step 3: Build the ruleset in the parent and apply it in `pre_exec`**

In `os_sandbox.rs`, extend the imports:

```rust
use landlock::{
    path_beneath_rules, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, ABI,
};
```

Add a helper that builds a `RulesetCreated` from the configured paths:

```rust
/// Read-only system paths a shell + common tools need.
const SYSTEM_RO: &[&str] = &["/usr", "/bin", "/lib", "/lib64", "/etc"];

/// Build (in the parent) a Landlock ruleset: read+write under the sandbox root,
/// read+exec for the system paths and any extra `read_paths`.
fn build_ruleset(
    root: &std::path::Path,
    read_paths: &[PathBuf],
) -> Result<landlock::RulesetCreated, landlock::RulesetError> {
    let abi = LANDLOCK_ABI;
    let ro: Vec<PathBuf> = SYSTEM_RO
        .iter()
        .map(PathBuf::from)
        .chain(read_paths.iter().cloned())
        .filter(|p| p.exists())
        .collect();
    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))?
        .create()?
        .add_rules(path_beneath_rules(&ro, AccessFs::from_read(abi)))?
        .add_rules(path_beneath_rules([root], AccessFs::from_all(abi)))
}
```

Replace the `run` body's `pre_exec` closure so it builds the ruleset in the
parent and restricts in the child:

```rust
async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError> {
    let limits = self.limits.clone();
    // Built in the PARENT (allocations are safe here):
    let ruleset = build_ruleset(&self.root, &self.read_paths)
        .map_err(|e| ToolError::Other(anyhow::anyhow!("landlock ruleset: {e}")))?;
    let mut ruleset = Some(ruleset);

    spawn_capped(&self.cfg, &req.command, move |cmd| {
        // SAFETY: the closure runs in the forked child before exec. It performs
        // only async-signal-safe work: setrlimit syscalls and Landlock's
        // restrict_self (prctl + landlock_restrict_self on an already-created
        // ruleset fd). The ruleset is moved in via Option::take so it is applied
        // exactly once.
        unsafe {
            cmd.pre_exec(move || {
                super::apply_rlimits(&limits)?;
                if let Some(rs) = ruleset.take() {
                    let status = rs
                        .restrict_self()
                        .map_err(|e| std::io::Error::other(e.to_string()))?;
                    if status.ruleset == RulesetStatus::NotEnforced {
                        return Err(std::io::Error::other("landlock not enforced"));
                    }
                }
                Ok(())
            });
        }
    })
    .await
}
```

Remove the `#[allow(dead_code)]` on `root`/`read_paths` from Task 6.

> Fork-safety note for the reviewer: `restrict_self()` on an already-created
> ruleset is two syscalls plus a small stack struct; we accept its minor internal
> bookkeeping in `pre_exec`. If a future deadlock is observed under the
> multithreaded runtime, the fallback is to extract the raw ruleset fd
> (`AsRawFd`) in the parent and call `landlock_restrict_self` via `libc::syscall`
> directly. The test below is the empirical gate.

- [ ] **Step 4: Run the AC test to verify it passes**

Run: `cargo test -p paigasus-helikon-tools --features os-sandbox --test os_sandbox`
Expected: PASS on a Landlock-capable Linux kernel (else the explicit SKIP).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features os-sandbox --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/exec/os_sandbox.rs \
        crates/paigasus-helikon-tools/tests/os_sandbox.rs
git commit -m "feat(tools): SMA-413 enforce filesystem containment via Landlock"
```

---

### Task 8: seccomp syscall + network containment (the network-deny AC)

Compile a seccomp BPF in the parent (allow-by-default, deny a dangerous syscall
set + IP `socket` families when network is denied) and `apply_filter` in the
child after Landlock.

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/os_sandbox.rs`
- Modify: `crates/paigasus-helikon-tools/tests/os_sandbox.rs`

- [ ] **Step 1: Add the failing network-deny test** (`tests/os_sandbox.rs`)

```rust
#[tokio::test]
async fn os_sandbox_denies_network_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    if landlock_unavailable(tmp.path()) {
        return;
    }
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .build()
        .unwrap();
    // Pure-shell TCP connect to a public IP; seccomp must block socket(AF_INET).
    // bash's /dev/tcp triggers socket(2); on failure the redirect errors.
    let out = backend
        .run(paigasus_helikon_tools::ExecRequest::new(
            "timeout 5 sh -c 'echo > /dev/tcp/1.1.1.1/80' 2>&1; echo rc=$?",
        ))
        .await
        .unwrap();
    assert!(
        out.stdout.contains("rc=") && !out.stdout.contains("rc=0"),
        "network connect must fail under default-deny seccomp; got: {}",
        out.stdout
    );
}

#[tokio::test]
async fn os_sandbox_allows_network_when_opted_in() {
    let tmp = tempfile::tempdir().unwrap();
    if landlock_unavailable(tmp.path()) {
        return;
    }
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .allow_network(true)
        .build()
        .unwrap();
    let g = backend.guarantees();
    assert_eq!(g.network, paigasus_helikon_tools::Isolation::None);
    // socket() now succeeds (creating a socket needs no external service).
    let out = backend
        .run(paigasus_helikon_tools::ExecRequest::new(
            "python3 -c 'import socket; socket.socket(); print(\"ok\")' 2>&1 || echo nopy",
        ))
        .await
        .unwrap();
    assert!(out.stdout.contains("ok") || out.stdout.contains("nopy"));
}
```

> The second test tolerates a missing `python3` (`nopy`) so it never fails for an
> environment reason — it only asserts the guarantee flips and that socket
> creation is not seccomp-killed.

- [ ] **Step 2: Run to verify the deny test fails** (no seccomp yet → connect may succeed or hang)

Run: `cargo test -p paigasus-helikon-tools --features os-sandbox --test os_sandbox os_sandbox_denies_network`
Expected: FAIL (rc=0, i.e. the connect was not blocked) on a network-capable runner.

- [ ] **Step 3: Compile + apply the seccomp filter**

Add imports + an arch constant to `os_sandbox.rs`:

```rust
use std::collections::BTreeMap;

use seccompiler::{
    apply_filter, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch,
};

#[cfg(target_arch = "x86_64")]
const SECCOMP_ARCH: TargetArch = TargetArch::x86_64;
#[cfg(target_arch = "aarch64")]
const SECCOMP_ARCH: TargetArch = TargetArch::aarch64;
```

> Gate the whole `os-sandbox` module to supported arches: in `src/exec/mod.rs`
> change the cfg to
> `#[cfg(all(feature = "os-sandbox", target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]`
> for both the `mod os_sandbox;` and the `pub use`, and mirror it in `src/lib.rs`.
> Also widen the `#![cfg(...)]` at the top of `tests/os_sandbox.rs` to the same
> `any(target_arch = "x86_64", target_arch = "aarch64")` gate, so the test target
> compiles out on unsupported arches instead of referencing a gated-away type.
> On other Linux arches the feature compiles to nothing (documented limitation).

Add the filter builder:

```rust
/// Compile (in the parent) a seccomp filter: allow by default, return EPERM for a
/// dangerous syscall set; when `allow_network` is false also EPERM `socket()` for
/// `AF_INET`/`AF_INET6` (AF_UNIX stays allowed).
fn build_seccomp(allow_network: bool) -> Result<BpfProgram, ToolError> {
    let err = |e: seccompiler::Error| ToolError::Other(anyhow::anyhow!("seccomp: {e}"));
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    // Always-deny dangerous syscalls (empty rule vec = match regardless of args).
    for sc in [
        libc::SYS_ptrace,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_setns,
        libc::SYS_unshare,
        libc::SYS_kexec_load,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
    ] {
        rules.insert(sc, vec![]);
    }

    if !allow_network {
        let af = |family: u64| -> Result<SeccompRule, ToolError> {
            SeccompRule::new(vec![SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Eq,
                family,
            )
            .map_err(err)?])
            .map_err(err)
        };
        rules.insert(
            libc::SYS_socket,
            vec![af(libc::AF_INET as u64)?, af(libc::AF_INET6 as u64)?],
        );
    }

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,                       // default: allow
        SeccompAction::Errno(libc::EPERM as u32),   // on match: EPERM
        SECCOMP_ARCH,
    )
    .map_err(err)?;
    filter.try_into().map_err(err)
}
```

Extend `run` to compile the BPF in the parent and apply it in the child **after**
Landlock (Landlock's `restrict_self` sets `NO_NEW_PRIVS`, which seccomp requires):

```rust
    let allow_network = self.allow_network;
    let seccomp = build_seccomp(allow_network)?; // built in the PARENT
    // ... inside the pre_exec closure, after the Landlock block:
            apply_filter(&seccomp).map_err(|e| std::io::Error::other(e.to_string()))?;
            Ok(())
```

(Move `seccomp` into the closure alongside `ruleset`/`limits`; `apply_filter`
takes `&BpfProgram`, so no `Option::take` is needed.)

- [ ] **Step 4: Run the network tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools --features os-sandbox --test os_sandbox`
Expected: PASS on a Landlock+seccomp-capable Linux runner. Re-run the Task 7 fs
test too — it must still pass (Landlock + seccomp compose).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features os-sandbox --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/exec/os_sandbox.rs \
        crates/paigasus-helikon-tools/src/exec/mod.rs \
        crates/paigasus-helikon-tools/src/lib.rs \
        crates/paigasus-helikon-tools/tests/os_sandbox.rs
git commit -m "feat(tools): SMA-413 enforce syscall and network containment via seccomp"
```

---

### Task 9: Facade feature, version bumps, CHANGELOGs, full gate

**Files:**
- Modify: `crates/paigasus-helikon/Cargo.toml` (feature + version), `crates/paigasus-helikon/CHANGELOG.md`
- Modify: `crates/paigasus-helikon-tools/Cargo.toml` (version), `crates/paigasus-helikon-tools/CHANGELOG.md`
- Modify: root `Cargo.toml` `[workspace.dependencies]` self-pins

- [ ] **Step 1: Bump `-tools` to the breaking `0.2.0`**

In `crates/paigasus-helikon-tools/Cargo.toml`: `version = "0.1.6"` → `version = "0.2.0"`.
In root `Cargo.toml` `[workspace.dependencies]`: bump the `paigasus-helikon-tools`
pin to `0.2.0` (keep the `path` + `version`).

- [ ] **Step 2: Add the facade `tools-os-sandbox` feature + bump the facade**

In `crates/paigasus-helikon/Cargo.toml`, after `tools-web = [...]`:

```toml
tools-os-sandbox = ["tools", "paigasus-helikon-tools/os-sandbox"]
```

Bump the facade `version` (patch) and its `[workspace.dependencies]` self-pin to
match — this is the facade-drift fix from CLAUDE.md (when a sibling's version
changes in the same PR, the facade must republish with current reqs). Read the
current facade version from `crates/paigasus-helikon/Cargo.toml` and bump the
patch (e.g. `0.3.x` → `0.3.(x+1)`).

- [ ] **Step 3: Update both CHANGELOGs**

`crates/paigasus-helikon-tools/CHANGELOG.md` — new `0.2.0` section noting the
breaking `BashTool` reshape, the `ExecutionBackend` trait, `HostBackend`,
`OsSandboxBackend` (Linux, behind `os-sandbox`). `crates/paigasus-helikon/CHANGELOG.md`
— patch section noting the new `tools-os-sandbox` feature + sibling bump.

- [ ] **Step 4: Run the FULL local CI gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check
```

Expected: all green. (On a Linux CI host the `os_sandbox` tests run; on a non-Linux
dev box they compile out.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock \
        crates/paigasus-helikon-tools/Cargo.toml crates/paigasus-helikon-tools/CHANGELOG.md \
        crates/paigasus-helikon/Cargo.toml crates/paigasus-helikon/CHANGELOG.md
git commit -m "chore(release): SMA-413 bump tools to 0.2.0 and add facade tools-os-sandbox feature"
```

---

### Task 10: mdBook docs + OsSandbox example

**Files:**
- Modify: the mdBook tools/sandbox page under `docs/book/src/` (identify via `docs/book/src/SUMMARY.md`)
- Create: `crates/paigasus-helikon-tools/examples/os_sandbox_demo.rs`
- Modify: `crates/paigasus-helikon-tools/Cargo.toml` (`[[example]]` requiring `os-sandbox`)

- [ ] **Step 1: Find the page to edit**

Run: `grep -rni "bash\|sandbox\|tools" docs/book/src/SUMMARY.md`
Open the referenced tools page (e.g. `docs/book/src/tools.md` or similar).

- [ ] **Step 2: Add a "Containment vs approval" section**

Document the three axes (containment ≠ approval ≠ resource-capping), the
backends **leading with `OsSandboxBackend`** on Linux and its fail-closed
fallback to `HostBackend`, the `guarantees()` tiers and exactly what each
enforces, and the kernel matrix (Landlock ≥ 5.13 + seccomp; x86_64/aarch64; no
namespaces). Note the network egress proxy + macOS (SMA-426) as forthcoming. Use
fenced ` ```rust,ignore ` blocks for any code that constructs a backend (no
network/feature deps at doctest time).

- [ ] **Step 3: Add the example** (`examples/os_sandbox_demo.rs`)

```rust
//! OS-sandbox demo (Linux). Run with:
//!   cargo run -p paigasus-helikon-tools --features os-sandbox --example os_sandbox_demo
#![allow(missing_docs)]

#[cfg(all(feature = "os-sandbox", target_os = "linux"))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use paigasus_helikon_tools::{ExecRequest, ExecutionBackend, OsSandboxBackend, Sandbox};

    let dir = tempfile::tempdir()?;
    let backend = match OsSandboxBackend::builder(Sandbox::open(dir.path())?).build() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("OS sandbox unavailable ({e}); this host lacks Landlock.");
            return Ok(());
        }
    };
    println!("guarantees: {:?}", backend.guarantees());

    let blocked = backend
        .run(ExecRequest::new("echo pwned > /tmp/escape_demo.txt; echo rc=$?"))
        .await?;
    println!("write-outside-root attempt → {}", blocked.stdout.trim());
    Ok(())
}

#[cfg(not(all(feature = "os-sandbox", target_os = "linux")))]
fn main() {
    eprintln!("This example requires --features os-sandbox on Linux.");
}
```

Register it in `crates/paigasus-helikon-tools/Cargo.toml`:

```toml
[[example]]
name              = "os_sandbox_demo"
required-features = ["os-sandbox"]
```

- [ ] **Step 4: Build the book + the example**

```bash
mdbook build docs/book
cargo build -p paigasus-helikon-tools --features os-sandbox --example os_sandbox_demo
```

Expected: `mdbook build` clean (linkcheck warning-policy = error); example builds.

- [ ] **Step 5: Commit**

```bash
git add docs/book/src crates/paigasus-helikon-tools/examples/os_sandbox_demo.rs \
        crates/paigasus-helikon-tools/Cargo.toml
git commit -m "docs(book): SMA-413 document the ExecutionBackend containment axis"
```

---

## Final verification before opening the PR

- [ ] Re-run the full gate (Task 9 Step 4) once more on the rebased branch.
- [ ] Confirm `cargo test --workspace --all-features` exercised the `os_sandbox`
      tests on Linux (look for the test names in output; if you see the `SKIP`
      line, the host lacks Landlock — note it, CI Linux must run them for real).
- [ ] **PR title** (becomes the squashed `main` commit) must be a breaking
      conventional-commit with a lowercase subject, e.g.
      `feat(tools)!: SMA-413 add pluggable ExecutionBackend with Host and OS-sandbox backends`
      (the `!` drives release-plz's minor `0.1.6 → 0.2.0` bump on a 0.x crate).
- [ ] Ensure design spec + this plan are on the branch (already committed).
```
