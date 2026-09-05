# Windows Job Object Subtree Kill — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make an exec timeout on Windows kill the whole spawned process subtree, matching the unix process-group kill, so a timed-out command cannot leave its real workload running.

**Architecture:** `spawn_capped` assigns the spawned `cmd.exe` to an anonymous Windows Job Object immediately after spawn, and calls `TerminateJobObject` on the timeout path where unix sends `SIGKILL` to the process group. Any Win32 failure degrades to today's `child.start_kill()` and emits a `warn!`. No unix behaviour changes.

**Tech Stack:** Rust, `tokio::process`, `windows-sys 0.61` (already in `Cargo.lock` transitively), `tracing`, `std::os::windows::io::OwnedHandle`.

**Spec:** `docs/superpowers/specs/2026-09-05-windows-job-object-subtree-kill-design.md`

## Global Constraints

- **Crate:** `paigasus-helikon-tools`. Base is `bd742ac` (tools `0.2.18`, facade `0.5.19`, both published).
- **Do not hand-bump any crate version and do not edit any `CHANGELOG.md`.** release-plz owns both. No `paigasus-helikon-core` API is added here.
- **MSRV 1.94.** `windows-sys 0.61.2` declares `rust-version = "1.71"` — fine.
- **Workspace inheritance is mandatory.** Third-party pins go in root `[workspace.dependencies]`; members reference them with `{ workspace = true }`.
- **Commit prefix:** `<type>(<scope>): SMA-613 <lowercase message>`. A `commit-msg` hook runs `convco check`.
- **Tracing conformance (enforced by `tests/workspace-lints/`):** every `tracing` macro under `crates/*/src` must carry an explicit `target: "paigasus::tools::exec"` **string literal**; **no `#[tracing::instrument]`** anywhere under `crates/*/src`; and the book's component table must equal the source component set.
- **`unsafe` needs a `SAFETY:` comment**, per the existing style at `exec/mod.rs:257-258` and `exec/mod.rs:345`.
- **Accepted gap (do not try to fix):** a grandchild spawned in the microseconds between `CreateProcessW` returning and `AssignProcessToJobObject` escapes the job. `CREATE_SUSPENDED` is explicitly out of scope — the std APIs that would make it safe are nightly-only.
- **No `KILL_ON_JOB_CLOSE`.** A normally-completed run leaves survivors alive, exactly as unix does. Create the job with no limit flags.
- **The Windows behaviour cannot be run locally.** `test (windows-latest, stable)` is the only execution gate and takes ~14 min. Local verification is cross-target `clippy` plus running the portable test on unix.

---

### Task 1: Unblock the Windows lint gate

`cargo clippy --target x86_64-pc-windows-gnu … -D warnings` is the only lint coverage the new Windows code will ever get — CI runs clippy on `ubuntu-latest` only (`ci.yml:43-44`), and the Windows matrix leg runs `cargo test`, which does not apply `-D warnings`. That command currently **fails on `main`** for an unrelated, latent reason, so it must be made green before it is useful as a gate.

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/host.rs:132`

**Interfaces:**
- Consumes: nothing.
- Produces: a green Windows-target clippy for the lib and for the
  `exec_timeout_portable` test target, relied on by every later task.

> **Scope note (ruling, 2026-09-05).** These commands were originally written as a
> single `--all-targets` invocation. That is **not achievable** and never was:
> `--all-targets` on the Windows target trips pre-existing `missing_docs` and
> `dead_code` failures across unrelated test files (`bash.rs`, `forkd_tls`,
> `egress_proxy`, `sandbox_navigation`) which are `#![cfg(unix)]`-gated and go
> near-empty on Windows. The earlier verification missed this because the build
> stopped at the lib error before reaching the test targets. Fixing those files is
> a repo-wide cleanup outside SMA-613, so the gate is scoped to the lib plus the
> one test target this ticket touches.

- [ ] **Step 1: Observe the existing failure**

Run:

```bash
cargo clippy --target x86_64-pc-windows-gnu -p paigasus-helikon-tools -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p paigasus-helikon-tools \
  --test exec_timeout_portable -- -D warnings
```

Expected: FAIL with

```text
error: unused variable: `limits`
  --> crates/paigasus-helikon-tools/src/exec/host.rs:132:13
```

The binding is consumed only inside the `#[cfg(unix)]` block of the closure below it, so on Windows it is dead.

- [ ] **Step 2: `cfg`-gate the binding**

In `crates/paigasus-helikon-tools/src/exec/host.rs`, change:

```rust
    async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError> {
        let limits = self.limits.clone();
```

to:

```rust
    async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError> {
        // Consumed only by the `#[cfg(unix)]` `pre_exec` hook below; on Windows
        // the closure captures nothing. Without the gate this is an
        // `unused_variables` error under `-D warnings` on the Windows target —
        // which CI cannot see, because clippy runs on ubuntu only.
        #[cfg(unix)]
        let limits = self.limits.clone();
```

- [ ] **Step 3: Verify the Windows target is clean**

Run:

```bash
cargo clippy --target x86_64-pc-windows-gnu -p paigasus-helikon-tools -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p paigasus-helikon-tools \
  --test exec_timeout_portable -- -D warnings
```

Expected: PASS, no warnings.

- [ ] **Step 4: Verify unix is unaffected**

Run:

```bash
cargo clippy -p paigasus-helikon-tools --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-tools
```

Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/src/exec/host.rs
git commit -m "fix(tools): SMA-613 cfg-gate the unix-only limits binding

The binding is consumed only inside the #[cfg(unix)] pre_exec hook, so on
Windows it is an unused_variables error under -D warnings. CI cannot see it:
clippy runs on ubuntu-latest only, and the Windows matrix leg runs cargo test,
which does not apply -D warnings. Surfaced by making cross-target clippy a
documented pre-PR step for SMA-613."
```

---

### Task 2: The regression test, red on Windows by construction

TDD, adapted to a platform that cannot be run locally. The test is **portable**: on unix it passes immediately (it guards the existing `process_group(0)` kill, which has no test of its own today), and it can be *falsified* locally by degrading the unix kill — which proves it is not vacuous. On Windows it will fail until Task 3, which is correct: it encodes the bug.

**Files:**
- Modify: `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`

**Interfaces:**
- Consumes: `HostBackend`, `Sandbox`, `ExecRequest` from `paigasus_helikon_tools` (already imported in this file).
- Produces: `timeout_kills_the_whole_subtree`, the acceptance test for Task 3.

- [ ] **Step 1: Write the failing test**

> **Amended during implementation.** The shipped test differs from the code below in
> two ways, both forced by Windows. (1) The script path and both sentinel paths are
> **absolute**, built at runtime with `format!`, because `Sandbox::open` canonicalizes
> and on Windows that yields a verbatim `\\?\C:\...` path `cmd.exe` may reject as UNC —
> see SMA-615. (2) The Windows invocation is **unquoted**: `Command::arg`'s escaper
> rewrites `"` to `\"` and `cmd.exe`'s escape character is `^`, so no literal quote
> survives the trip to a nested `cmd /C`. The quotes inside the generated script are
> fine, and the unix arm keeps its quotes because `execve` does not mangle them.
> Read `tests/exec_timeout_portable.rs` for the shipped form.

Append to `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`:

```rust
/// Name of the script the test drops into the sandbox for the grandchild to run.
#[cfg(unix)]
const GRANDCHILD_SCRIPT_NAME: &str = "grandchild.sh";
#[cfg(windows)]
const GRANDCHILD_SCRIPT_NAME: &str = "grandchild.cmd";

/// Writes `started` immediately, then `alive` only after a delay that outlives
/// the backend timeout. Two sentinels, not one: a test that asserted only
/// "`alive` is absent" would pass for free every time the grandchild failed to
/// launch at all (wrong cwd, script not written, `.cmd` misparsed) — a false
/// green on a Windows-only path whose sole behavioural gate is one CI job.
#[cfg(unix)]
const GRANDCHILD_SCRIPT: &str = "echo started > started\nsleep 4\necho alive > alive\n";

/// CRLF is explicit: the file is created at runtime, so nothing in
/// `.gitattributes` governs it, and `cmd.exe` batch parsing is the wrong place
/// to discover an LF assumption.
#[cfg(windows)]
const GRANDCHILD_SCRIPT: &str =
    "@echo off\r\necho started>started\r\nping -n 5 127.0.0.1 >NUL\r\necho alive>alive\r\n";

/// A command whose sentinel-writer is a **grandchild** of the shell `spawn_capped`
/// spawns, so killing only the direct child leaves it running.
///
/// The trailing `; true` is load-bearing: without it `sh -c` applies its
/// single-command `exec` optimisation and replaces the outer shell, collapsing
/// the tree so the writer is a child, not a grandchild — and the test would
/// silently stop guarding two levels.
#[cfg(unix)]
const SPAWNS_GRANDCHILD: &str = "sh grandchild.sh; true";

/// `build_command` turns this into `cmd /C "cmd /C grandchild.cmd"`, so the outer
/// `cmd.exe` spawns an inner `cmd.exe` that runs the batch and waits.
///
/// Deliberately not `start /B`: `START` is the documented `CREATE_BREAKAWAY_FROM_JOB`
/// case, and a job created with no limit flags is exactly the configuration it
/// interacts with — a test that passed because `start` broke away would be worse
/// than no test. A plain nested `cmd` needs no such escape.
#[cfg(windows)]
const SPAWNS_GRANDCHILD: &str = "cmd /C grandchild.cmd";

/// A timed-out run kills the whole spawned subtree, not just the direct child.
///
/// Regression guard for SMA-613. On unix this guards the long-standing
/// `process_group(0)` + `SIGKILL` path, which had no test of its own. On Windows
/// it guards the Job Object kill that replaced a bare `TerminateProcess` against
/// `cmd.exe`, which left every grandchild running to completion.
#[tokio::test]
async fn timeout_kills_the_whole_subtree() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(GRANDCHILD_SCRIPT_NAME), GRANDCHILD_SCRIPT).unwrap();

    // 1s, not the 200ms used above: the `started` positive control must be
    // written before the kill lands, and 1s is generous margin for process
    // launch on a loaded CI runner.
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(Duration::from_secs(1))
        .build();

    // Failure case is ~6s (1s timeout + the 5s GRACE reader drain, because a
    // surviving grandchild holds the inherited pipe write end) plus the 6s wait
    // below. 30s leaves headroom without masking a real hang.
    let out = tokio::time::timeout(
        Duration::from_secs(30),
        backend.run(ExecRequest::new(SPAWNS_GRANDCHILD)),
    )
    .await
    .expect("run must return promptly, not hang")
    .unwrap();

    assert!(out.timed_out, "the command must outlive the 1s timeout");

    // Wait past the moment a surviving grandchild would have written `alive`.
    tokio::time::sleep(Duration::from_secs(6)).await;

    assert!(
        tmp.path().join("started").exists(),
        "positive control failed: the grandchild never ran, so the `alive` \
         assertion below would have passed for free"
    );
    assert!(
        !tmp.path().join("alive").exists(),
        "the grandchild outlived the timeout: the subtree was not killed"
    );
}
```

- [ ] **Step 2: Run it on unix — it must PASS**

Run:

```bash
cargo test -p paigasus-helikon-tools --test exec_timeout_portable timeout_kills_the_whole_subtree -- --nocapture
```

Expected: PASS in roughly 7s. Unix already kills the process group, so this is the guard confirming existing behaviour.

If it fails, the test harness is wrong (script path, cwd, timing) — fix that before going further. Do not proceed with a test whose unix arm is broken.

- [ ] **Step 3: Falsify it — prove it is not vacuous**

Temporarily degrade the unix kill so it behaves like today's Windows path. In `crates/paigasus-helikon-tools/src/exec/mod.rs`, in the timeout arm, comment out the `libc::kill` block and replace it with `let _ = child.start_kill();`:

```rust
            #[cfg(unix)]
            {
                // TEMPORARY FALSIFICATION — revert in Step 4.
                let _ = child.start_kill();
            }
```

Run:

```bash
cargo test -p paigasus-helikon-tools --test exec_timeout_portable timeout_kills_the_whole_subtree
```

Expected: **FAIL** with `the grandchild outlived the timeout: the subtree was not killed`.

This is the red half of the cycle, and the only place in this plan where the guard can be observed failing on a machine we own. A regression test never seen red is a hypothesis, not a guard.

- [ ] **Step 4: Revert the falsification**

Run:

```bash
# Reverses ONLY the falsification hunk. Do not use `git checkout -- <file>`:
# that discards every uncommitted change in the file, not just this one.
git diff -- crates/paigasus-helikon-tools/src/exec/mod.rs | git apply -R
cargo test -p paigasus-helikon-tools --test exec_timeout_portable timeout_kills_the_whole_subtree
```

Expected: PASS again. Confirm `git diff crates/paigasus-helikon-tools/src/exec/mod.rs` is empty.

- [ ] **Step 5: Verify it compiles for Windows**

Run:

```bash
cargo clippy --target x86_64-pc-windows-gnu -p paigasus-helikon-tools -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p paigasus-helikon-tools \
  --test exec_timeout_portable -- -D warnings
```

Expected: PASS. This compiles the `#[cfg(windows)]` consts and the test body; it cannot run them.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs
git commit -m "test(tools): SMA-613 guard that a timeout kills the whole subtree

Portable regression test with a positive control: the grandchild writes a
started sentinel immediately and an alive sentinel only after a delay past the
timeout, so the test cannot pass vacuously when the grandchild never launched.

Passes on unix today, guarding the process_group(0) kill that had no test of
its own, and verified to fail there when that kill is degraded to start_kill().
Expected red on Windows until the Job Object lands."
```

---

### Task 3: Job Object kill, instrumented

**Files:**
- Create: `crates/paigasus-helikon-tools/src/exec/job_object.rs`
- Modify: `Cargo.toml` (root `[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon-tools/Cargo.toml`
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs`
- Modify: `Cargo.lock` (regenerated, must be committed)

**Interfaces:**
- Consumes: `timeout_kills_the_whole_subtree` from Task 2 as the acceptance test.
- Produces: `pub(crate) struct JobObject` with `assign(process: RawHandle) -> std::io::Result<Self>` and `terminate(&self) -> bool`, used only by `spawn_capped`.

> **Amended during implementation.** `terminate` ships as
> `-> std::io::Result<()>`, not `-> bool`, so the timeout-path `warn!` can name the
> OS error; the caller matches on the `Result` rather than using `is_some_and`. The
> `bool` form shown in the steps below is the pre-review design, kept for the record.
> Likewise, the regression test in Task 2 ships with **absolute** script and sentinel
> paths and an **unquoted** Windows invocation — see the amendment note in Task 2.

- [ ] **Step 1: Add the dependencies**

In the root `Cargo.toml`, add to `[workspace.dependencies]` (keep the file's existing alignment style):

```toml
# Windows Job Objects, for the exec-timeout subtree kill (SMA-613). Already in
# Cargo.lock transitively via tokio/mio, so this adds an edge, not a package.
# Win32_Security is required, not cosmetic: CreateJobObjectW is gated
# #[cfg(feature = "Win32_Security")] because its signature names
# SECURITY_ATTRIBUTES. Win32_Foundation is transitively implied
# (Win32_System_JobObjects -> Win32_System -> Win32 -> Win32_Foundation) and is
# listed only because the code names its types directly.
windows-sys = { version = "0.61", features = [
  "Win32_Foundation",
  "Win32_Security",
  "Win32_System_JobObjects",
] }
```

In `crates/paigasus-helikon-tools/Cargo.toml`, add `tracing` to `[dependencies]` (after `anyhow`, matching the existing alignment):

```toml
tracing               = { workspace = true }
```

and add a new target section immediately after the existing `[target.'cfg(unix)'.dependencies]` block:

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { workspace = true }
```

`tracing` is unconditional rather than `cfg(windows)`-gated: a target-gated dependency whose absence is felt on only one platform is a trap for the next person to add an event.

- [ ] **Step 2: Create the Job Object wrapper**

Create `crates/paigasus-helikon-tools/src/exec/job_object.rs`:

```rust
//! A Windows Job Object — the platform equivalent of the unix process-group
//! kill [`super::spawn_capped`] performs on timeout (SMA-613).
//!
//! Deliberately minimal: the job is created with **no limit flags**, so it kills
//! its members only when [`JobObject::terminate`] is called explicitly. In
//! particular `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` is *not* set, because that
//! would reap survivors of a normally-completed run — which unix does not do.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};

/// Owns an anonymous job object handle.
///
/// The wrapped handle is always the **job** handle. The process handle passed to
/// [`JobObject::assign`] is borrowed from tokio's `Child` and is never closed
/// here — closing it would pull the rug from under tokio's own reaping.
///
/// `OwnedHandle` is `Send + Sync` and closes on drop, which is why this type
/// needs neither an `unsafe impl Send` (required, since `spawn_capped`'s future
/// is `Send`-bounded by `#[async_trait]`) nor a hand-written `Drop`.
pub(crate) struct JobObject(OwnedHandle);

impl JobObject {
    /// Create an anonymous job object and assign `process` to it.
    ///
    /// Note the ordering: the handle is wrapped in an `OwnedHandle` *before* the
    /// assignment is attempted, so a failing assign drops the wrapper and closes
    /// the handle rather than leaking it. That path is reachable — it is exactly
    /// the locked-down-runner case the caller degrades on.
    pub(crate) fn assign(process: RawHandle) -> io::Result<Self> {
        // SAFETY: a NULL `lpJobAttributes` selects default security and, more to
        // the point, leaves `bInheritHandle` FALSE — an inheritable job handle
        // would leak into every later spawn, since std spawns with
        // `bInheritHandles = TRUE`. A NULL `lpName` makes the job anonymous.
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if raw.is_null() {
            // CreateJobObjectW reports failure as NULL, not INVALID_HANDLE_VALUE.
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `raw` is a fresh, non-null handle we exclusively own, so
        // transferring ownership to `OwnedHandle` is sound.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) });

        // SAFETY: both handles are valid — the job was just created, and
        // `process` is borrowed from a live tokio `Child`.
        if unsafe { AssignProcessToJobObject(raw, process as HANDLE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Terminate every process in the job. Returns `false` if the call failed,
    /// which the caller must treat as "nothing was killed" and fall back.
    pub(crate) fn terminate(&self) -> bool {
        // SAFETY: the handle is valid for the lifetime of `self`. The exit code
        // becomes every member's, which is harmless: the timeout path reports
        // `exit_code: None` regardless, per `ExecOutput::exit_code`.
        unsafe { TerminateJobObject(self.0.as_raw_handle() as HANDLE, 1) != 0 }
    }
}
```

- [ ] **Step 3: Wire it into `spawn_capped`**

In `crates/paigasus-helikon-tools/src/exec/mod.rs`, add the module next to the existing backend module declarations near the top of the file (both lines gated — an ungated `use` of a Windows-only module is a hard error elsewhere):

```rust
#[cfg(windows)]
mod job_object;
#[cfg(windows)]
use job_object::JobObject;
```

Then, immediately after the existing `#[cfg(unix)] let pgid = child.id();` and **before** the `stdout`/`stderr` pipes are taken, add:

```rust
    // Assign as the very next statement after spawn: a grandchild spawned in
    // the window before this lands escapes the job (accepted gap, SMA-613).
    #[cfg(windows)]
    let job = match child.raw_handle().map(JobObject::assign) {
        Some(Ok(j)) => Some(j),
        Some(Err(e)) => {
            tracing::debug!(
                target: "paigasus::tools::exec",
                error = %e,
                "could not put the child in a job object; a timeout will kill only \
                 the direct child"
            );
            None
        }
        // `raw_handle()` is `None` once the child has exited — vanishingly rare
        // this soon after spawn, but it degrades the same way.
        None => None,
    };
```

Finally, replace the timeout arm's `#[cfg(not(unix))]` block with:

```rust
            #[cfg(windows)]
            {
                // Any Win32 failure — including a failed terminate — degrades to
                // exactly today's behaviour. Without the `start_kill` fallback a
                // failed terminate would kill *nothing*, not even `cmd.exe`,
                // which is strictly worse than not having the job at all.
                if !job.as_ref().is_some_and(JobObject::terminate) {
                    tracing::warn!(
                        target: "paigasus::tools::exec",
                        "job object unavailable or terminate failed; killed only the \
                         direct child, so processes it spawned may have survived the \
                         timeout"
                    );
                    let _ = child.start_kill();
                }
            }
            #[cfg(not(any(unix, windows)))]
            {
                let _ = child.start_kill();
            }
```

- [ ] **Step 4: Verify the Windows target compiles and lints clean**

Run:

```bash
cargo clippy --target x86_64-pc-windows-gnu -p paigasus-helikon-tools -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p paigasus-helikon-tools \
  --test exec_timeout_portable -- -D warnings
```

Expected: PASS. This is the only lint coverage this code will ever get.

If `windows-sys` reports an unresolved `SECURITY_ATTRIBUTES`, the `Win32_Security` feature is missing from the root manifest.

- [ ] **Step 5: Verify unix is unchanged**

Run:

```bash
cargo test -p paigasus-helikon-tools
cargo clippy -p paigasus-helikon-tools --all-features --all-targets -- -D warnings
```

Expected: both PASS, including `timeout_kills_the_whole_subtree` — the unix arm must be byte-for-byte unaffected.

- [ ] **Step 6: Verify the tracing conformance lints**

Run:

```bash
cargo test -p paigasus-helikon-workspace-lints
```

Expected: `tracing_target_coverage` PASSES (the two events carry the literal `target: "paigasus::tools::exec"`), and `tracing_target_docs` **FAILS**, because `paigasus::tools` is now emitted in source but absent from the book. That failure is expected here and is fixed in Task 4.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/paigasus-helikon-tools/Cargo.toml \
        crates/paigasus-helikon-tools/src/exec/job_object.rs \
        crates/paigasus-helikon-tools/src/exec/mod.rs
git commit -m "feat(tools): SMA-613 kill the whole subtree on a windows timeout

Assign the spawned cmd.exe to an anonymous job object immediately after spawn
and TerminateJobObject on the timeout path, where unix SIGKILLs the process
group. Previously the timeout ran TerminateProcess against cmd.exe alone, so
everything it spawned ran to completion while the call reported a clean
timed_out: true.

The job carries no limit flags, so a normally-completed run leaves survivors
alive exactly as unix does. Any Win32 failure, terminate included, falls back
to start_kill() and warns: a failed terminate with no fallback would kill
nothing at all, which is worse than not having the job.

Accepted gap: a grandchild spawned in the microseconds before the assignment
lands escapes the job. Closing it needs CREATE_SUSPENDED, whose safe undo
(main_thread_handle) is nightly-only."
```

---

### Task 4: Documentation

Four rustdoc sites, one false comment already in the tree, and two book pages. The `tracing_target_docs` failure from Task 3 Step 6 turns green here.

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs` (`ExecOutput::timed_out`, `spawn_capped` doc)
- Modify: `crates/paigasus-helikon-tools/src/exec/host.rs:2` and `:35`
- Modify: `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs:18-21`
- Modify: `docs/book/src/concepts/tools.md`
- Modify: `docs/book/src/concepts/observability-evaluation.md`

**Interfaces:**
- Consumes: the behaviour landed in Task 3.
- Produces: nothing code-facing.

- [ ] **Step 1: Update `ExecOutput::timed_out`**

In `crates/paigasus-helikon-tools/src/exec/mod.rs`, replace:

```rust
    /// Whether the command was killed because it exceeded the timeout.
    pub timed_out: bool,
```

with:

```rust
    /// Whether the command was killed because it exceeded the timeout.
    ///
    /// A timeout kills the whole spawned subtree, not just the direct child: a
    /// process group `SIGKILL` on unix, a Job Object termination on Windows.
    ///
    /// One accepted gap on Windows: a process spawned in the brief window
    /// between the shell starting and its assignment to the job object is not a
    /// member, and survives. Closing it requires APIs that are nightly-only on
    /// stable Rust today.
    pub timed_out: bool,
```

- [ ] **Step 2: Update `spawn_capped`'s doc comment**

In the same file, change the first line of `spawn_capped`'s doc from:

```rust
/// Spawn `command` under `cfg`, draining stdout/stderr concurrently, killing the
/// whole process group on timeout.
```

to:

```rust
/// Spawn `command` under `cfg`, draining stdout/stderr concurrently, killing the
/// whole process subtree on timeout — a process group on unix, a Job Object on
/// Windows.
```

- [ ] **Step 3: Fix the two `host.rs` doc sites**

`crates/paigasus-helikon-tools/src/exec/host.rs:2` — change `a timeout (process-group kill)` to `a timeout (whole-subtree kill)`.

`crates/paigasus-helikon-tools/src/exec/host.rs:35` — change:

```rust
    /// Wall-clock timeout before the process group is killed (default 30s).
```

to:

```rust
    /// Wall-clock timeout before the whole process subtree is killed (default 30s).
```

Both are on the all-platforms default backend. Leave `os_sandbox.rs:59` and `os_sandbox_seatbelt.rs:58` alone — those crates are unix-only, so "process group" is still correct there.

- [ ] **Step 4: Correct the false comment on `HANG`**

In `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`, replace the `#[cfg(windows)] const HANG` doc comment with:

```rust
/// `ping` ships with every Windows install; `-n 5` blocks ~4s, 20x the 200ms
/// timeout. Output goes to `NUL` to keep the captured streams clean.
///
/// Note this redirect does **not** stop `ping` holding our stdout/stderr pipe
/// write ends: `CreateProcess` runs with `bInheritHandles = TRUE`, so every
/// inheritable handle in `cmd.exe` is duplicated into `ping` regardless of where
/// `cmd.exe` points its `hStdOutput`. Before SMA-613 that inherited writer is
/// exactly why this test took ~4s on Windows against ~0.2s on unix — the reader
/// drain waited out the orphaned grandchild.
```

- [ ] **Step 5: Document timeout semantics in the tools book page**

In `docs/book/src/concepts/tools.md`, in the `HostBackend` section, immediately after the paragraph beginning "The default backend. Pins the working directory…", insert:

```markdown
When a command exceeds its timeout the **whole spawned subtree** is killed, not
just the shell: a process-group `SIGKILL` on unix, a Job Object termination on
Windows. `ExecOutput::timed_out` is `true` and `exit_code` is `None` on every
platform — a killed process has no meaningful exit code.

One accepted gap on Windows: a process spawned in the brief window between the
shell starting and its assignment to the job object is not a member of it, and
survives the kill. Closing that window needs Win32 process-attribute APIs that
are nightly-only on stable Rust today.
```

- [ ] **Step 6: Register `paigasus::tools` in the observability book page**

In `docs/book/src/concepts/observability-evaluation.md`, add a row to the table inside the `tracing-components:start` / `tracing-components:end` markers, after the `paigasus::core` row:

```markdown
| `paigasus::tools` | `paigasus-helikon-tools` | `exec` | provisional |
```

`provisional`, not `stable`: per that page's own stability rules a provisional component carries no rename promise, which is the honest status for a brand-new component with a single Windows-only subsystem.

Then update the prose below the table. Change:

```markdown
Ten crates have a name under this rule (nine derived, plus the facade's
`facade`) with no call site emitting on it yet, because those crates carry no
`tracing` instrumentation today: `facade`, `macros`, `mcp`, `tools`, `evals`,
`cli`, `sessions_sqlite`, `sessions_postgres`, `sessions_redis`,
`sessions_testkit`.
```

to:

```markdown
Nine crates have a name under this rule (eight derived, plus the facade's
`facade`) with no call site emitting on it yet, because those crates carry no
`tracing` instrumentation today: `facade`, `macros`, `mcp`, `evals`,
`cli`, `sessions_sqlite`, `sessions_postgres`, `sessions_redis`,
`sessions_testkit`.
```

The prose is not parsed by any test, so missing this half fails nothing — it just silently becomes untrue.

- [ ] **Step 7: Verify the conformance lints are now green**

Run:

```bash
cargo test -p paigasus-helikon-workspace-lints
```

Expected: PASS, including `tracing_target_docs`, which failed at the end of Task 3.

- [ ] **Step 8: Verify docs and markdown gates**

Run:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-tools --all-features --no-deps
npx markdownlint-cli2
mdbook build docs/book
```

Expected: all PASS. If `npx markdownlint-cli2` fails to start with a Node syntax error, run `npm ci` first — CI pins the version via `package-lock.json`.

- [ ] **Step 9: Commit**

```bash
git add crates/paigasus-helikon-tools/src crates/paigasus-helikon-tools/tests docs/book
git commit -m "docs(tools): SMA-613 document the whole-subtree timeout kill

Four rustdoc sites still described the timeout as a process-group kill, which
was unix-specific wording on the all-platforms default backend, and states the
Windows accepted gap on the API surface rather than only in the book.

Corrects a comment that was already wrong: >NUL does not stop the grandchild
inheriting our pipe write ends, because CreateProcess runs with
bInheritHandles = TRUE. That inherited writer is why the test took ~4s on
Windows against ~0.2s on unix.

Registers paigasus::tools as a provisional tracing component, which the
workspace lint requires now that the crate emits."
```

---

### Task 5: Full local gate sweep

Every CI gate that can run on this host, in one pass, before the PR.

**Files:** none.

**Interfaces:**
- Consumes: Tasks 1-4.
- Produces: evidence the PR is ready.

- [ ] **Step 1: Rebase onto current `main`**

```bash
git fetch origin main
git rebase origin/main
```

If `Cargo.lock` conflicts, take `main`'s version and regenerate rather than hand-merging:

```bash
git checkout --theirs Cargo.lock && cargo build --workspace && git add Cargo.lock && git rebase --continue
```

- [ ] **Step 2: Run the CI-equivalent gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
npx markdownlint-cli2
bash scripts/check-markdownlint-config.sh
mdbook build docs/book
```

Expected: all PASS.

- [ ] **Step 3: Run the Windows-target lint one final time**

```bash
cargo clippy --target x86_64-pc-windows-gnu -p paigasus-helikon-tools -- -D warnings
cargo clippy --target x86_64-pc-windows-gnu -p paigasus-helikon-tools \
  --test exec_timeout_portable -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: Verify no version or changelog was touched**

```bash
git diff origin/main --stat -- '**/Cargo.toml' '**/CHANGELOG.md'
```

Expected: the only `Cargo.toml` changes are the root `[workspace.dependencies]` addition and the tools crate's two dependency lines. **No `version =` line may appear in the diff, and no `CHANGELOG.md` at all** — release-plz owns both.

- [ ] **Step 5: Confirm the commit history is conventional**

```bash
convco check "$(git merge-base origin/main HEAD)..HEAD"
```

Expected: PASS. Use the merge-base, never a branch tip — `convco` silently walks all of history when the base is not an ancestor.

---

## Post-implementation note for the PR

`test (windows-latest, stable)` is the only gate that executes the new behaviour, and it takes ~14 minutes. Two things to check on the first CI run:

1. **`timeout_kills_the_whole_subtree` passes on Windows.** If it fails on the `started` positive control, the grandchild never launched and the batch script or command string is wrong — not the job object.
2. **`timeout_reports_no_exit_code`'s Windows wall time drops** from ~4.06s toward the unix ~0.2s. Not asserted, but it is the secondary signal from the ticket that the subtree really died.

If `AssignProcessToJobObject` turns out to be blocked on the runner, the plan of record is to **revert the PR and reopen SMA-613** with the constraint documented — not to weaken the test until it passes.
