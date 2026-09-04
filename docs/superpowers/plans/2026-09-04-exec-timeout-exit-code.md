# SMA-569 implementation plan — `exec` timeout reports `exit_code: None`

Spec: `docs/superpowers/specs/2026-09-04-exec-timeout-exit-code-design.md`
Branch: `feature/sma-569-exec-timeout-reports-none-exit-code`
PR title: `fix(tools): SMA-569 report exit_code None on every timeout path`

Six tasks. Tasks 1-3 are the fix and its coverage; tasks 4-5 are the required-check
promotion Sven chose at Gate 1; task 6 is the live apply, which is gated on his go-ahead.

---

## Task 1 — discard the child status on the timeout path

**File:** `crates/paigasus-helikon-tools/src/exec/mod.rs` (~line 263)

Replace the grace-period match with a discarding await:

```rust
            // Reap the child (bounded by GRACE) but ignore its status: a killed
            // process has no meaningful exit code. On Windows `start_kill()` is
            // `TerminateProcess`, which assigns a real code, and on unix the child
            // can still win the race to exit normally before our SIGKILL lands --
            // both would otherwise contradict `ExecOutput::exit_code`.
            let _ = tokio::time::timeout(GRACE, child.wait()).await;
            None
```

Do **not** touch the SIGKILL / `start_kill()` block above it, and do **not** remove the
`GRACE` wait — the child must still be reaped, and `bash_timeout_with_background_process_does_not_hang`
depends on that timing budget.

**Verify:** `cargo build -p paigasus-helikon-tools`

## Task 2 — state the invariant on the field, binding implementors

**File:** `crates/paigasus-helikon-tools/src/exec/mod.rs` (~line 86)

```rust
    /// Process exit code. Always `None` when [`ExecOutput::timed_out`] is `true`,
    /// and `None` for a process killed by a signal — a killed process has no
    /// meaningful exit code. Implementors of [`ExecutionBackend`] must uphold
    /// this on every platform.
    pub exit_code: Option<i32>,
```

Per the spec, `ExecOutput::new` is deliberately **not** changed to normalize the field.

**Verify:** `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-tools --all-features --no-deps`
(intra-doc links must resolve).

## Task 3 — portable regression test

**File (new):** `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`

```rust
//! Exec tests that spawn a **real** child through `spawn_capped` and are NOT
//! `cfg`-gated. Every other real-process exec test in this crate is unix-only
//! (`tests/host_backend.rs` is file-level `#![cfg(unix)]`); `tests/exec_backend.rs`
//! is ungated but drives a `MockBackend` and never spawns anything. This file is
//! the one that must compile and pass on Windows too — keep it that way.
#![allow(missing_docs)]

use std::time::Duration;

use paigasus_helikon_tools::{ExecRequest, HostBackend, Sandbox};

/// A command that blocks well past the backend timeout. Per-platform because
/// `spawn_capped` runs `sh -c` on unix and `cmd /C` on Windows.
#[cfg(unix)]
const HANG: &str = "sleep 5";

/// `ping` ships with every Windows install; `-n 5` blocks ~4s, 20x the 200ms
/// timeout. Output is redirected to `NUL` so the grandchild never inherits our
/// stdout/stderr pipe handles: Windows has no process-group kill, so `ping`
/// outlives the `TerminateProcess` of `cmd.exe`, and a surviving pipe *writer*
/// would stall the reader drain past the guard below.
#[cfg(windows)]
const HANG: &str = "ping -n 5 127.0.0.1 >NUL 2>&1";

/// `HostBackend`'s default allowlist is `["PATH", "HOME"]` — unix-shaped. On
/// Windows `HOME` does not exist and `spawn_capped` calls `env_clear()`, so
/// without `SystemRoot` Winsock cannot resolve its provider DLLs and `ping`
/// exits in milliseconds, before the timeout fires.
#[cfg(windows)]
const ENV_ALLOWLIST: &[&str] = &["PATH", "SystemRoot", "windir", "PATHEXT", "TEMP", "TMP"];
#[cfg(unix)]
const ENV_ALLOWLIST: &[&str] = &["PATH", "HOME"];

/// A timed-out run reports `exit_code: None` on every platform, per the contract
/// on `ExecOutput::exit_code`. Regression guard for SMA-569: before the fix the
/// grace-period reap returned the child's code, and on Windows
/// `start_kill()`/`TerminateProcess` makes that a real `Some(1)`.
#[tokio::test]
async fn timeout_reports_no_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(Duration::from_millis(200))
        .env_allowlist(ENV_ALLOWLIST.iter().copied())
        .build();

    // Budget mirrors tests/bash.rs:98-100 — tool timeout + GRACE reap + one GRACE
    // reader drain is ~10.2s worst case; 20s leaves CI headroom without masking a hang.
    let out = tokio::time::timeout(
        Duration::from_secs(20),
        backend.run(ExecRequest::new(HANG)),
    )
    .await
    .expect("run must return promptly, not hang")
    .unwrap();

    assert!(out.timed_out, "the command must outlive the 200ms timeout");
    assert_eq!(
        out.exit_code, None,
        "a process killed by the timeout has no meaningful exit code"
    );
}
```

Confirm `tempfile` is already a dev-dependency of the crate before relying on it.

**Verify:** `cargo test -p paigasus-helikon-tools --all-features --test exec_timeout_portable`
(unix locally; the Windows leg is verified by PR CI).

**Sanity-check the guard is real:** temporarily revert Task 1 and confirm the test still
passes on unix — it should, and that is the point the spec makes about the unix leg being
non-discriminating. The Windows leg is what actually fails pre-fix.

## Task 4 — promote `test (windows-latest, stable)` to required

**File:** `.github/rulesets/main-protection-checks.json`

Add after the `test (macos-latest, stable)` entry:

```json
          { "context": "test (windows-latest, stable)" },
```

Only the `stable` leg; `test (windows-latest, 1.94)` stays signal-only, matching how
`ubuntu` and `macos` are already declared.

**Verify:** `jq empty .github/rulesets/main-protection-checks.json`

## Task 5 — sync the three docs that mirror the required list

The ruleset JSON is canonical, but three places restate it and drift silently otherwise.

| File | Where |
|---|---|
| `CLAUDE.md` | ~line 108 — the required-contexts sentence and its "required because" rationales |
| `CONTRIBUTING.md` | ~line 314 — the repo-configuration table row |
| `docs/runbooks/ci-architecture.md` | the required-check narrative |

Rationale to use verbatim at each site, matching the house pattern: **required because it is
the only gate that exercises the Windows timeout path — `cmd /C` process spawning and a
`TerminateProcess`-based kill, which unlike the unix path has no process-group semantics and
assigns a real exit code.**

**Verify:** `npx markdownlint-cli2` (now runnable — Node 24.20.0 via nvm) must stay at
0 issues, and `mdbook build docs/book` must stay clean if any book page is touched.

## Task 6 — apply the ruleset to GitHub (GATED, not automatic)

`scripts/apply-repo-config.sh` PUTs the ruleset via the GitHub API. This is a **live,
immediately-effective branch-protection change**, so it is not run during implementation.

Sequence: merge-ready PR → observe `test (windows-latest, stable)` green on the PR →
Sven runs (or explicitly authorises) `sh scripts/apply-repo-config.sh`.

Leaving the JSON merged but unapplied would put the declaration and the live config out of
sync, so this step must be done — just not silently, and not by me unprompted.

---

## Full gate replay before the PR

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-tools --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
npx markdownlint-cli2
```

`cargo test --workspace --all-features` is the CI gate; run it if time allows, but the
crate-scoped run is the one that covers this change.

## Out of scope (recorded, not forgotten)

- Windows Job Objects for real subtree kill — follow-up ticket.
- Platform-aware default `env_allowlist` on `HostBackend` — follow-up ticket.
- Any version bump (release-plz owns it).
