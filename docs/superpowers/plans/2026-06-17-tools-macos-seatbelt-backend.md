# macOS Seatbelt `ExecutionBackend` (SMA-426) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a macOS Seatbelt `OsSandboxBackend` (write-focused containment via `sandbox-exec`) behind the existing `ExecutionBackend` trait, with the same type names as the Linux backend, so `BashTool`/agent code is unchanged across platforms.

**Architecture:** A new `target_os = "macos"`-gated module reuses the shared `spawn_capped` helper (generalized with an argv prefix) to run `/usr/bin/sandbox-exec -D ROOT=<root> -p <profile> sh -c <command>`. The SBPL profile is `(deny default)` + read-all + write-only-root + an all-or-nothing `(allow network*)`. Fail-closed `build()` probes a real shell. Zero new crate dependencies.

**Tech Stack:** Rust, `std::process` / `tokio::process`, macOS Seatbelt (`sandbox-exec`), `libc` (`setrlimit`), `async-trait`, `thiserror`.

**Spec:** `docs/superpowers/specs/2026-06-17-tools-macos-seatbelt-backend-design.md` (read it first — esp. §3 spike evidence and §13 review dispositions).

**Working notes for the implementer:**
- The dev machine is **macOS arm64**, so all macOS tests run **natively** (`cargo test -p paigasus-helikon-tools --features os-sandbox`).
- The **Linux** backend's code (`src/exec/os_sandbox.rs`, `tests/os_sandbox.rs`) cannot run here. After touching shared code, cross-check Linux compiles with:
  `cargo check -p paigasus-helikon-tools --target x86_64-unknown-linux-gnu --features os-sandbox --lib`
  (add the target once: `rustup target add x86_64-unknown-linux-gnu`). `--lib` only — `--tests`/`--examples` fail to cross-build on macOS (ring C build in unrelated dev-deps).
- Commits are signed via a 1Password SSH key; if a commit fails with "failed to fill whole buffer", ask the user to unlock the vault, then retry.
- Run `cargo fmt --all` + `cargo clippy -p paigasus-helikon-tools --all-features --all-targets -- -D warnings` before each commit (pre-commit hook is a no-op).

---

## File structure

| File | Responsibility | Action |
|------|----------------|--------|
| `crates/paigasus-helikon-tools/src/exec/mod.rs` | trait + `spawn_capped` + cfg wiring | Modify — generalize `build_command`/`spawn_capped`; add macOS cfg branch |
| `crates/paigasus-helikon-tools/src/exec/host.rs` | host backend | Modify — update `spawn_capped` call site |
| `crates/paigasus-helikon-tools/src/exec/os_sandbox.rs` | Linux backend | Modify — update `spawn_capped` call site (Linux-only) |
| `crates/paigasus-helikon-tools/src/exec/os_sandbox_seatbelt.rs` | **macOS Seatbelt backend** | **Create** |
| `crates/paigasus-helikon-tools/src/lib.rs` | crate re-exports + crate doc | Modify — macOS re-export + doc |
| `crates/paigasus-helikon-tools/tests/os_sandbox_seatbelt.rs` | macOS AC tests | **Create** |
| `crates/paigasus-helikon-tools/tests/os_sandbox.rs` | Linux AC tests | Modify — `HELIKON_REQUIRE_SANDBOX` retrofit |
| `crates/paigasus-helikon-tools/examples/os_sandbox_demo.rs` | demo | Modify — enable on macOS |
| `.github/workflows/ci.yml` | CI | Modify — `HELIKON_REQUIRE_SANDBOX=1` on the test job |
| `.github/rulesets/main-protection-checks.json` | required checks | Modify — add macOS test context |
| `CONTRIBUTING.md`, `CLAUDE.md` | required-check docs | Modify — list the new required context |
| `crates/paigasus-helikon-tools/README.md`, `CHANGELOG.md` | crate docs | Modify |
| `crates/paigasus-helikon/README.md` | facade README | Modify — os-sandbox covers macOS |
| `docs/book/src/concepts/tools.md` | mdBook | Modify — backend/platform matrix |

---

## Task 1: Generalize `spawn_capped` with an argv prefix (refactor, no behaviour change)

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs`
- Modify: `crates/paigasus-helikon-tools/src/exec/host.rs:114`
- Modify: `crates/paigasus-helikon-tools/src/exec/os_sandbox.rs:248` (Linux-only)

- [ ] **Step 1: Confirm the call sites.**

Run: `grep -rn "spawn_capped" crates/paigasus-helikon-tools/src`
Expected: the definition in `exec/mod.rs`, one call in `host.rs`, one call in `os_sandbox.rs`. (If more appear, update them all the same way.)

- [ ] **Step 2: Add the `OsString` import and change `build_command` + `spawn_capped` in `exec/mod.rs`.**

Add near the top imports (alongside `use std::path::PathBuf;`):

```rust
use std::ffi::OsString;
```

Replace the `build_command` function with:

```rust
/// Build the platform shell command without yet setting cwd/env. `prefix`, when
/// non-empty, is prepended as `program [args...]` before `sh -c <command>` — used
/// by the macOS Seatbelt backend to wrap the shell in `sandbox-exec`. Empty for
/// the host and Linux backends.
fn build_command(prefix: &[OsString], command: &str) -> tokio::process::Command {
    #[cfg(unix)]
    {
        match prefix.split_first() {
            Some((program, rest)) => {
                let mut c = tokio::process::Command::new(program);
                c.args(rest).arg("sh").arg("-c").arg(command);
                c
            }
            None => {
                let mut c = tokio::process::Command::new("sh");
                c.arg("-c").arg(command);
                c
            }
        }
    }
    #[cfg(windows)]
    {
        // `prefix` is always empty on Windows; bind it to avoid `unused_variables`
        // under `-D warnings` (clippy runs on ubuntu; Windows is signal-only).
        let _ = prefix;
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    }
}
```

Change the `spawn_capped` signature and its first line:

```rust
pub(crate) async fn spawn_capped(
    cfg: &ExecConfig,
    prefix: &[OsString],
    command: &str,
    configure_child: impl FnOnce(&mut tokio::process::Command),
) -> Result<ExecOutput, ToolError> {
    let mut cmd = build_command(prefix, command);
    // ...rest unchanged...
```

- [ ] **Step 3: Update the host call site (`host.rs:114`).**

```rust
spawn_capped(&self.cfg, &[], &req.command, move |_cmd| {
```

(The empty slice infers as `&[OsString]` from the parameter type.)

- [ ] **Step 4: Update the Linux call site (`os_sandbox.rs`, the `spawn_capped(&self.cfg, &req.command, move |cmd| {` line).**

```rust
spawn_capped(&self.cfg, &[], &req.command, move |cmd| {
```

- [ ] **Step 5: Build + run the existing suite (regression check on macOS).**

Run: `cargo test -p paigasus-helikon-tools`
Expected: PASS (host/bash/sandbox tests unaffected — pure refactor).

- [ ] **Step 6: Cross-check the Linux backend still compiles.**

Run: `cargo check -p paigasus-helikon-tools --target x86_64-unknown-linux-gnu --features os-sandbox --lib`
Expected: compiles clean (the Linux `os_sandbox.rs` call-site edit is verified here).

- [ ] **Step 7: fmt + clippy + commit.**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/exec/mod.rs crates/paigasus-helikon-tools/src/exec/host.rs crates/paigasus-helikon-tools/src/exec/os_sandbox.rs
git commit -m "refactor(tools): SMA-426 thread an argv prefix through spawn_capped"
```

---

## Task 2: The macOS Seatbelt backend module

**Files:**
- Create: `crates/paigasus-helikon-tools/src/exec/os_sandbox_seatbelt.rs`
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs` (cfg wiring)
- Modify: `crates/paigasus-helikon-tools/src/lib.rs` (re-export)
- Test: `crates/paigasus-helikon-tools/tests/os_sandbox_seatbelt.rs`

- [ ] **Step 1: Write the failing test (guarantees + builds).**

Create `crates/paigasus-helikon-tools/tests/os_sandbox_seatbelt.rs`:

```rust
#![allow(missing_docs)]
#![cfg(all(feature = "os-sandbox", target_os = "macos"))]

// `ExecutionBackend` is not imported: `build()` returns `Arc<dyn ExecutionBackend>`,
// so trait methods resolve on the trait object without the trait in scope.
use paigasus_helikon_tools::{Isolation, OsSandboxBackend, Sandbox};

/// Skip (loudly) when Seatbelt can't be established here — UNLESS
/// `HELIKON_REQUIRE_SANDBOX=1`, in which case an unavailable sandbox is a hard
/// failure, so a CI runner that stops enforcing turns the build red, never green.
/// Returns true if the caller should `return`.
fn seatbelt_unavailable(root: &std::path::Path) -> bool {
    if OsSandboxBackend::builder(Sandbox::open(root).unwrap())
        .build()
        .is_ok()
    {
        return false;
    }
    if std::env::var("HELIKON_REQUIRE_SANDBOX").as_deref() == Ok("1") {
        panic!("HELIKON_REQUIRE_SANDBOX=1 but Seatbelt could not be established on this host");
    }
    eprintln!("SKIP: Seatbelt unavailable on this host; os-sandbox AC not exercised");
    true
}

#[tokio::test]
async fn os_sandbox_builds_and_reports_guarantees() {
    let tmp = tempfile::tempdir().unwrap();
    if seatbelt_unavailable(tmp.path()) {
        return;
    }
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .build()
        .expect("Seatbelt available");
    let g = backend.guarantees();
    assert_eq!(g.filesystem, Isolation::OsKernel);
    assert_eq!(g.syscalls, Isolation::None);
    assert_eq!(g.network, Isolation::OsKernel); // default deny
    assert_eq!(g.label, "os-sandbox (seatbelt)");
}
```

- [ ] **Step 2: Run it — expect a compile failure.**

Run: `cargo test -p paigasus-helikon-tools --features os-sandbox --test os_sandbox_seatbelt`
Expected: FAIL — `OsSandboxBackend` not found for macOS (type doesn't exist yet).

- [ ] **Step 3: Create the backend module `src/exec/os_sandbox_seatbelt.rs`.**

```rust
//! [`OsSandboxBackend`] — OS-enforced Bash containment on macOS via Seatbelt
//! (the `sandbox-exec` profile sandbox). **Write-focused:** deny-by-default,
//! reads allowed (dyld needs the cryptex shared cache), read+write only within
//! the sandbox root, all-or-nothing network. Fail-closed: `build()` errors if the
//! profile cannot be applied.
//!
//! **Honesty (see the design spec §2.2/§2.3):** `guarantees().filesystem` is
//! `OsKernel` but enforces *write* containment only — reads are unrestricted,
//! weaker than the Linux backend which restricts reads too. `syscalls` is `None`:
//! Seatbelt is an operation-level MAC, not a syscall filter, and the profile
//! allows `process*`. Broad `mach-lookup`/`process*` are an accepted v1 residual
//! risk; deny-network also blocks `AF_UNIX` (stricter than Linux). `sandbox-exec`
//! is Apple-deprecated but ships on every macOS; the trait seam keeps an FFI swap
//! open later.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::ToolError;

use super::{
    spawn_capped, ExecConfig, ExecOutput, ExecRequest, ExecutionBackend, Isolation, ResourceLimits,
    SandboxGuarantees, DEFAULT_MAX_OUTPUT, DEFAULT_TIMEOUT,
};
use crate::sandbox::Sandbox;

/// Absolute path to the macOS Seatbelt wrapper (ships with the OS). Absolute so a
/// scrubbed `PATH` cannot hide it.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Construction failures for [`OsSandboxBackend`]. Distinct from
/// `ToolError::Denied` (an in-`run` refusal) and from `SandboxError`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OsSandboxError {
    /// Seatbelt could not be established: `sandbox-exec` missing, the profile was
    /// rejected, or the probe showed the sandbox did not contain a write.
    #[error("OS sandbox unavailable: {0}")]
    Unsupported(String),
}

/// Builder for [`OsSandboxBackend`]. Method-for-method parity with the Linux
/// builder so consumer code is identical across platforms.
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
    /// Override resource limits applied inside the jail (via `setrlimit`).
    pub fn rlimits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }
    /// Allow outbound network (default: deny all egress, including `AF_UNIX`).
    pub fn allow_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }
    /// **No-op on macOS.** The write-focused Seatbelt posture allows all reads, so
    /// extra read-only exceptions have no effect. Kept for API parity with the
    /// Linux backend so consumer code is identical across platforms.
    pub fn read_paths<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.read_paths = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Finish building. **Fail-closed:** returns `Err` if Seatbelt cannot be
    /// established (no `sandbox-exec`, profile rejected, or the probe shows the
    /// sandbox did not contain a write inside the root).
    pub fn build(self) -> Result<Arc<dyn ExecutionBackend>, OsSandboxError> {
        // `read_paths` is intentionally unused on macOS (see the builder method).
        let _ = self.read_paths;
        let root = self.sandbox.root().to_path_buf(); // already canonical (Sandbox::open)
        let profile = build_profile(self.allow_network);
        probe(&root, &profile)?;
        let prefix = wrapper_prefix(&root, &profile);
        Ok(Arc::new(OsSandboxBackend {
            cfg: ExecConfig {
                cwd: root,
                env_allowlist: self.env_allowlist,
                timeout: self.timeout,
                max_output_bytes: self.max_output_bytes,
            },
            limits: self.limits,
            allow_network: self.allow_network,
            prefix,
        }))
    }
}

/// Build the write-focused SBPL profile. `ROOT` is supplied separately via
/// `sandbox-exec -D ROOT=…` and referenced as `(param "ROOT")`, so no path text is
/// spliced into the profile (no SBPL-injection surface).
fn build_profile(allow_network: bool) -> String {
    let mut p = String::from(
        r#"(version 1)
(deny default)
(allow process*)
(allow sysctl-read)
(allow mach-lookup)
(allow file-read*)
(allow file-write*
  (subpath (param "ROOT"))
  (literal "/dev/null") (literal "/dev/stdout") (literal "/dev/stderr")
  (literal "/dev/tty") (literal "/dev/dtracehelper"))
"#,
    );
    if allow_network {
        p.push_str("(allow network*)\n");
    }
    p
}

/// The argv that must precede `sh -c <command>`: the Seatbelt wrapper with `ROOT`
/// bound and the profile inline.
fn wrapper_prefix(root: &Path, profile: &str) -> Vec<OsString> {
    vec![
        OsString::from(SANDBOX_EXEC),
        OsString::from("-D"),
        OsString::from(format!("ROOT={}", root.display())),
        OsString::from("-p"),
        OsString::from(profile),
    ]
}

/// Fail-closed probe: run a real shell under the profile that writes a file inside
/// the root, then removes it. Validates `sandbox-exec` exists, the profile
/// compiles, the shell starts under it, and the root write-rule actually permits
/// writes (catching a canonicalization/rule bug at construction). The marker path
/// is passed as `$1` (argv), never interpolated into the script. The
/// write-OUTSIDE-root denial is covered by the AC test (a required CI check).
fn probe(root: &Path, profile: &str) -> Result<(), OsSandboxError> {
    let marker = root.join(".helikon-seatbelt-probe");
    let out = std::process::Command::new(SANDBOX_EXEC)
        .arg("-D")
        .arg(format!("ROOT={}", root.display()))
        .arg("-p")
        .arg(profile)
        .arg("/bin/sh")
        .arg("-c")
        .arg(r#"echo ok > "$1" && rm -f "$1""#)
        .arg("seatbelt-probe") // $0
        .arg(&marker) // $1 — no shell interpolation
        .output()
        .map_err(|e| OsSandboxError::Unsupported(format!("cannot run {SANDBOX_EXEC}: {e}")))?;
    let _ = std::fs::remove_file(&marker);
    if !out.status.success() {
        return Err(OsSandboxError::Unsupported(format!(
            "Seatbelt probe failed (status {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// OS-enforced execution backend (macOS, Seatbelt). See module docs.
pub struct OsSandboxBackend {
    cfg: ExecConfig,
    limits: ResourceLimits,
    allow_network: bool,
    prefix: Vec<OsString>,
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
        // The jail is applied by `sandbox-exec` (the prefix); the only `pre_exec`
        // work is the shared, async-signal-safe `setrlimit`s.
        spawn_capped(&self.cfg, &self.prefix, &req.command, move |_cmd| {
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
            filesystem: Isolation::OsKernel, // write-containment only (see module docs)
            network: if self.allow_network {
                Isolation::None
            } else {
                Isolation::OsKernel
            },
            syscalls: Isolation::None,
            label: "os-sandbox (seatbelt)",
        }
    }
}
```

- [ ] **Step 4: Wire the module into `src/exec/mod.rs`.**

Immediately after the existing Linux `pub use os_sandbox::{…}` block (around line 25), add:

```rust
#[cfg(all(feature = "os-sandbox", target_os = "macos"))]
mod os_sandbox_seatbelt;
#[cfg(all(feature = "os-sandbox", target_os = "macos"))]
pub use os_sandbox_seatbelt::{OsSandboxBackend, OsSandboxBackendBuilder, OsSandboxError};
```

- [ ] **Step 5: Re-export from `src/lib.rs`.**

Immediately after the existing Linux `pub use exec::{OsSandboxBackend, …};` block (around line 45), add:

```rust
#[cfg(all(feature = "os-sandbox", target_os = "macos"))]
pub use exec::{OsSandboxBackend, OsSandboxBackendBuilder, OsSandboxError};
```

- [ ] **Step 6: Run the test — expect PASS.**

Run: `cargo test -p paigasus-helikon-tools --features os-sandbox --test os_sandbox_seatbelt`
Expected: `os_sandbox_builds_and_reports_guarantees ... ok` (the probe runs a real `sandbox-exec` here).

- [ ] **Step 7: fmt + clippy + Linux cross-check + commit.**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --all-features --all-targets -- -D warnings
cargo check -p paigasus-helikon-tools --target x86_64-unknown-linux-gnu --features os-sandbox --lib
git add crates/paigasus-helikon-tools/src/exec/os_sandbox_seatbelt.rs crates/paigasus-helikon-tools/src/exec/mod.rs crates/paigasus-helikon-tools/src/lib.rs crates/paigasus-helikon-tools/tests/os_sandbox_seatbelt.rs
git commit -m "feat(tools): SMA-426 add macOS Seatbelt OsSandboxBackend"
```

---

## Task 3: Filesystem AC test — write blocked outside root, allowed inside

**Files:**
- Modify: `crates/paigasus-helikon-tools/tests/os_sandbox_seatbelt.rs`

- [ ] **Step 1: Add the failing test.**

Append to `tests/os_sandbox_seatbelt.rs`:

```rust
#[tokio::test]
async fn os_sandbox_blocks_write_outside_root_at_os_layer() {
    let tmp = tempfile::tempdir().unwrap();
    if seatbelt_unavailable(tmp.path()) {
        return;
    }
    let outside = tempfile::tempdir().unwrap(); // sibling dir NOT under the sandbox root
    let target = outside.path().join("escape.txt");
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .build()
        .unwrap();

    // Absolute path outside the root: the shell's own logic would allow it;
    // Seatbelt must block the write at the OS layer.
    let cmd = format!("echo pwned > {}", target.display());
    let out = backend
        .run(paigasus_helikon_tools::ExecRequest::new(cmd))
        .await
        .unwrap();
    assert_ne!(
        out.exit_code,
        Some(0),
        "write outside root must fail; stderr={}",
        out.stderr
    );
    assert!(
        !target.exists(),
        "no file may be created outside the sandbox root"
    );

    // A write INSIDE the root (cwd = root) succeeds.
    let ok = backend
        .run(paigasus_helikon_tools::ExecRequest::new(
            "echo ok > inside.txt",
        ))
        .await
        .unwrap();
    assert_eq!(
        ok.exit_code,
        Some(0),
        "write inside root must succeed; stderr={}",
        ok.stderr
    );
    assert!(tmp.path().join("inside.txt").exists());
}
```

- [ ] **Step 2: Run it — expect PASS.**

Run: `cargo test -p paigasus-helikon-tools --features os-sandbox --test os_sandbox_seatbelt os_sandbox_blocks_write_outside_root_at_os_layer`
Expected: PASS (the §3 spike confirmed this behaviour). If it FAILS with the inside-write denied, the root rule/canonicalization is wrong — revisit `build_profile`/`wrapper_prefix` (they must use `sandbox.root()`, which is canonical).

- [ ] **Step 3: Commit.**

```bash
cargo fmt --all
git add crates/paigasus-helikon-tools/tests/os_sandbox_seatbelt.rs
git commit -m "test(tools): SMA-426 assert Seatbelt blocks writes outside the sandbox root"
```

---

## Task 4: Network AC tests — deny by default, allow when opted in

**Files:**
- Modify: `crates/paigasus-helikon-tools/tests/os_sandbox_seatbelt.rs`

- [ ] **Step 1: Add the two failing tests.**

Append to `tests/os_sandbox_seatbelt.rs`. The probe prints **our own markers** (not localized OS strings): `EPERM` when Seatbelt blocks `connect`, `REFUSED` when it reaches the stack (closed local port). Validated in the §3 spike.

```rust
/// A `python3` probe: connect to a closed local port. Prints EPERM if the sandbox
/// blocks connect(2), REFUSED if it reached the network stack. `2>&1` folds the
/// (unused) stderr into stdout. python3 ships on the GitHub macOS runner.
const NET_PROBE: &str = r#"python3 -c 'import socket
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.settimeout(2)
try:
    s.connect(("127.0.0.1", 9)); print("CONNECTED")
except PermissionError: print("EPERM")
except ConnectionRefusedError: print("REFUSED")
except Exception as e: print("OTHER", type(e).__name__)' 2>&1"#;

#[tokio::test]
async fn os_sandbox_denies_network_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    if seatbelt_unavailable(tmp.path()) {
        return;
    }
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .build()
        .unwrap();
    let out = backend
        .run(paigasus_helikon_tools::ExecRequest::new(NET_PROBE))
        .await
        .unwrap();
    assert!(
        out.stdout.contains("EPERM"),
        "connect must be sandbox-denied (EPERM) under default-deny; got: {}",
        out.stdout
    );
}

#[tokio::test]
async fn os_sandbox_allows_network_when_opted_in() {
    let tmp = tempfile::tempdir().unwrap();
    if seatbelt_unavailable(tmp.path()) {
        return;
    }
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .allow_network(true)
        .build()
        .unwrap();
    assert_eq!(backend.guarantees().network, Isolation::None);
    let out = backend
        .run(paigasus_helikon_tools::ExecRequest::new(NET_PROBE))
        .await
        .unwrap();
    // Reached the stack → closed port refuses. Pairs with the deny test so a
    // regression in either direction fails one of the two.
    assert!(
        out.stdout.contains("REFUSED"),
        "connect must reach the stack (REFUSED) under allow_network; got: {}",
        out.stdout
    );
}
```

- [ ] **Step 2: Run them — expect PASS.**

Run: `cargo test -p paigasus-helikon-tools --features os-sandbox --test os_sandbox_seatbelt`
Expected: all four tests PASS.

- [ ] **Step 3: Commit.**

```bash
cargo fmt --all
git add crates/paigasus-helikon-tools/tests/os_sandbox_seatbelt.rs
git commit -m "test(tools): SMA-426 assert Seatbelt network deny/allow toggle"
```

---

## Task 5: Enable the demo example on macOS

**Files:**
- Modify: `crates/paigasus-helikon-tools/examples/os_sandbox_demo.rs`

- [ ] **Step 1: Broaden both cfg gates to include macOS.**

Replace the `#[cfg(all(feature = "os-sandbox", target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]` on the async `main` with:

```rust
#[cfg(all(
    feature = "os-sandbox",
    any(
        all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")),
        target_os = "macos"
    )
))]
```

Replace the mirrored `#[cfg(not(all(…)))]` on the fallback `main` with the negation:

```rust
#[cfg(not(all(
    feature = "os-sandbox",
    any(
        all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")),
        target_os = "macos"
    )
)))]
```

Update the fallback `eprintln!` text to: `"This example requires --features os-sandbox on Linux (x86_64/aarch64) or macOS."`

- [ ] **Step 2: Run the demo on macOS.**

Run: `cargo run -p paigasus-helikon-tools --features os-sandbox --example os_sandbox_demo`
Expected: prints `guarantees: …` with `label: "os-sandbox (seatbelt)"`, then a blocked-write line (the `/tmp/escape_demo.txt` write is denied — outside the root).

- [ ] **Step 3: Linux cross-check + commit.**

```bash
cargo fmt --all
cargo check -p paigasus-helikon-tools --target x86_64-unknown-linux-gnu --features os-sandbox --lib
git add crates/paigasus-helikon-tools/examples/os_sandbox_demo.rs
git commit -m "docs(tools): SMA-426 enable the os_sandbox_demo example on macOS"
```

---

## Task 6: CI honesty wiring (REQUIRE_SANDBOX, macOS required check, Linux retrofit)

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/rulesets/main-protection-checks.json`
- Modify: `crates/paigasus-helikon-tools/tests/os_sandbox.rs` (Linux guard retrofit)
- Modify: `CONTRIBUTING.md`, `CLAUDE.md` (required-check lists)

- [ ] **Step 1: Set `HELIKON_REQUIRE_SANDBOX=1` on the test job.**

In `.github/workflows/ci.yml`, in the `test:` job, add a job-level `env` block between `runs-on: ${{ matrix.os }}` and `steps:`:

```yaml
    runs-on: ${{ matrix.os }}
    env:
      # Turn a silently-unenforcing sandbox into a hard failure (never green-by-skip).
      HELIKON_REQUIRE_SANDBOX: 1
    steps:
```

- [ ] **Step 2: Retrofit the Linux test guard to honour it.**

In `crates/paigasus-helikon-tools/tests/os_sandbox.rs`, change `landlock_unavailable` so that an unavailable sandbox panics under `HELIKON_REQUIRE_SANDBOX=1` instead of skipping:

```rust
fn landlock_unavailable(tmp: &std::path::Path) -> bool {
    if OsSandboxBackend::builder(Sandbox::open(tmp).unwrap())
        .build()
        .is_ok()
    {
        return false;
    }
    if std::env::var("HELIKON_REQUIRE_SANDBOX").as_deref() == Ok("1") {
        panic!("HELIKON_REQUIRE_SANDBOX=1 but Landlock could not be established on this host");
    }
    eprintln!("SKIP: Landlock unavailable on this kernel; os-sandbox AC not exercised");
    true
}
```

(Note the original returns early via `.is_err()`; this rewrite keeps identical semantics plus the hard-fail branch.)

- [ ] **Step 3: Add the macOS test as a required status check.**

In `.github/rulesets/main-protection-checks.json`, add to the `required_status_checks` array (after the `test (ubuntu-latest, stable)` entry):

```json
          { "context": "test (macos-latest, stable)" },
```

- [ ] **Step 4: Update the required-check prose.**

In `CONTRIBUTING.md` and `CLAUDE.md`, find the sentence listing required contexts (search for `test (ubuntu-latest, stable)`) and add `test (macos-latest, stable)` to that list, with a short note that the macOS job is required because it is the only gate that compiles + runs the Seatbelt backend.

- [ ] **Step 5: Verify the YAML + JSON parse and fmt.**

Run: `python3 -c "import json; json.load(open('.github/rulesets/main-protection-checks.json'))" && echo JSON_OK`
Expected: `JSON_OK`.
Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 6: Commit.**

```bash
git add .github/workflows/ci.yml .github/rulesets/main-protection-checks.json crates/paigasus-helikon-tools/tests/os_sandbox.rs CONTRIBUTING.md CLAUDE.md
git commit -m "ci: SMA-426 require macOS test + HELIKON_REQUIRE_SANDBOX hard-fail"
```

> **Post-merge verification (do during the PR, see Task 8):** if `test (ubuntu-latest, stable)` goes red because the runner lacks Landlock (i.e. the retrofit surfaced a real skip), scope `HELIKON_REQUIRE_SANDBOX` to the macOS matrix entry instead of the whole job, and re-push. The ubuntu Landlock tests are expected to enforce on `ubuntu-latest`; a red here is a genuine signal, not noise.

---

## Task 7: Docs — crate README, CHANGELOG, facade README, crate doc, mdBook

**Files:**
- Modify: `crates/paigasus-helikon-tools/README.md`
- Modify: `crates/paigasus-helikon-tools/CHANGELOG.md`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs` (crate-level doc)
- Modify: `crates/paigasus-helikon/README.md` (facade)
- Modify: `docs/book/src/concepts/tools.md`

- [ ] **Step 1: Crate README.**

In `crates/paigasus-helikon-tools/README.md`:
- Line ~3: change "OS-enforced Bash containment via Landlock + seccomp behind the `os-sandbox` feature (Linux)." → "OS-enforced Bash containment behind the `os-sandbox` feature (Linux: Landlock + seccomp; macOS: Seatbelt)."
- Line ~7: change "`OsSandboxBackend` (Linux, feature `os-sandbox`) for OS-kernel-enforced containment via Landlock (filesystem) and seccomp-bpf (syscalls and network)." → "`OsSandboxBackend` (feature `os-sandbox`) for OS-kernel-enforced containment — Linux via Landlock (filesystem) + seccomp-bpf (syscalls and network) with read+write restriction; macOS via Seatbelt (`sandbox-exec`) with **write-only** containment (reads unrestricted) and an all-or-nothing network toggle."
- Line ~18 install comment: "with OS-enforced Bash containment (Linux: Landlock + seccomp):" → "with OS-enforced Bash containment (Linux: Landlock + seccomp; macOS: Seatbelt):"
- Line ~43 examples: change "`os_sandbox_demo` (OS-sandbox containment demo, Linux only, requires `--features os-sandbox`)." → "`os_sandbox_demo` (OS-sandbox containment demo, Linux + macOS, requires `--features os-sandbox`)."

- [ ] **Step 2: Crate-level doc in `src/lib.rs`.**

In the `//!` block (around lines 18-22), change "The `OsSandboxBackend` (Linux, behind the `os-sandbox` feature) instead enforces filesystem and syscall containment at the OS layer." → "The `OsSandboxBackend` (behind the `os-sandbox` feature) instead enforces containment at the OS layer — on Linux via Landlock + seccomp (filesystem reads/writes + syscalls + network), on macOS via Seatbelt (`sandbox-exec`; write-only filesystem containment + an all-or-nothing network toggle)."

- [ ] **Step 3: CHANGELOG.**

In `crates/paigasus-helikon-tools/CHANGELOG.md`, under `## [Unreleased]`, add:

```markdown
### Added

- *(tools)* SMA-426 add a macOS Seatbelt `OsSandboxBackend` (via `sandbox-exec`) behind the existing `os-sandbox` feature — write-focused OS containment (deny-by-default, read+write only within the sandbox root, all-or-nothing network), same API as the Linux backend.
```

- [ ] **Step 4: Facade README.**

In `crates/paigasus-helikon/README.md`, search for `os-sandbox` / `tools-os-sandbox` and update any "Linux" qualifier so the os-sandbox containment is described as "Linux (Landlock + seccomp) and macOS (Seatbelt)". (If the facade README does not mention os-sandbox specifically, no change is needed — note that in the commit.)

- [ ] **Step 5: mdBook tools page.**

Read `docs/book/src/concepts/tools.md`, find the `ExecutionBackend` / backend-matrix / `os-sandbox` section, and integrate these facts (match the page's existing prose style):
- macOS now has an `OsSandboxBackend` via **Seatbelt** (`sandbox-exec`), behind the same `os-sandbox` feature.
- Posture matrix: **Linux** = Landlock (fs read+write) + seccomp (syscalls + network) → `filesystem`/`syscalls`/`network` all `OsKernel`. **macOS** = Seatbelt → `filesystem: OsKernel` (**write-only**, reads unrestricted), `network: OsKernel`/`None`, `syscalls: None`.
- Be explicit that macOS `OsKernel` on the filesystem axis is **weaker** than Linux (write containment only) and that deny-network also blocks `AF_UNIX`; note `sandbox-exec` is Apple-deprecated but ships on every macOS.

- [ ] **Step 6: Build the book + verify links.**

Run: `mdbook build docs/book`
Expected: completes with no warnings (linkcheck `warning-policy = "error"`). If `mdbook` is absent, note it and rely on the `book-build` CI check.

- [ ] **Step 7: Commit.**

```bash
git add crates/paigasus-helikon-tools/README.md crates/paigasus-helikon-tools/CHANGELOG.md crates/paigasus-helikon-tools/src/lib.rs crates/paigasus-helikon/README.md docs/book/src/concepts/tools.md
git commit -m "docs(tools): SMA-426 document the macOS Seatbelt backend"
```

---

## Task 8: Full local gate + push + PR

**Files:** none (verification + integration)

- [ ] **Step 1: Reproduce the CI gates locally (macOS).**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
HELIKON_REQUIRE_SANDBOX=1 cargo test -p paigasus-helikon-tools --all-features
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-tools --all-features --no-deps
```
Expected: all green. The `HELIKON_REQUIRE_SANDBOX=1` run proves the Seatbelt tests actually enforce here (they don't skip).

- [ ] **Step 2: Cross-check the whole workspace still builds for Linux.**

```bash
cargo check -p paigasus-helikon-tools --target x86_64-unknown-linux-gnu --features os-sandbox --lib
```
Expected: clean (the Linux backend + shared `spawn_capped` change compile).

- [ ] **Step 3: Push the branch.**

```bash
git push -u origin feature/sma-426-paigasus-helikon-tools-macos-seatbelt-executionbackend-for
```
(The pre-push hook runs fmt + clippy + convco; expect it to pass. If a signing error appears, ask the user to unlock 1Password.)

- [ ] **Step 4: Open the PR.**

```bash
gh pr create --fill --title "feat(tools): SMA-426 add macOS Seatbelt ExecutionBackend for Bash"
```
(Title satisfies pr-title.yml: valid `type(scope):` + lowercase subject after `SMA-426`.) Add a body summarizing the write-focused posture decision and linking the spec.

- [ ] **Step 5: Watch CI; honour the Task 6 fallback.**

Run: `gh pr checks --watch`
Expected: all required checks green — including the newly-required `test (macos-latest, stable)`. If `test (ubuntu-latest, stable)` fails on the Landlock retrofit, apply the Task 6 fallback (scope `HELIKON_REQUIRE_SANDBOX` to the macOS matrix entry) and re-push.

---

## Self-review (completed by plan author)

**Spec coverage:**
- §2.1 mechanism (sandbox-exec) → Task 2 (`wrapper_prefix`, `SANDBOX_EXEC`). ✅
- §2.2 write-focused posture → Task 2 (`build_profile`), Task 3 (AC test). ✅
- §2.3 guarantees honesty → Task 2 (`guarantees`), Task 7 (docs). ✅
- §2.4 CI honesty (guard + REQUIRE_SANDBOX + macOS required) → Task 2 (guard), Task 6. ✅
- §5 spawn_capped generalization → Task 1. ✅
- §6.1 builder parity incl. read_paths no-op → Task 2. ✅
- §6.2 profile, ROOT via -D, reuse canonical root → Task 2. ✅
- §6.3 fail-closed probe (/bin/sh + write check) → Task 2 (`probe`). ✅
- §9 tests (python3 EPERM/REFUSED markers) → Tasks 3, 4. ✅
- §9 example on macOS → Task 5. ✅
- §10 docs (README/CHANGELOG/facade/lib/mdBook) → Task 7. ✅
- §8 release (additive, no Cargo change) → no task needed; CHANGELOG entry in Task 7; release-plz handles the bump.
- Review #1/#2/#3 (CI) → Task 6. #4/#5/#7/#8/#9/#10/#11/#12/#13 → Tasks 2/6 as dispositioned in spec §13. ✅

**Placeholder scan:** no TBD/TODO; every code step shows complete code; the one read-then-integrate step (mdBook, Task 7 Step 5) lists the exact facts to add. ✅

**Type consistency:** `spawn_capped(cfg, prefix, command, configure_child)` used identically in Tasks 1 & 2; `build_profile(allow_network) -> String`, `wrapper_prefix(root, profile) -> Vec<OsString>`, `probe(root, profile) -> Result<(), OsSandboxError>`, `seatbelt_unavailable(root) -> bool` consistent across tasks. `OsSandboxBackend`/`OsSandboxBackendBuilder`/`OsSandboxError` match the Linux names (required for cross-platform parity). ✅
