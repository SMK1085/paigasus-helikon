# SMA-615 Windows cwd Pinning — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Determine on Windows CI whether `HostBackend` actually pins the child's working directory to the sandbox root, and either fix it or record the finding — leaving a permanent, ungated regression test either way.

**Architecture:** A new ungated integration test asserts the documented cwd contract; it is simultaneously the experiment and the permanent guard. A Windows-only *oracle* test asserts the raw, un-normalized reported path so that a green contract test still distinguishes "Windows normalized the verbatim prefix" from "cmd.exe tolerated it". Task 2 is a hard CI checkpoint: the branch taken afterwards (Tasks 3A–5A, or Task 3B) depends on what the Windows gate reports. If a fix is needed it is one Windows-gated `dunce::canonicalize` call in `Sandbox::open`, so every consumer of `Sandbox::root()` — including third-party `ExecutionBackend` implementors — is fixed at one choke point.

**Tech Stack:** Rust 2024 (MSRV 1.94), `tokio` test runtime, `tempfile`, `cap-std`, and — only on the confirmed branch, only on Windows — `dunce`.

**Spec:** `docs/superpowers/specs/2026-09-05-windows-cwd-pin-design.md`

## Global Constraints

- **Worktree:** `/Users/sven/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-615`, branch `feature/sma-615-hostbackend-may-not-actually-pin-cwd-on-windows-sandboxopen`, based on `origin/main` at `1d661c8`. Run everything from there; never `cd` to the main checkout.
- **Commit format:** `<type>(<scope>): SMA-615 <lowercase message>`. Valid types are in `.versionrc`; `tools` is the scope. The `commit-msg` hook runs `convco check`.
- **Never hand-bump a version and never edit a `CHANGELOG.md`.** release-plz owns both. The commit subject *is* the changelog entry.
- **Do not push again while a CI run is in flight.** `.github/workflows/ci.yml:8-10` sets `cancel-in-progress: true` on pull requests, so a second push cancels the experiment.
- **`cargo test` runs without `-D warnings` on the Windows gate,** and `clippy` runs on ubuntu only. New `#[cfg(windows)]` code is linted by *no* CI gate — Task 4A carries the local cross-target check.
- **Every assertion in `tests/exec_cwd.rs` must embed both `out.stdout` and `out.stderr`.** `cargo test` runs without `--nocapture`, so a passing test's output never reaches the CI log; the assertion message is the only readout this experiment has.
- **Never use a bare `.unwrap()` on a path operation in `tests/exec_cwd.rs`** — it surfaces a bare `io::Error` and discards the readout on exactly the paths where it matters.

## File Structure

| File | Status | Responsibility |
| -- | -- | -- |
| `crates/paigasus-helikon-tools/tests/exec_cwd.rs` | **create** (Task 1) | The whole cwd question: contract test, behavioural test, Windows oracle. Ungated. |
| `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs` | modify (Task 1) | Module-doc correction only — its "only ungated file" claim is stale. |
| `crates/paigasus-helikon-tools/src/sandbox.rs` | modify (Task 3A **or** 3B) | The canonicalize call and `root()`'s contract. |
| `Cargo.toml` (root) | modify (Task 3A) | `dunce` version pin in `[workspace.dependencies]`. |
| `crates/paigasus-helikon-tools/Cargo.toml` | modify (Task 3A) | `dunce` as a Windows-only dependency. |
| `crates/paigasus-helikon-tools/tests/sandbox.rs` | modify (Task 3A) | `root()`'s assertion, which the strip invalidates. |
| `crates/paigasus-helikon-tools/src/exec/host.rs` | modify (Tasks 4A, 5A) | The degrade warning, a wrong comment, and the module-doc caveat. |
| `docs/book/src/concepts/tools.md` | modify (Task 5A) | The user-facing cwd claim. |

**Branching:** Task 2 selects. If **W1 (confirmed)** → Tasks 3A, 4A, 5A, then 6. If **W2 or W3 (refuted)** → Task 3B, then 6. Never both branches.

---

### Task 1: The ungated cwd tests (Round 1)

**Files:**

- Create: `crates/paigasus-helikon-tools/tests/exec_cwd.rs`
- Modify: `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs:1-6`

**Interfaces:**

- Consumes: `paigasus_helikon_tools::{ExecOutput, ExecRequest, HostBackend, Sandbox}` — all re-exported at the crate root (`src/lib.rs:38-42,56`). Note `ExecutionBackend` need **not** be imported: `HostBackendBuilder::build` returns `Arc<dyn ExecutionBackend>` and method lookup on a trait object resolves without the trait in scope, which is why the sibling test files do not import it either.
- Produces: `run_in_sandbox(dir: &Path, command: &str) -> (ExecOutput, PathBuf)` and `reported_cwd(out: &ExecOutput) -> &str`, both private to this file. Task 3A/3B rewrite the oracle test in this same file and reuse both.

- [ ] **Step 1: Write the test file**

Create `crates/paigasus-helikon-tools/tests/exec_cwd.rs` with exactly this content:

```rust
//! `HostBackend`'s **working-directory pinning**, asserted end-to-end on every
//! platform (SMA-615).
//!
//! Ungated on purpose. Every pre-existing real-process test that depends on cwd
//! is `#![cfg(unix)]` (`tests/host_backend.rs`), and the two ungated
//! process-spawning files that predate this one were both deliberately made
//! cwd-independent, so nothing in the suite has ever checked this on Windows.
//!
//! Every assertion here embeds BOTH captured streams. On Windows the question
//! this file exists to answer is settled by text `cmd.exe` may print to either
//! one, and `cargo test` runs without `--nocapture`, so a passing test's output
//! never reaches the CI log — the assertion message is the only readout.
#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use paigasus_helikon_tools::{ExecOutput, ExecRequest, HostBackend, Sandbox};

/// Prints the child's working directory: a `sh` builtin on unix, `cd` with no
/// arguments on Windows.
#[cfg(unix)]
const PRINT_CWD: &str = "pwd";
#[cfg(windows)]
const PRINT_CWD: &str = "cd";

/// Prints a file named by a RELATIVE path — the user-visible consequence of a
/// pinned (or unpinned) working directory.
#[cfg(unix)]
const PRINT_MARKER: &str = "cat marker.txt";
#[cfg(windows)]
const PRINT_MARKER: &str = "type marker.txt";

/// Short, newline-free, `%`-free ASCII. `cmd.exe`'s `type` emits file bytes
/// verbatim and a `%` would risk environment expansion, so this content cannot
/// make the test fail for a quoting or expansion reason.
const MARKER_CONTENT: &str = "sma615-marker";

/// What `cmd.exe` prints when it rejects its startup working directory as a UNC
/// path and resets to `%SystemRoot%`. The banner and the reset are the same code
/// path, so seeing it is confirmation on its own — whatever `cd` then printed,
/// and whichever stream it landed on.
#[cfg(windows)]
const UNC_BANNER: &str = "UNC paths are not supported";

/// Run `command` through a real `HostBackend` over a fresh sandbox at `dir`,
/// returning the captured output alongside the root the backend was told to pin
/// to.
///
/// The outer `tokio::time::timeout` mirrors `exec_timeout_portable.rs:48` and
/// `exec_env_defaults.rs:166`: a regression must fail fast rather than stall the
/// required Windows gate for the whole backend timeout.
async fn run_in_sandbox(dir: &Path, command: &str) -> (ExecOutput, PathBuf) {
    let sandbox = Sandbox::open(dir).expect("open sandbox");
    let root = sandbox.root().to_path_buf();
    let backend = HostBackend::builder(sandbox)
        .timeout(Duration::from_secs(10))
        .build();

    let out = tokio::time::timeout(
        Duration::from_secs(20),
        backend.run(ExecRequest::new(command)),
    )
    .await
    .expect("run must return promptly, not hang")
    .expect("the backend must not error; a non-zero exit is a normal result");

    (out, root)
}

/// The child's own report of its working directory, exactly as printed — NOT
/// normalized. The LAST non-empty line, because on Windows a UNC banner would
/// precede `cd`'s output.
fn reported_cwd(out: &ExecOutput) -> &str {
    out.stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "the shell printed no working directory at all; stdout={:?} stderr={:?}",
                out.stdout, out.stderr
            )
        })
}

/// `HostBackend` runs its command with the working directory pinned to the
/// sandbox root — the contract stated in `src/exec/host.rs`'s module docs, on
/// `HostBackend::builder`, and in `docs/book/src/concepts/tools.md`.
#[tokio::test]
async fn host_backend_pins_cwd_to_the_sandbox_root() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, root) = run_in_sandbox(tmp.path(), PRINT_CWD).await;

    assert_eq!(
        out.exit_code, Some(0),
        "the shell must run at all before its working directory means anything; \
         stdout={:?} stderr={:?}",
        out.stdout, out.stderr
    );

    // Decisive on its own: `cmd.exe` prints this banner on the same code path
    // that resets the working directory to `%SystemRoot%`.
    #[cfg(windows)]
    assert!(
        !out.stdout.contains(UNC_BANNER) && !out.stderr.contains(UNC_BANNER),
        "cmd.exe rejected the sandbox root as a UNC path and reset its working \
         directory; stdout={:?} stderr={:?}",
        out.stdout, out.stderr
    );

    let reported = reported_cwd(&out);

    // Canonicalize BOTH sides. That normalizes 8.3 short names (the Windows
    // runner's TEMP is `C:\Users\RUNNER~1\...`), case, and verbatim-ness. It
    // normalizes *spelling*, never *identity*, so a working directory of
    // `C:\Windows` still compares unequal here.
    let observed = Path::new(reported).canonicalize().unwrap_or_else(|e| {
        panic!(
            "the shell reported a working directory that does not resolve: \
             {reported:?}: {e}; stdout={:?} stderr={:?}",
            out.stdout, out.stderr
        )
    });
    let expected = root
        .canonicalize()
        .unwrap_or_else(|e| panic!("the sandbox root {} does not resolve: {e}", root.display()));

    assert_eq!(
        observed, expected,
        "the child ran in the wrong directory: it reported {reported:?}; \
         stdout={:?} stderr={:?}",
        out.stdout, out.stderr
    );
}

/// A relative path in a command resolves inside the sandbox. This is what a user
/// actually hits, and it fails with a different message than the contract test
/// above — so a red gate distinguishes "the working directory is elsewhere" from
/// "the two spellings differ".
///
/// The ungated counterpart of `tests/host_backend.rs`'s
/// `host_backend_runs_command_in_cwd`, which stays where it is: that file's other
/// tests are unix-only for `rlimit` reasons.
#[tokio::test]
async fn host_backend_resolves_a_relative_path_in_the_sandbox() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("marker.txt"), MARKER_CONTENT).unwrap();

    let (out, _root) = run_in_sandbox(tmp.path(), PRINT_MARKER).await;

    assert_eq!(
        out.exit_code, Some(0),
        "a relative path must resolve inside the sandbox; stdout={:?} stderr={:?}",
        out.stdout, out.stderr
    );
    assert!(
        out.stdout.contains(MARKER_CONTENT),
        "the file read back did not contain the marker; stdout={:?} stderr={:?}",
        out.stdout, out.stderr
    );
}

/// **Round-1 oracle (SMA-615) — its expected answer is NOT known.**
///
/// `host_backend_pins_cwd_to_the_sandbox_root` cannot tell two refuting worlds
/// apart, because it compares canonicalized forms. The child may report
/// `C:\Users\...` (Windows normalized the verbatim prefix away before `cmd.exe`
/// ever saw it) or `\\?\C:\Users\...` (`cmd.exe` tolerated a verbatim working
/// directory). Both pass that test; only the second justifies saying "a verbatim
/// path is a fine `cmd.exe` cwd".
///
/// This test asserts the RAW, un-normalized report, so whichever way it falls its
/// message names the observed string. It exists to be read once, on
/// `test (windows-latest, stable)`, and is then rewritten to assert whatever was
/// observed — at which point it becomes a characterization guard that goes red if
/// a future Rust or Windows release changes this behaviour underneath us.
///
/// DO NOT delete this without replacing it with the observed truth.
#[tokio::test]
#[cfg(windows)]
async fn windows_child_reports_a_verbatim_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, root) = run_in_sandbox(tmp.path(), PRINT_CWD).await;
    let reported = reported_cwd(&out);

    assert!(
        reported.starts_with(r"\\?\"),
        "ORACLE: the child reported {reported:?} for a working directory passed \
         as {}; the verbatim prefix did not survive into the child. \
         stdout={:?} stderr={:?}",
        root.display(),
        out.stdout,
        out.stderr
    );
}
```

- [ ] **Step 2: Run the new tests and confirm they pass on macOS**

```bash
cargo test -p paigasus-helikon-tools --test exec_cwd
```

Expected: `2 passed` (the oracle is `#[cfg(windows)]` and does not compile here). If either fails, the bug is in the test, not in Windows — fix it before going further. macOS is the platform that was never in doubt.

- [ ] **Step 3: Correct the stale module doc**

In `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`, replace lines 1-6 (the `//!` block ending `keep it that way.`) with:

```rust
//! Exec tests that spawn a **real** child through `spawn_capped` and are NOT
//! `cfg`-gated. Two sibling files are the same shape — `tests/exec_env_defaults.rs`
//! (SMA-614) and `tests/exec_cwd.rs` (SMA-615). The unix-only ones are
//! `tests/host_backend.rs` and `tests/exec_env_non_unicode.rs`, both file-level
//! `#![cfg(unix)]`; `tests/exec_backend.rs` is ungated but drives a `MockBackend`
//! and never spawns anything. These three ungated files must compile and pass on
//! Windows too — keep them that way.
```

- [ ] **Step 4: Run the full crate suite**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-tools
```

Expected: all green. `clippy` must be clean — it is the gate that will not see the Windows arms.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/tests/exec_cwd.rs \
        crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs
git commit -m "test(tools): SMA-615 assert host backend cwd pinning on every platform"
```

---

### Task 2: Open the draft PR and read the Windows verdict

**Files:** none — this task's deliverable is a *verdict* recorded on the PR.

**Interfaces:**

- Consumes: Task 1's commit.
- Produces: the world (W1, W2, or W3) that selects Task 3A or Task 3B.

- [ ] **Step 1: Push the branch**

```bash
git push -u origin feature/sma-615-hostbackend-may-not-actually-pin-cwd-on-windows-sandboxopen
```

- [ ] **Step 2: Open the PR as a draft**

```bash
gh pr create --draft \
  --title "test(tools): SMA-615 assert host backend cwd pinning on every platform" \
  --body "$(cat <<'EOF'
Round 1 of SMA-615. **Draft on purpose: the Windows gate is expected to be red, and that is the experiment.**

`Sandbox::open` canonicalizes its root, which on Windows yields a verbatim `\\?\C:\...` path. `cmd.exe` may treat a working directory starting `\\` as a UNC path, print "UNC paths are not supported", and reset to `%SystemRoot%` — in which case `HostBackend` does not pin the working directory on Windows at all. The claim could not be reproduced or refuted from the arm64 macOS dev host, and no existing test covers it: every real-process test that depends on cwd is `#![cfg(unix)]`.

This commit adds the ungated test that settles it on `test (windows-latest, stable)`. Round 2 will push either the fix or the finding, and mark this ready.

| World | contract test | oracle test | Meaning |
| -- | -- | -- | -- |
| W1 | red | red | confirmed — cwd is not pinned |
| W2 | green | red | refuted — `CreateProcessW` normalized the prefix |
| W3 | green | green | refuted — `cmd.exe` tolerated the verbatim path |

Spec: `docs/superpowers/specs/2026-09-05-windows-cwd-pin-design.md`

Closes SMA-615
EOF
)"
```

- [ ] **Step 3: Wait for the Windows leg, touching nothing**

Do not push, amend, or rebase while this runs — `cancel-in-progress` would kill the experiment.

```bash
gh pr checks --watch
```

- [ ] **Step 4: Read the verdict**

```bash
SHA=$(git rev-parse HEAD)
gh api "repos/SMK1085/paigasus-helikon/commits/$SHA/check-runs" \
  --jq '.check_runs[] | select(.name | startswith("test (windows")) | {name, conclusion, id}'
```

If the Windows leg is red, read the actual failure text — never infer from the conclusion alone:

```bash
gh run view <run-id> --log-failed | grep -B5 -A25 'exec_cwd'
```

- [ ] **Step 5: Classify and record**

Match what you read against the table. A red leg whose failure is in neither `host_backend_pins_cwd_to_the_sandbox_root` nor `windows_child_reports_a_verbatim_cwd` is an unrelated flake — re-run the job, do not treat it as a verdict.

Post the raw observed path as a PR comment so the finding survives the branch:

```bash
gh pr comment --body "Round 1 verdict: **W<N>**. The child reported \`<observed path>\`. Proceeding with <Task 3A: the fix | Task 3B: the finding>."
```

**Note (spec Constraint 2):** `cargo test` has no `--no-fail-fast`, so a red `exec_cwd` aborts the remaining Windows test binaries. That is expected here and is why round 2 re-runs the full suite.

---

## Branch A — the suspicion was CONFIRMED (W1)

Only run Tasks 3A–5A if Task 2 landed on W1. Otherwise skip to Task 3B.

### Task 3A: Strip the verbatim prefix in `Sandbox::open`

**Files:**

- Modify: `Cargo.toml` (root, `[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon-tools/Cargo.toml` (`[target.'cfg(windows)'.dependencies]`)
- Modify: `crates/paigasus-helikon-tools/src/sandbox.rs:35-38` and the `root()` rustdoc at `:46`
- Modify: `crates/paigasus-helikon-tools/tests/sandbox.rs:12-17`
- Modify: `crates/paigasus-helikon-tools/tests/exec_cwd.rs` (the oracle)
- Modify: `Cargo.lock` (regenerated, committed)

**Interfaces:**

- Consumes: Task 2's verdict (W1) and Task 1's `run_in_sandbox` / `reported_cwd` helpers.
- Produces: `Sandbox::root()` returns a non-verbatim path on Windows whenever a safe traditional spelling exists. Task 4A's warning detects the case where it does not.

- [ ] **Step 1: Update the oracle test to the observed truth**

In `crates/paigasus-helikon-tools/tests/exec_cwd.rs`, replace the whole `windows_child_reports_a_verbatim_cwd` test (doc comment included) with the characterization guard below. In W1 the child never received a usable working directory at all, so the fact worth pinning is that the prefix reaches the child unchanged — which is exactly what the fix must stop:

```rust
/// Characterization guard (SMA-615, round 2). Round 1 observed **W1** on
/// `test (windows-latest, stable)`: handed a verbatim `\\?\C:\...` working
/// directory, `cmd.exe` rejected it as a UNC path and reset to `%SystemRoot%`.
///
/// After the fix, `Sandbox::root()` no longer carries the verbatim prefix for a
/// root that has a valid traditional spelling — which every `tempfile` root does
/// — so the child must now report a non-verbatim path. This goes red if the
/// strip is ever removed, or if a future Rust or Windows release changes the
/// behaviour underneath us.
#[tokio::test]
#[cfg(windows)]
async fn windows_child_reports_a_non_verbatim_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, root) = run_in_sandbox(tmp.path(), PRINT_CWD).await;
    let reported = reported_cwd(&out);

    assert!(
        !reported.starts_with(r"\\?\"),
        "the child reported the verbatim path {reported:?} for a working \
         directory passed as {}; the SMA-615 strip did not happen. \
         stdout={:?} stderr={:?}",
        root.display(),
        out.stdout,
        out.stderr
    );
}
```

- [ ] **Step 2: Add the `dunce` pin to the workspace**

In the root `Cargo.toml`, inside `[workspace.dependencies]`, add this line in the same aligned style as its neighbours (see `cap-std = "4"` at `:22`):

```toml
dunce                 = "1"
```

A bare `"1"` — not `"*"`, which `deny.toml:102`'s `wildcards = "deny"` rejects.

- [ ] **Step 3: Add it to the tools crate, Windows-only**

In `crates/paigasus-helikon-tools/Cargo.toml`, extend the existing Windows target block so it reads:

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { workspace = true }
dunce       = { workspace = true }
```

Windows-only on purpose: `dunce` is the identity function off Windows, and a
default-features consumer of this crate does not otherwise pull it (the workspace
only has it today as a transitive build-dependency of `aws-lc-sys`, via rustls).

- [ ] **Step 4: Run the test to verify it fails**

```bash
cargo test -p paigasus-helikon-tools --test sandbox
```

Expected on macOS: **PASS** — this step cannot fail here, because the strip is Windows-only and `dunce::canonicalize` is `fs::canonicalize` on unix. The genuine red-to-green transition for this task happened on the Windows gate in Task 2 and is re-verified in Task 6. Record that explicitly rather than pretending to a local TDD cycle the platform cannot provide.

- [ ] **Step 5: Apply the strip in `Sandbox::open`**

In `crates/paigasus-helikon-tools/src/sandbox.rs`, replace:

```rust
        let canonical = root.canonicalize().map_err(|source| SandboxError::Open {
            path: root.to_path_buf(),
            source,
        })?;
```

with:

```rust
        // On Windows `std::fs::canonicalize` returns a VERBATIM path
        // (`\\?\C:\...`), which `cmd.exe` mistakes for a UNC path: it prints
        // "UNC paths are not supported. Defaulting to Windows directory" and
        // resets its working directory to `%SystemRoot%`, so nothing is pinned
        // (SMA-615, confirmed on `test (windows-latest, stable)`).
        // `dunce::canonicalize` is `fs::canonicalize` followed by a prefix strip
        // applied only when the result has a valid traditional spelling; see
        // `Sandbox::root` for the cases where it does not, and
        // `HostBackendBuilder::build` for the warning they raise.
        #[cfg(windows)]
        let canonical = dunce::canonicalize(root);
        #[cfg(not(windows))]
        let canonical = root.canonicalize();
        let canonical = canonical.map_err(|source| SandboxError::Open {
            path: root.to_path_buf(),
            source,
        })?;
```

- [ ] **Step 6: Reword the `root()` rustdoc**

Replace the single doc line on `Sandbox::root` (`/// The canonical sandbox root on the host filesystem (diagnostics / cwd).`) with:

```rust
    /// The sandbox root on the host filesystem, resolved through
    /// `canonicalize` (diagnostics / cwd).
    ///
    /// **Windows:** the verbatim `\\?\` prefix that `canonicalize` produces is
    /// stripped, because `cmd.exe` treats a working directory beginning `\\` as
    /// a UNC path and silently resets to `%SystemRoot%` (SMA-615). The prefix is
    /// *kept* whenever no safe traditional spelling exists — a network share, a
    /// path over 260 characters, a reserved DOS name or otherwise
    /// legacy-invalid component, or a non-Unicode path — and a backend cannot
    /// pin the working directory to such a root.
    /// [`HostBackendBuilder::build`](crate::HostBackendBuilder::build) logs a
    /// warning in that case.
```

- [ ] **Step 7: Fix the `tests/sandbox.rs` assertion the strip invalidates**

Replace `open_succeeds_on_existing_dir` with the following, and add the Windows companion directly after it:

```rust
#[test]
fn open_succeeds_on_existing_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::open(tmp.path()).expect("open sandbox");
    // Compare RESOLVED forms, not spellings: on Windows `Sandbox::open` strips
    // the verbatim `\\?\` prefix (SMA-615), so `root()` no longer equals a bare
    // `canonicalize()` — but it must still name the same directory.
    assert_eq!(
        sandbox.root().canonicalize().unwrap(),
        tmp.path().canonicalize().unwrap()
    );
}

/// The SMA-615 strip actually happened. A `tempfile` root always has a valid
/// traditional spelling, so `root()` must not carry the verbatim prefix. Without
/// this, `open_succeeds_on_existing_dir` above would stay green if the strip were
/// removed, because canonicalizing both sides hides the difference.
#[test]
#[cfg(windows)]
fn open_strips_the_verbatim_prefix_on_windows() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::open(tmp.path()).expect("open sandbox");
    let root = sandbox.root();
    assert!(
        !root.as_os_str().as_encoded_bytes().starts_with(b"\\\\"),
        "root() kept the verbatim prefix: {}",
        root.display()
    );
}
```

- [ ] **Step 8: Verify green locally, with a fresh lockfile**

```bash
cargo build --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-tools
```

If `--locked` fails, run `cargo build --workspace` once to regenerate `Cargo.lock`, then re-run with `--locked`. `Cargo.lock` is committed in this repo.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock crates/paigasus-helikon-tools/Cargo.toml \
        crates/paigasus-helikon-tools/src/sandbox.rs \
        crates/paigasus-helikon-tools/tests/sandbox.rs \
        crates/paigasus-helikon-tools/tests/exec_cwd.rs
git commit -m "fix(tools): SMA-615 strip the verbatim prefix from the sandbox root so windows honours the cwd"
```

---

### Task 4A: Warn when a root has no non-verbatim spelling

**Files:**

- Modify: `crates/paigasus-helikon-tools/src/exec/host.rs:84-100` (inside `build`)
- Modify: `crates/paigasus-helikon-tools/src/exec/host.rs:139-142` (a factually wrong comment)

**Interfaces:**

- Consumes: `Sandbox::root()` from Task 3A, which may still carry `\\?\`.
- Produces: one `tracing::warn!` on target `paigasus::tools::exec` per `HostBackend` constructed over such a root. Nothing consumes it programmatically.

- [ ] **Step 1: Add the warning in `HostBackendBuilder::build`**

In `crates/paigasus-helikon-tools/src/exec/host.rs`, inside `build`, replace:

```rust
        Arc::new(HostBackend {
            cfg: ExecConfig {
                cwd: self.sandbox.root().to_path_buf(),
```

with:

```rust
        let cwd = self.sandbox.root().to_path_buf();
        // A root whose verbatim `\\?\` prefix survived `Sandbox::open` has no
        // safe traditional spelling (network share, over 260 characters,
        // reserved DOS name, non-Unicode). `cmd.exe` rejects such a path as a
        // UNC working directory and resets to `%SystemRoot%`, so this backend
        // cannot honour the cwd contract it advertises. Warn here, where the
        // object making that claim is built — not in `Sandbox::open`, which is
        // shared with the filesystem tools, which do not care about cwd and
        // would make this fire once per request in a server (SMA-615).
        #[cfg(windows)]
        if cwd.as_os_str().as_encoded_bytes().starts_with(b"\\\\") {
            tracing::warn!(
                target: "paigasus::tools::exec",
                cwd = %cwd.display(),
                "sandbox root has no non-verbatim spelling; cmd.exe will not honour \
                 it as a working directory and commands will run from %SystemRoot%"
            );
        }
        Arc::new(HostBackend {
            cfg: ExecConfig {
                cwd,
```

`as_encoded_bytes` is dunce's own documented detection idiom and is stable well below this workspace's 1.94 MSRV.

- [ ] **Step 2: Correct the wrong `-D warnings` comment**

Still in `host.rs`, in `ExecutionBackend::run`, replace the comment block that reads `` // `unused_variables` error under `-D warnings` on the Windows target — `` / `// which CI cannot see, because clippy runs on ubuntu only.` with:

```rust
        // Consumed only by the `#[cfg(unix)]` `pre_exec` hook below; on Windows
        // the closure captures nothing. Without the gate this is an
        // `unused_variables` WARNING on the Windows target — visible in the
        // `test (windows-latest, stable)` log but not fatal there, because that
        // job runs a bare `cargo test` with no `-D warnings` — and invisible to
        // `clippy`, which runs on ubuntu only. Keep the gate: no CI gate would
        // catch its removal (SMA-615).
```

- [ ] **Step 3: Lint the Windows-only code, which no CI gate lints**

```bash
rustup target add x86_64-pc-windows-msvc
cargo clippy -p paigasus-helikon-tools --target x86_64-pc-windows-msvc --all-targets -- -D warnings
```

This is a check-only compile and needs no Windows linker. If it cannot be made to work from this host, say so explicitly on the PR rather than skipping it silently — per the spec, the gap is accepted only when recorded.

- [ ] **Step 4: Verify green locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-tools
```

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/src/exec/host.rs
git commit -m "fix(tools): SMA-615 warn when a windows sandbox root cannot be pinned as a cwd"
```

---

### Task 5A: Bring the documentation into line

**Files:**

- Modify: `crates/paigasus-helikon-tools/src/exec/host.rs:1-5` (module doc)
- Modify: `docs/book/src/concepts/tools.md:379-381`

**Interfaces:**

- Consumes: the behaviour from Tasks 3A and 4A.
- Produces: nothing code-facing.

Per the spec's per-site table, four of the seven "cwd-pinned" claims are deliberately left alone: `src/bash.rs:64` (the model-facing description — a caveat there is prompt bloat the model cannot act on, and making it conditional would need a public API change; tracked as a follow-up), `src/lib.rs:12` and `src/exec/host.rs:107` (true after the fix, and the latter already defers to the module docs), `README.md:7` and `docs/book/src/concepts/tools.md:185` (the roster line and the security disclaimer both stay accurate). **Do not edit those four.**

- [ ] **Step 1: Add the caveat to the `host.rs` module doc**

Replace the module doc block at the top of `crates/paigasus-helikon-tools/src/exec/host.rs` with:

```rust
//! [`HostBackend`] — the default execution backend. A cwd-pinned shell with env
//! scrubbing, an output cap, a timeout (whole-subtree kill), and `rlimit`s.
//! **NOT a security boundary:** a spawned command can read/write anything this
//! process can. Gate it with a `PermissionPolicy` or use [`OsSandboxBackend`]
//! for OS-enforced containment.
//!
//! **One Windows caveat.** The working directory is pinned by handing the
//! sandbox root to `cmd.exe`, which rejects any path beginning `\\` as a UNC
//! path and silently falls back to `%SystemRoot%`. [`Sandbox::root`] therefore
//! strips the verbatim `\\?\` prefix that canonicalization adds — but it can
//! only do so when a safe traditional spelling exists. For a network share, a
//! path over 260 characters, a reserved DOS name, or a non-Unicode path there is
//! none, and the working directory is **not** pinned; [`HostBackendBuilder::build`]
//! logs a warning on the `paigasus::tools::exec` target when that happens
//! (SMA-615).
```

- [ ] **Step 2: Add the caveat to the book**

In `docs/book/src/concepts/tools.md`, immediately after the paragraph ending `same OS access as the parent process.` (line ~381), insert a blank line and then:

```markdown
> **Windows:** pinning works by handing the sandbox root to `cmd.exe`, which
> rejects a path beginning `\\` as a UNC path and falls back to `%SystemRoot%`.
> `Sandbox::root()` strips the verbatim `\\?\` prefix that canonicalization adds
> so that ordinary roots pin correctly. For a root with no safe traditional
> spelling — a network share, a path over 260 characters, a reserved DOS name, or
> a non-Unicode path — the working directory is **not** pinned, and
> `HostBackend::builder(...).build()` logs a warning on the
> `paigasus::tools::exec` target.
```

- [ ] **Step 3: Verify the docs gates**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
npx markdownlint-cli2
mdbook build docs/book
```

All three must be clean. `mdbook` is configured with `[output.linkcheck] warning-policy = "error"`.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-tools/src/exec/host.rs docs/book/src/concepts/tools.md
git commit -m "docs(tools): SMA-615 document the windows cwd pinning caveat"
```

---

## Branch B — the suspicion was REFUTED (W2 or W3)

Only run Task 3B if Task 2 landed on W2 or W3. Otherwise it has already been handled by Tasks 3A–5A.

### Task 3B: Record the finding

**Files:**

- Modify: `crates/paigasus-helikon-tools/src/sandbox.rs:35` (comment only)
- Modify: `crates/paigasus-helikon-tools/tests/exec_cwd.rs` (the oracle)

**Interfaces:**

- Consumes: Task 2's verdict (W2 or W3) and Task 1's helpers.
- Produces: nothing. No behaviour, no API, and no dependency changes on this branch.

- [ ] **Step 1: Rewrite the oracle to the observed truth**

Replace `windows_child_reports_a_verbatim_cwd` (doc comment included) in `crates/paigasus-helikon-tools/tests/exec_cwd.rs`.

**If round 1 observed W3** (the child reported `\\?\C:\...`), keep the assertion as-is and replace only the doc comment with:

```rust
/// Characterization guard (SMA-615). Round 1 observed **W3** on
/// `test (windows-latest, stable)`: the child received the verbatim
/// `\\?\C:\...` working directory unchanged and `cmd.exe` honoured it — no UNC
/// banner, no `%SystemRoot%` fallback. That is why `Sandbox::open` still stores
/// a plain `canonicalize()` result.
///
/// Pinned so the assumption cannot rot silently: this goes red if a future Rust
/// or Windows release stops passing the prefix through, which would be the
/// signal to revisit stripping it.
```

**If round 1 observed W2** (the child reported a plain `C:\...`), replace the whole test with:

```rust
/// Characterization guard (SMA-615). Round 1 observed **W2** on
/// `test (windows-latest, stable)`: handed a verbatim `\\?\C:\...` working
/// directory, `CreateProcessW` normalized it away, so the child reported a plain
/// `C:\...` path and `cmd.exe` never saw a leading `\\`. The suspicion in the
/// ticket does not hold for that reason — not because `cmd.exe` tolerates
/// verbatim paths, which was never tested.
///
/// Pinned so the assumption cannot rot silently: this goes red if a future Rust
/// or Windows release stops normalizing, at which point the verbatim path would
/// reach `cmd.exe` and the cwd would stop being pinned.
#[tokio::test]
#[cfg(windows)]
async fn windows_child_reports_a_normalized_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, root) = run_in_sandbox(tmp.path(), PRINT_CWD).await;
    let reported = reported_cwd(&out);

    assert!(
        !reported.starts_with(r"\\?\"),
        "the child reported the verbatim path {reported:?} for a working \
         directory passed as {}; CreateProcessW no longer normalizes it, so \
         cmd.exe may stop honouring the sandbox root. stdout={:?} stderr={:?}",
        root.display(),
        out.stdout,
        out.stderr
    );
}
```

- [ ] **Step 2: Record the finding at the canonicalize call**

In `crates/paigasus-helikon-tools/src/sandbox.rs`, insert directly above `let canonical = root.canonicalize()`. Fill `<W2 or W3>`, `<the observed path>`, and the date from Task 2's PR comment — do not paraphrase and do not generalize beyond the runner that was measured:

```rust
        // Deliberately a plain `canonicalize()`, verbatim `\\?\` prefix and all.
        // SMA-615 suspected `cmd.exe` would reject that as a UNC path and reset
        // its working directory to `%SystemRoot%`. Measured on
        // `test (windows-latest, stable)` on <date>: it does not — the child
        // reported `<the observed path>` (<W2 or W3>). `tests/exec_cwd.rs` is the
        // standing evidence and goes red if this changes.
        //
        // Scope: one runner image. `HKCU\Software\Microsoft\Command Processor\
        // DisableUNCCheck` and cmd.exe differences across Windows Server
        // releases can change the answer on other hosts.
```

- [ ] **Step 3: Verify green locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-tools
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-tools/src/sandbox.rs crates/paigasus-helikon-tools/tests/exec_cwd.rs
git commit -m "test(tools): SMA-615 pin the observed windows cwd behaviour"
```

---

### Task 6: Push round 2 and mark the PR ready

**Files:** none.

**Interfaces:**

- Consumes: Tasks 3A–5A or Task 3B.
- Produces: a ready-for-review PR with a green required set.

- [ ] **Step 1: Push**

```bash
git push
```

- [ ] **Step 2: Retitle the PR to the round-2 subject**

The PR title becomes the squashed `main` commit, so it must be the round-2 one — this is what release-plz reads.

Branch A:

```bash
gh pr edit --title "fix(tools): SMA-615 strip the verbatim prefix from the sandbox root so windows honours the cwd"
```

Branch B:

```bash
gh pr edit --title "test(tools): SMA-615 pin host backend cwd on every platform"
```

Both satisfy `pr-title.yml`'s two independent rules: a valid `type(scope):` prefix, and a subject whose first character after `SMA-615 ` is lowercase.

- [ ] **Step 3: Wait for the full required set**

```bash
gh pr checks --watch
```

All fifteen required contexts must be green — in particular `test (windows-latest, stable)`, which now runs the whole workspace suite rather than aborting early.

- [ ] **Step 4: Mark ready for review**

```bash
gh pr ready
```

- [ ] **Step 5: Update the PR body to describe what actually landed**

Replace the round-1 body's "expected to be red" framing with the finding, the world observed, and — on branch A — the behaviour change to `Sandbox::root()` on Windows.

---

## Self-Review

**Spec coverage.** Decision 1 → Task 1 Step 1 (all three tests) and Task 2. Decision 2 → Task 2 (draft, no-push-while-running, Checks API + `--log-failed`). Decision 3 → Task 3A Step 5. Decision 4 → Task 3A Steps 2-3 (Windows-gated). Decision 5 → Task 4A Step 1 (warning at `build`, `as_encoded_bytes` idiom), documented in Task 5A. Decision 6 → Task 3A Step 9, Task 3B Step 4, Task 6 Step 2. Constraint 1 → Task 4A Steps 2-3. Constraint 2 → Task 2 Step 5 note. Constraint 3 → Task 2 Step 3. Constraint 8 → Task 3A Step 7. Documentation table → Task 5A, including the explicit do-not-edit list. Goal 2 → Tasks 1, 3A Step 1, 3B Step 1.

**Deliberate deviation from the TDD template.** Task 3A has no genuine red-then-green cycle, because the failing observation is on a platform this host cannot run. Step 4 says so rather than staging a fake local failure. Task 1's tests are written before any fix, which is the real TDD cycle here — the red is on the Windows gate in Task 2.

**Known accepted gaps** (both recorded in the spec, repeated here so an executor does not "fix" them): Decision 5's degrade path has no test, because a >260-character root needs long-path support that is not guaranteed on the runner; and `src/bash.rs:64` is knowingly left claiming unconditional cwd pinning.

**Type consistency.** `run_in_sandbox` and `reported_cwd` keep the same signatures across Tasks 1, 3A and 3B. The oracle is named `windows_child_reports_a_verbatim_cwd` in Task 1 and is renamed exactly once — to `windows_child_reports_a_non_verbatim_cwd` (Task 3A, W1) or `windows_child_reports_a_normalized_cwd` (Task 3B, W2); the W3 path keeps the original name because the original assertion is what proved true.
