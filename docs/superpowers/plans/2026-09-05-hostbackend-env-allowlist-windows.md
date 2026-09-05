# HostBackend platform-aware `env_allowlist` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `HostBackend`'s default environment allowlist platform-aware so a
default-configured backend can run an ordinary command on Windows, without widening
the unix default.

**Architecture:** One `pub` platform-`cfg`'d `DEFAULT_ENV_ALLOWLIST` const in
`src/exec/mod.rs` becomes the single source of truth; all three builders collect
from it, and `lib.rs` re-exports it so callers can extend rather than replace it.
Correctness is pinned two ways: an exact-equality unit assertion on the const, and
an end-to-end test that inspects the actual child environment on each platform.

**Tech Stack:** Rust 2021, MSRV 1.94, `tokio` (process + test), `tempfile`.

**Spec:** `docs/superpowers/specs/2026-09-05-hostbackend-env-allowlist-windows-design.md`

## Global Constraints

- **Crate:** all source changes are in `crates/paigasus-helikon-tools`. Run every
  `cargo` command from the repo root.
- **Unix default must not change.** It is exactly `["PATH", "HOME"]` before and
  after. Task 1's assertion is the mechanical guard.
- **Windows default is exactly these 8 names, in this order:** `PATH`,
  `SystemRoot`, `PATHEXT`, `TEMP`, `TMP`, `USERPROFILE`, `APPDATA`,
  `LOCALAPPDATA`. Do **not** add `COMSPEC`, `windir`, `HOME`, `SystemDrive`, or
  `OS` — each was considered and excluded with reasons in the spec.
- **No `cfg(not(any(unix, windows)))` arm.** `build_command` (`src/exec/mod.rs:292`)
  has only two arms; a third here would advertise portability the crate lacks.
- **`missing_docs` is workspace-enforced** (`[lints] workspace = true` in the crate).
  Every new `pub` item needs a `///` doc comment or the `docs` gate fails.
- **Every integration test file opens with `#![allow(missing_docs)]`** — the house
  pattern (`tests/bash.rs:1`, `tests/host_backend.rs:1`,
  `tests/exec_timeout_portable.rs:6`). `clippy --all-targets -D warnings` covers
  test targets.
- **Do not hand-bump any crate version or edit any `CHANGELOG.md`.** release-plz
  owns that; a manual bump deadlocks it (CLAUDE.md).
- **Commit type is `feat(tools):`** with an `SMA-614` prefix on the subject —
  `.versionrc` maps `feat` to a Minor bump, which this PR earns by adding public API.
- **The PR title carries the CHANGELOG entry.** PRs are squash-merged, so the PR
  title becomes the `main` commit subject and is what release-plz puts in
  `CHANGELOG.md` — the commit *bodies* below do not land there. The spec requires
  the env-surface change to be reviewable from the release PR alone, so the PR
  title must say it, e.g.
  `feat(tools): SMA-614 pass 8 env vars by default on windows, not 1`.
  It must also satisfy both `pr-title.yml` rules: a valid `type(scope):` prefix,
  and a lowercase first word after the `SMA-###` prefix.
- Commit message trailer, on every commit:

  ```text
  Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
  ```

## File Structure

| File | Change | Responsibility |
| --- | --- | --- |
| `crates/paigasus-helikon-tools/src/exec/mod.rs` | Modify | Owns `DEFAULT_ENV_ALLOWLIST` (source of truth) + its exact-equality pin; `spawn_capped` reads env losslessly |
| `crates/paigasus-helikon-tools/src/lib.rs` | Modify | Re-exports the const from the private `exec` module |
| `crates/paigasus-helikon-tools/src/exec/host.rs` | Modify | Consumes the const; rustdoc enumerating both platform lists |
| `crates/paigasus-helikon-tools/src/exec/os_sandbox.rs` | Modify | Consumes the const (Linux); rustdoc |
| `crates/paigasus-helikon-tools/src/exec/os_sandbox_seatbelt.rs` | Modify | Consumes the const (macOS); rustdoc |
| `crates/paigasus-helikon-tools/tests/exec_env_defaults.rs` | Create | End-to-end proof that the *default* backend gives a working child env |
| `crates/paigasus-helikon-tools/tests/exec_env_non_unicode.rs` | Create | Unix-only regression guard for the `var_os` change (own process — see Task 2) |
| `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs` | Modify | Drops its bespoke allowlist workaround |
| `docs/book/src/concepts/tools.md` | Modify | Public docs: the `HostBackend` example stops hardcoding `["PATH","HOME"]` |

---

### Task 1: The shared platform-aware const

**Files:**

- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs:39-42` (add const after
  `DEFAULT_MAX_OUTPUT`), and append a `#[cfg(test)] mod tests` at end of file
  (currently 371 lines, no test module exists)
- Modify: `crates/paigasus-helikon-tools/src/exec/host.rs:102`
- Modify: `crates/paigasus-helikon-tools/src/exec/os_sandbox.rs:227`
- Modify: `crates/paigasus-helikon-tools/src/exec/os_sandbox_seatbelt.rs:219`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs:39-42` (the `pub use exec::{...}` block)

**Interfaces:**

- Consumes: nothing.
- Produces: `paigasus_helikon_tools::DEFAULT_ENV_ALLOWLIST`, declared as
  `pub const DEFAULT_ENV_ALLOWLIST: &[&str]` (which elaborates to
  `&'static [&'static str]`). Tasks 3 and 4 depend on this exact path and type.

**Note on intermediate state:** Step 3 adds a doctest that imports the const from
the crate root, but the re-export only lands in Step 6. A full `cargo test` run
between those two steps will fail on that doctest. That is expected — run
`cargo test --lib` (which does not build doctests) until Step 7.

- [ ] **Step 1: Write the failing test**

Append to the very end of `crates/paigasus-helikon-tools/src/exec/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::DEFAULT_ENV_ALLOWLIST;

    /// Exact equality, not `contains`: this is what stops a future change from
    /// quietly adding a credential-bearing name to either platform's default.
    #[test]
    #[cfg(unix)]
    fn unix_default_allowlist_is_unchanged() {
        assert_eq!(
            DEFAULT_ENV_ALLOWLIST,
            ["PATH", "HOME"],
            "SMA-614 must not widen the unix default"
        );
    }

    /// The Windows list must stay the minimum-to-function set the spec argues
    /// for. `COMSPEC`, `windir` and `HOME` were considered and excluded.
    #[test]
    #[cfg(windows)]
    fn windows_default_allowlist_is_the_agreed_set() {
        assert_eq!(
            DEFAULT_ENV_ALLOWLIST,
            [
                "PATH",
                "SystemRoot",
                "PATHEXT",
                "TEMP",
                "TMP",
                "USERPROFILE",
                "APPDATA",
                "LOCALAPPDATA",
            ],
            "SMA-614 pins the Windows default; changing it needs a spec update"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-tools --lib`

Expected: FAIL to compile — `cannot find value DEFAULT_ENV_ALLOWLIST in super`.

- [ ] **Step 3: Add the const**

In `crates/paigasus-helikon-tools/src/exec/mod.rs`, immediately after
`pub const DEFAULT_MAX_OUTPUT: usize = 1 << 20;` (line 42):

```rust
/// Environment variable names a child process receives when the caller does not
/// override the allowlist with [`HostBackend::builder`]'s `env_allowlist`.
///
/// The list is platform-specific, because a minimal-but-working environment is:
///
/// - **unix:** `PATH`, `HOME`
/// - **Windows:** `PATH`, `SystemRoot`, `PATHEXT`, `TEMP`, `TMP`, `USERPROFILE`,
///   `APPDATA`, `LOCALAPPDATA`
///
/// Both lists are spelled out because docs.rs renders only the Linux build, so a
/// Windows reader would otherwise see the unix arm and nothing else.
///
/// `env_allowlist` *replaces* this list rather than extending it. To keep the
/// platform defaults and add your own name:
///
/// ```
/// use paigasus_helikon_tools::DEFAULT_ENV_ALLOWLIST;
///
/// let names: Vec<&str> =
///     DEFAULT_ENV_ALLOWLIST.iter().copied().chain(["MY_VAR"]).collect();
/// assert!(names.contains(&"PATH"));
/// assert!(names.contains(&"MY_VAR"));
/// ```
#[cfg(unix)]
pub const DEFAULT_ENV_ALLOWLIST: &[&str] = &["PATH", "HOME"];

/// Environment variable names a child process receives when the caller does not
/// override the allowlist. See the unix arm for the full per-platform list;
/// on Windows it is `PATH`, `SystemRoot`, `PATHEXT`, `TEMP`, `TMP`,
/// `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`.
#[cfg(windows)]
pub const DEFAULT_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "SystemRoot",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
];
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-tools --lib`

Expected: PASS — `unix_default_allowlist_is_unchanged` runs; the Windows test is
`cfg`-ed out on this host.

- [ ] **Step 5: Wire the three builders to the const**

In `crates/paigasus-helikon-tools/src/exec/host.rs`, replace line 102:

```rust
            env_allowlist: vec!["PATH".to_owned(), "HOME".to_owned()],
```

with:

```rust
            env_allowlist: DEFAULT_ENV_ALLOWLIST.iter().map(|s| (*s).to_owned()).collect(),
```

The `(*s)` deref is **required, not stylistic**: `.iter()` over `&[&str]` yields
`&&str`, and `s.to_owned()` on a `&&str` resolves to `<&str as ToOwned>` and
produces another `&str`, which will not collect into `Vec<String>`. Keep the deref.

Add `DEFAULT_ENV_ALLOWLIST` to the existing `use super::{...}` list at
`host.rs:13-16`.

Make the identical change at `os_sandbox.rs:227` and
`os_sandbox_seatbelt.rs:219`, adding the import to each file's `use super::{...}`
block. Both are unix-gated, so they always see `["PATH", "HOME"]` — their behaviour
is byte-for-byte unchanged.

- [ ] **Step 6: Re-export from the crate root**

In `crates/paigasus-helikon-tools/src/lib.rs`, extend the existing block at
lines 39-42 so it reads:

```rust
pub use exec::{
    ExecOutput, ExecRequest, ExecutionBackend, HostBackend, HostBackendBuilder, Isolation,
    ResourceLimits, SandboxGuarantees, DEFAULT_ENV_ALLOWLIST,
};
```

No facade change is needed: `crates/paigasus-helikon/src/lib.rs:37` re-exports the
whole crate as `pub use paigasus_helikon_tools as tools;`.

- [ ] **Step 7: Verify the crate still builds and the doctest passes**

Run: `cargo test -p paigasus-helikon-tools`

Expected: PASS, including the new doctest on `DEFAULT_ENV_ALLOWLIST`.

Run: `cargo clippy -p paigasus-helikon-tools --all-features --all-targets -- -D warnings`

Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-tools/src/
git commit -m "$(cat <<'EOF'
feat(tools): SMA-614 add platform-aware DEFAULT_ENV_ALLOWLIST

Replaces the ["PATH","HOME"] literal duplicated across all three backend
builders with one platform-cfg'd const, re-exported from the crate root so
callers can extend the default instead of replacing it. On Windows the default
becomes PATH, SystemRoot, PATHEXT, TEMP, TMP, USERPROFILE, APPDATA,
LOCALAPPDATA; the unix default is unchanged and pinned by an exact-equality
assertion.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
EOF
)"
```

---

### Task 2: Read env values losslessly

`spawn_capped` uses `std::env::var`, which silently drops any value that is not
valid Unicode — the same silent-no-op failure class that produced this bug, and
more likely on Windows.

**Files:**

- Create: `crates/paigasus-helikon-tools/tests/exec_env_non_unicode.rs`
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs:223-227`

**Interfaces:**

- Consumes: nothing from Task 1. (Order-independent, but commit after Task 1.)
- Produces: nothing other tasks consume.

**Why this test gets its own file:** it calls `std::env::set_var`, which is not
thread-safe, and `cargo test` runs tests within one binary in parallel. A dedicated
integration-test file is its own process with exactly one test, so there is no
concurrent reader to race. Do **not** move it into `exec_env_defaults.rs`.

- [ ] **Step 1: Write the failing test**

Create `crates/paigasus-helikon-tools/tests/exec_env_non_unicode.rs`:

```rust
//! Regression guard: `spawn_capped` must pass an allowlisted variable through
//! even when its value is not valid Unicode. `std::env::var` returns `Err` for
//! those and the value is silently dropped — the same silent-no-op failure mode
//! that made `HOME` a no-op on Windows (SMA-614).
//!
//! This lives in its own file on purpose: it calls `std::env::set_var`, which is
//! not thread-safe, and one test per binary means no concurrent reader.
#![cfg(unix)]
#![allow(missing_docs)]

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use paigasus_helikon_tools::{ExecRequest, HostBackend, Sandbox};

#[tokio::test]
async fn non_unicode_env_value_reaches_the_child() {
    // 0xFF is not valid UTF-8, so `std::env::var` would return Err here.
    std::env::set_var("SMA614_NON_UNICODE", OsString::from_vec(vec![0xff]));

    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .env_allowlist(["PATH", "SMA614_NON_UNICODE"])
        .build();

    let out = backend
        .run(ExecRequest::new(r#"test -n "$SMA614_NON_UNICODE""#))
        .await
        .unwrap();

    assert_eq!(
        out.exit_code,
        Some(0),
        "a non-Unicode value must not be silently dropped; stderr: {}",
        out.stderr
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-tools --test exec_env_non_unicode`

Expected: FAIL — `exit_code` is `Some(1)`, because `env::var` dropped the value so
`$SMA614_NON_UNICODE` is empty and `test -n ""` exits 1.

- [ ] **Step 3: Switch to `var_os`**

In `crates/paigasus-helikon-tools/src/exec/mod.rs`, replace lines 223-227:

```rust
    for name in &cfg.env_allowlist {
        if let Ok(val) = std::env::var(name) {
            cmd.env(name, val);
        }
    }
```

with:

```rust
    for name in &cfg.env_allowlist {
        // `var_os`, not `var`: a value that is not valid Unicode must be passed
        // through, not silently dropped. `Command::env` takes `AsRef<OsStr>`.
        if let Some(val) = std::env::var_os(name) {
            cmd.env(name, val);
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-tools --test exec_env_non_unicode`

Expected: PASS.

Run: `cargo test -p paigasus-helikon-tools`

Expected: PASS — no other test regresses.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/src/exec/mod.rs \
        crates/paigasus-helikon-tools/tests/exec_env_non_unicode.rs
git commit -m "$(cat <<'EOF'
feat(tools): SMA-614 pass non-Unicode env values through to the child

spawn_capped used std::env::var, which returns Err for a value that is not valid
UTF-8 and dropped it without diagnostic. var_os preserves it, and Command::env
already accepts AsRef<OsStr>.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
EOF
)"
```

---

### Task 3: Prove the default works, and delete the workaround

**Files:**

- Create: `crates/paigasus-helikon-tools/tests/exec_env_defaults.rs`
- Modify: `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs:25-33` (delete
  the two `ENV_ALLOWLIST` consts) and `:48` (delete the `.env_allowlist(...)` call)

**Interfaces:**

- Consumes: `paigasus_helikon_tools::DEFAULT_ENV_ALLOWLIST` from Task 1.
- Produces: nothing other tasks consume.

**Critical context — do not weaken this into a `ping` assertion.** The claim that
`ping.exe` fails Winsock init without `%SystemRoot%` is an *unverified hypothesis*
inherited from SMA-569 (which hedged it as "may fail"). Nobody has ever run `ping`
on `windows-latest` without `SystemRoot`. If the hypothesis is false, a
`ping`-only test passes identically with and without the fix and proves nothing. The
primary Windows assertion below inspects the environment **directly** and is
therefore independent of it.

- [ ] **Step 1: Write the failing test**

Create `crates/paigasus-helikon-tools/tests/exec_env_defaults.rs`:

```rust
//! `HostBackend`'s **default** environment allowlist, asserted end-to-end on
//! every platform (SMA-614). Every backend here is built WITHOUT calling
//! `.env_allowlist()` — that is the whole point.
#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::time::Duration;

use paigasus_helikon_tools::{ExecRequest, HostBackend, Sandbox, DEFAULT_ENV_ALLOWLIST};

/// `sh` exports a few names of its own regardless of what we pass in: dash
/// exports `PWD`; bash-as-sh also exports `SHLVL` and `_`. Asserting a subset
/// keeps the test shell-agnostic across the ubuntu and macOS runners.
#[cfg(unix)]
const SH_INJECTED: &[&str] = &["PWD", "SHLVL", "_"];

/// The default allowlist is the *complete* description of the child environment.
/// Running `env` and checking the exported name set proves both halves of that:
/// nothing allowlisted is missing, and nothing un-allowlisted leaked in.
///
/// This is the mechanical guard for SMA-614's "no widening of the unix default".
#[tokio::test]
#[cfg(unix)]
async fn unix_default_env_is_exactly_the_allowlist() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(Duration::from_secs(10))
        .build();

    let out = backend.run(ExecRequest::new("env")).await.unwrap();
    assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);

    let observed: BTreeSet<&str> = out
        .stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, _)| name)
        .collect();

    let permitted: BTreeSet<&str> = DEFAULT_ENV_ALLOWLIST
        .iter()
        .copied()
        .chain(SH_INJECTED.iter().copied())
        .collect();

    let leaked: Vec<&&str> = observed.difference(&permitted).collect();
    assert!(
        leaked.is_empty(),
        "the child saw names outside the default allowlist: {leaked:?}"
    );
    assert!(
        observed.contains("PATH"),
        "PATH is allowlisted and must reach the child; saw {observed:?}"
    );
}

/// The primary Windows assertion. `cmd` echoes a literal `%NAME%` for a variable
/// it cannot expand, so a missing entry shows up as a `%` in stdout. This fails
/// deterministically without the SMA-614 fix and does not depend on any
/// hypothesis about Winsock.
#[tokio::test]
#[cfg(windows)]
async fn windows_default_env_expands_every_allowlisted_name() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(Duration::from_secs(10))
        .build();

    let out = backend
        .run(ExecRequest::new(
            "echo [%SystemRoot%][%PATHEXT%][%TEMP%][%USERPROFILE%][%APPDATA%][%LOCALAPPDATA%]",
        ))
        .await
        .unwrap();

    assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
    assert!(
        !out.stdout.contains('%'),
        "an unexpanded %NAME% means that variable never reached the child: {}",
        out.stdout.trim()
    );
}

/// SMA-614's acceptance criterion verbatim: "a default-configured `HostBackend`
/// can run an ordinary networked command on Windows without a caller-supplied
/// allowlist."
///
/// Treat this as a **smoke test only**. Its discriminating power is unverified —
/// nobody has confirmed that `ping` actually fails without `%SystemRoot%`, so it
/// may well pass with or without the fix. The real guard is
/// `windows_default_env_expands_every_allowlisted_name` above.
#[tokio::test]
#[cfg(windows)]
async fn windows_default_env_runs_a_networked_command() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(Duration::from_secs(10))
        .build();

    // Outer budget so a regression fails fast instead of stalling the required
    // Windows gate for the full backend timeout. Mirrors exec_timeout_portable.rs.
    let out = tokio::time::timeout(
        Duration::from_secs(30),
        backend.run(ExecRequest::new("ping -n 1 127.0.0.1")),
    )
    .await
    .expect("run must return promptly, not hang")
    .unwrap();

    assert!(!out.timed_out, "a single loopback ping must not time out");
    assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-tools --test exec_env_defaults`

Expected on this macOS host: the unix test **PASSES** — the unix default is
unchanged by design, so this leg is a non-regression guard, not a red-to-green
step. The Windows legs cannot run here; they are `cfg`-ed out.

If `unix_default_env_is_exactly_the_allowlist` fails, Task 1 wired a builder
wrongly. Stop and fix Task 1 before continuing.

- [ ] **Step 3: Delete the workaround from the timeout test**

In `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`, delete both
`ENV_ALLOWLIST` consts and their doc comments (lines 25-33):

```rust
/// `HostBackend`'s default allowlist is `["PATH", "HOME"]` — unix-shaped.
#[cfg(unix)]
const ENV_ALLOWLIST: &[&str] = &["PATH", "HOME"];

/// On Windows `HOME` does not exist, and `spawn_capped` calls `env_clear()`, so
/// without `SystemRoot` Winsock cannot resolve its provider DLLs: `ping` would
/// exit in milliseconds, before the timeout ever fires.
#[cfg(windows)]
const ENV_ALLOWLIST: &[&str] = &["PATH", "SystemRoot", "windir", "PATHEXT", "TEMP", "TMP"];
```

and delete line 48:

```rust
        .env_allowlist(ENV_ALLOWLIST.iter().copied())
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools`

Expected: PASS. `exec_timeout_portable.rs` now uses the default and no longer
declares an unused const.

Run: `cargo clippy -p paigasus-helikon-tools --all-features --all-targets -- -D warnings`

Expected: no warnings (a leftover unused `ENV_ALLOWLIST` would be caught here).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/tests/
git commit -m "$(cat <<'EOF'
feat(tools): SMA-614 assert the default env allowlist end-to-end

Adds tests/exec_env_defaults.rs, which builds the backend without calling
env_allowlist(). The unix leg runs `env` and asserts the exported name set is
exactly the allowlist plus the names sh injects, which is the real guard against
widening the unix default. The Windows leg echoes %NAME% for each entry, since
cmd prints an unexpanded %NAME% when a variable is missing — deterministic, and
independent of the unverified claim that ping fails without %SystemRoot%. A ping
smoke test is kept alongside it, labelled as such.

exec_timeout_portable.rs drops its bespoke allowlist workaround and now exercises
the default, per the ticket's acceptance criteria.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
EOF
)"
```

---

### Task 4: Documentation

Four rustdoc sites hardcode `["PATH","HOME"]` in prose, and the mdBook example
hardcodes it in code. **docs.rs builds only `x86_64-unknown-linux-gnu`**, so a
Windows reader sees the unix arm and nothing else — every corrected comment must
therefore *enumerate both lists literally*, never say "the platform-appropriate
default".

**Files:**

- Modify: `crates/paigasus-helikon-tools/src/exec/host.rs:41` and `:97`
- Modify: `crates/paigasus-helikon-tools/src/exec/os_sandbox.rs:64`
- Modify: `crates/paigasus-helikon-tools/src/exec/os_sandbox_seatbelt.rs:63`
- Modify: `docs/book/src/concepts/tools.md:384-392`

**Interfaces:**

- Consumes: `DEFAULT_ENV_ALLOWLIST` from Task 1.
- Produces: nothing.

- [ ] **Step 1: Correct `host.rs`'s two doc comments**

Replace the `env_allowlist` doc at `host.rs:41`:

```rust
    /// Env var names to pass through (REPLACES the default `["PATH","HOME"]`).
```

with:

```rust
    /// Env var names to pass through to the child.
    ///
    /// This **replaces** [`DEFAULT_ENV_ALLOWLIST`] rather than extending it. On
    /// Windows a list that omits `SystemRoot` will break networked commands, so
    /// prefer extending:
    ///
    /// ```ignore
    /// .env_allowlist(DEFAULT_ENV_ALLOWLIST.iter().copied().chain(["MY_VAR"]))
    /// ```
    ///
    /// A name that is absent from this process's environment is dropped
    /// **without diagnostic** — the child simply never sees it.
    ///
    /// To reproduce the pre-SMA-614 minimal environment, pass `["PATH"]`.
```

Replace the builder doc at `host.rs:97`:

```rust
    /// Start building a `HostBackend` over `sandbox` (cwd = `sandbox.root()`),
    /// with a 30s timeout, `["PATH","HOME"]` env allowlist, 1 MiB output cap.
```

with:

```rust
    /// Start building a `HostBackend` over `sandbox` (cwd = `sandbox.root()`),
    /// with a 30s timeout, a 1 MiB output cap, and the platform default env
    /// allowlist — `["PATH", "HOME"]` on unix, and on Windows `["PATH",
    /// "SystemRoot", "PATHEXT", "TEMP", "TMP", "USERPROFILE", "APPDATA",
    /// "LOCALAPPDATA"]`. See [`DEFAULT_ENV_ALLOWLIST`].
```

Both lists are spelled out because docs.rs renders only the Linux build.

- [ ] **Step 2: Correct the two OS-sandbox doc comments**

At `os_sandbox.rs:64` and `os_sandbox_seatbelt.rs:63`, replace:

```rust
    /// Env var names to pass through (REPLACES the default `["PATH","HOME"]`).
```

with:

```rust
    /// Env var names to pass through to the child.
    ///
    /// This **replaces** [`DEFAULT_ENV_ALLOWLIST`] rather than extending it.
    /// This backend is unix-only, so that default is `["PATH", "HOME"]`.
    ///
    /// A name that is absent from this process's environment is dropped
    /// without diagnostic.
```

- [ ] **Step 3: Update the mdBook example**

In `docs/book/src/concepts/tools.md`, replace the `HostBackend` paragraph and
example (lines 380-392):

````markdown
The default backend. Pins the working directory to the sandbox root and scrubs the
environment to a configurable allowlist, but spawned commands have the same OS
access as the parent process.

```rust,ignore
use paigasus_helikon_tools::{BashTool, HostBackend, Sandbox};

// Uses the platform default env allowlist.
let backend = HostBackend::builder(Sandbox::open("./workspace")?)
    .timeout(std::time::Duration::from_secs(10))
    .build();
let tool = BashTool::<()>::new(backend);
```

The default allowlist is platform-specific, because a minimal-but-working
environment is: `PATH` and `HOME` on unix; `PATH`, `SystemRoot`, `PATHEXT`,
`TEMP`, `TMP`, `USERPROFILE`, `APPDATA` and `LOCALAPPDATA` on Windows. `HOME` does
not exist on Windows, so a unix-shaped list leaves a Windows child with `PATH`
alone — enough to break Winsock initialization, temp-file writes, and `cmd.exe`
extension resolution.

`env_allowlist` **replaces** that default rather than extending it, so keep the
platform names when you add your own:

```rust,ignore
use paigasus_helikon_tools::DEFAULT_ENV_ALLOWLIST;

let backend = HostBackend::builder(Sandbox::open("./workspace")?)
    .env_allowlist(DEFAULT_ENV_ALLOWLIST.iter().copied().chain(["MY_VAR"]))
    .build();
```
````

- [ ] **Step 4: Verify the docs gates**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-tools --all-features --no-deps`

Expected: clean, no warnings. An intra-doc link to `DEFAULT_ENV_ALLOWLIST` that
does not resolve fails here.

Run: `mdbook build docs/book`

Expected: clean. `[output.linkcheck] warning-policy = "error"` means a broken link
is a failure.

Run: `PATH="$HOME/.nvm/versions/node/v24.20.0/bin:$PATH" npx markdownlint-cli2`

Expected: `0 issues`. (The repo's pinned `markdownlint-cli2` needs Node ≥ 20; the
default `node` on this machine is v18 and crashes with a regex-flags `SyntaxError`.)

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/src/ docs/book/src/concepts/tools.md
git commit -m "$(cat <<'EOF'
docs(tools): SMA-614 document the platform-aware env allowlist

Four rustdoc sites hardcoded ["PATH","HOME"] in prose and the mdBook example
hardcoded it in code. Each now enumerates both platform lists literally, because
docs.rs builds only x86_64-unknown-linux-gnu and a Windows reader would otherwise
see the unix arm alone. Also documents that env_allowlist replaces rather than
extends, that an absent name is dropped without diagnostic, and the rollback path
to the pre-SMA-614 minimal environment.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
EOF
)"
```

---

### Task 5: Full local gate sweep

Every required CI context that can run on this macOS host, run before the branch is
pushed. `test (windows-latest, stable)` cannot be reproduced locally — it is the one
gate that must be watched on the PR.

**Files:** none (verification only).

- [ ] **Step 1: Run every reproducible gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
PATH="$HOME/.nvm/versions/node/v24.20.0/bin:$PATH" npx markdownlint-cli2
bash scripts/check-markdownlint-config.sh
mdbook build docs/book
convco check $(git merge-base origin/main HEAD)..HEAD
```

Expected: all pass. `convco check` **must** use a merge-base — given a branch tip it
silently walks the whole history and rejects correct branches (CLAUDE.md).

- [ ] **Step 2: Confirm the diff matches the plan**

Run: `git diff origin/main --stat`

Expected: exactly these files, and **no** `Cargo.toml` or `CHANGELOG.md` among them
— release-plz owns versioning.

```text
crates/paigasus-helikon-tools/src/exec/mod.rs
crates/paigasus-helikon-tools/src/exec/host.rs
crates/paigasus-helikon-tools/src/exec/os_sandbox.rs
crates/paigasus-helikon-tools/src/exec/os_sandbox_seatbelt.rs
crates/paigasus-helikon-tools/src/lib.rs
crates/paigasus-helikon-tools/tests/exec_env_defaults.rs
crates/paigasus-helikon-tools/tests/exec_env_non_unicode.rs
crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs
docs/book/src/concepts/tools.md
docs/superpowers/plans/2026-09-05-hostbackend-env-allowlist-windows.md
docs/superpowers/specs/2026-09-05-hostbackend-env-allowlist-windows-design.md
```

- [ ] **Step 3: Confirm no stray debug code**

Run: `git diff origin/main | grep -nE '^\+.*(dbg!|println!|eprintln!|TODO|FIXME|XXX)'`

Expected: no output.
