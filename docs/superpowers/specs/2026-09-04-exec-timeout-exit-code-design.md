# SMA-569 — `exec` timeout must report `exit_code: None` on every platform

- **Status:** approved at Gate 1 (revised after adversarial challenge)
- **Linear:** [SMA-569](https://linear.app/smaschek/issue/SMA-569/exec-timeout-can-report-someexit-code-on-windows-despite-execoutput)
- **Crate:** `paigasus-helikon-tools`
- **Commit / PR title:** `fix(tools): SMA-569 report exit_code None on every timeout path`
- **Type:** bug fix, pre-existing. User-visible behaviour change on Windows — must
  reach the CHANGELOG, hence `fix(...)` and not `chore(...)`/`refactor(...)`.

All paths below are repo-root-relative.

## Problem

`ExecOutput` documents its own contract at `crates/paigasus-helikon-tools/src/exec/mod.rs:86`:

```rust
/// Process exit code, or `None` if killed by signal / timeout.
pub exit_code: Option<i32>,
```

`spawn_capped` violates it on the timeout path
(`crates/paigasus-helikon-tools/src/exec/mod.rs:246-268`). After the wall-clock timeout
fires and the child is killed, the grace-period reap returns the child's exit code verbatim:

```rust
match tokio::time::timeout(GRACE, child.wait()).await {
    Ok(Ok(status)) => status.code(),   // can be Some(..)
    _ => None,
}
```

`BashTool` surfaces `exit_code` and `timed_out` as sibling JSON fields
(`crates/paigasus-helikon-tools/src/bash.rs:167-168`), so a consumer can observe
`timed_out: true` next to a non-null `exit_code` — self-contradictory against the
documented contract, and a shape a model reading the tool output has to reconcile on its own.

### Two ways to hit it

1. **Windows.** `Child::start_kill()` routes to `TerminateProcess(handle, 1)`, which assigns
   a real exit code, so `ExitStatus::code()` returns `Some(1)` and every timed-out command on
   Windows reports a non-null exit code. Not strictly deterministic — a child that exits of
   its own accord first yields its natural code instead — but the contract is broken either way.
2. **Unix (a race).** The child is SIGKILLed, and `code()` returns `None` for a
   signal-terminated process, which masks the bug in the common case. But if the child exits
   *on its own* in the window between `tokio::time::timeout` expiring and the
   `kill(-pgid, SIGKILL)` landing, `wait()` reports that natural exit and `code()` returns
   `Some(0)`. Rare and timing-dependent — not worth a test — but it means the fix is a
   correctness fix on Unix too, not merely a Windows accommodation.

## Decision

Take the shape the ticket suggests. Await the grace-period wait purely to **reap** the child;
discard its value and yield `None` unconditionally on the timeout path.

```rust
Err(_) => {
    timed_out = true;
    // ... SIGKILL (unix) / start_kill() (windows), unchanged ...
    // Reap the child (and bound the wait) but ignore its status: a killed
    // process has no meaningful exit code, and `ExecOutput::exit_code`
    // documents `None` for the timeout path on every platform.
    let _ = tokio::time::timeout(GRACE, child.wait()).await;
    None
}
```

`.await` binds tighter than `=`, so the discarded value is a `Result`, not a `Future`, and
`clippy::let_underscore_future` cannot fire. The identical `let _ = …await` pattern already
ships three times in this same file (lines 256, 261, 329) and passes the `clippy` gate today.

The `GRACE`-bounded wait is otherwise untouched, so nothing about reaping, pipe draining, or
the `bash_timeout_with_background_process_does_not_hang` worst-case timing changes.

**Diagnostic loss (accepted).** Post-fix, the Unix race case is indistinguishable from a
plain SIGKILL, and nothing records which occurred. `spawn_capped` emits no traces today and
the crate has no `tracing` dependency, so adding one is scope creep. If it ever gains
tracing, the discarded status is the natural `debug!` field.

**Also tighten the doc comment** at `crates/paigasus-helikon-tools/src/exec/mod.rs:86` to
state the invariant as a contract binding on *implementors*, not just a description of what
the built-in backends happen to do:

```rust
/// Process exit code. Always `None` when `timed_out` is `true`, and `None` for a
/// process killed by a signal — a killed process has no meaningful exit code.
/// Implementors of [`ExecutionBackend`] must uphold this on every platform.
pub exit_code: Option<i32>,
```

### Alternatives considered and rejected

- **`#[cfg(windows)] { None }` / `#[cfg(unix)] { status.code() }`.** Preserves a value nobody
  consumes, keeps the Unix race, and adds a platform split to a path that does not need one.
- **Loosen the doc comment instead** (`"…or `None` if killed by signal / timeout on Unix"`).
  Rejected: `timed_out` already carries "was this killed by the timeout" unambiguously; making
  `exit_code` platform-dependent pushes that split onto every consumer, including the model
  reading `BashTool`'s JSON.
- **Report the kill code in a new field.** Out of scope, and `ExecOutput` is `#[non_exhaustive]`
  so it can be added later if a need appears. None exists today.
- **Normalize in `ExecOutput::new` (`if timed_out { exit_code = None }`).** Raised by the
  challenge: `new` is `pub` and accepts any field combination, so a third-party
  `ExecutionBackend` can re-open this bug and `BashTool` will faithfully forward it.
  **Rejected** — silently rewriting a caller's explicit argument is a worse surprise than the
  contract violation it prevents, and it would mask rather than surface a buggy backend. The
  tightened doc comment above is the chosen enforcement. Revisit only if a second producer
  actually violates it.

## Test strategy

The claim in the first draft that "there is no cross-platform exec test anywhere in the crate"
was **wrong**: `crates/paigasus-helikon-tools/tests/exec_backend.rs` carries no `cfg` gate and
already asserts `exit_code`/`timed_out` projection (lines 100-101). The accurate, narrower
claim is: **no cross-platform test spawns a real process through `spawn_capped`.**
`exec_backend.rs` exercises the trait and type surface against an in-process `MockBackend`
(line 45); it never spawns anything.

Add **one portable test** asserting the full contract — `timed_out == true` **and**
`exit_code == None` — through `HostBackend`, the only backend that reaches `spawn_capped` on
Windows. `Sandbox` has no `cfg` gates at all and `exec/host.rs` has exactly one (the rlimit
`pre_exec` at line 115), so `HostBackend` compiles and runs on Windows unmodified.

### Placement

A **new `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`**, with a file-level
`//!` comment stating the boundary. Rejected alternatives: `tests/host_backend.rs` is
file-level `#![cfg(unix)]`; folding into `tests/exec_backend.rs` would change that file's
character from pure mock-driven surface tests to real process spawning. The `_portable`
suffix is load-bearing — `exec_backend.rs` is *also* ungated, so a bare `exec_timeout.rs`
would not signal which file is the portable one.

### Skeleton

```rust
//! Exec tests that spawn a **real** child through `spawn_capped` and are NOT
//! `cfg`-gated. Every other real-process exec test in this crate is Unix-only;
//! this file must compile and pass on Windows too. Keep it that way.
#![allow(missing_docs)]

/// Blocks well past the backend timeout. Per-platform because `spawn_capped`
/// runs `sh -c` on unix and `cmd /C` on Windows.
#[cfg(unix)]
const HANG: &str = "sleep 5";
/// `ping` ships with every Windows install; `-n 5` blocks ~4s, 20x the 200ms
/// timeout. Output goes to `NUL` so the grandchild never inherits our stdout /
/// stderr pipe handles — see "Windows orphan hazard" below.
#[cfg(windows)]
const HANG: &str = "ping -n 5 127.0.0.1 >NUL 2>&1";

#[tokio::test]
async fn timeout_reports_no_exit_code() { /* 200ms backend timeout, 20s outer guard */ }
```

Separate `#[cfg]` `const`s rather than `if cfg!(windows)`, so neither platform compiles a
string it cannot run. Outer `tokio::time::timeout` of 20s, matching the budget reasoning
already written out at `crates/paigasus-helikon-tools/tests/bash.rs:98-100`, so a regression
surfaces as a failure rather than a hung CI job.

### Windows hazards this test must survive

The challenge surfaced three, all real, none previously exercised anywhere in this repo:

1. **`env_clear()` strips `SystemRoot`.** `exec/mod.rs:217` clears the environment and
   `HostBackend`'s default allowlist is `["PATH", "HOME"]` (`exec/host.rs:102`) — a
   Unix-shaped list; `HOME` does not exist on Windows. Winsock resolves provider DLLs via
   `%SystemRoot%`, so `ping.exe` may fail to initialize, exit in milliseconds, and make
   `timed_out` false. **Mitigation:** the test sets an explicit Windows allowlist
   (`["PATH", "SystemRoot", "windir", "PATHEXT", "TEMP", "TMP"]`) via `.env_allowlist(..)`
   rather than relying on the default.
2. **Orphaned grandchild holding our pipes.** `cmd.process_group(0)` is `#[cfg(unix)]`-only
   (`exec/mod.rs:225-229`), so `start_kill()` terminates only `cmd.exe`; `ping.exe` survives.
   A surviving pipe *writer* stalls the reader drain, and on Windows `tokio` reads child
   stdio through the blocking pool, where aborting the task does not cancel an in-flight
   `ReadFile` — the runtime's `Drop` would then block *after* the test body's 20s guard has
   already returned. **Mitigation:** `>NUL 2>&1` inside the `cmd /C` string means `ping`
   never inherits our handles at all; `cmd.exe`'s own handles close when it is terminated.
3. **`cmd /C` argument quoting.** `build_command` does `c.arg("/C").arg(command)`
   (`exec/mod.rs:307-308`); Rust quotes the space-containing arg and `cmd.exe` re-parses it.
   Its rule-2 quote-stripping handles the `>`/`&` redirect fine, but this path has never been
   exercised. **Mitigation:** none needed up front — this is what the PR's Windows CI run
   verifies.

**Fallback if the Windows leg proves red or flaky in PR CI** (decided now, not under
pressure): first try dropping the redirect to a bare `ping -n 5 127.0.0.1` and re-read the
failure; if it still fails, keep the test with `#[cfg_attr(windows, ignore)]` plus a comment
naming the observed failure, and file a follow-up rather than deleting the Windows assertion.

### What CI actually enforces — read this before signing off AC2

Being blunt, because the first draft understated it: **the Unix leg of this test cannot fail
if the fix is reverted.** Pre-fix, the Unix timeout path SIGKILLs and `code()` already returns
`None`. The Unix assertion is a *characterization* test, not a regression test. The only
discriminating leg is Windows — and per CLAUDE.md, `test (windows-latest, stable)` is
**signal-only, not a required context**.

Net: with the scope as written, the fix ships with **zero enforced regression coverage**. A
future refactor of `spawn_capped` can reintroduce this exact bug with every required gate green.

**Decision (Sven, Gate 1): promote `test (windows-latest, stable)` to a required context**
as part of this PR. The gap is closed properly rather than tracked; this PR then gates on the
very check that discriminates the bug it fixes.

Verified safe to promote:

- `test (windows-latest, stable)` is currently **green on `main`** (checked via the Checks
  API), so promotion does not block on a pre-existing failure.
- The `test` job is **not path-filtered** (`.github/workflows/ci.yml:64-97`), so it reports on
  every PR. This matters: `docs/runbooks/ci-architecture.md:11` records that a path-filtered
  *required* check never reports on a PR that touches none of its paths and blocks that PR
  forever — the reason `markdown-lint` is deliberately unfiltered. `test` is already immune.
- Only the `stable` leg is promoted, matching how `ubuntu` and `macos` are already declared.
  `test (windows-latest, 1.94)` stays signal-only.

Cost, stated plainly: every future PR now gates on the slowest runner in the matrix.

### Files this touches

| File | Change |
| --- | --- |
| `.github/rulesets/main-protection-checks.json` | Add `{ "context": "test (windows-latest, stable)" }` |
| `CONTRIBUTING.md` (~line 314) | Add to the required-contexts table with its rationale |
| `CLAUDE.md` (~line 108) | Add to the required-contexts list with its rationale |
| `docs/runbooks/ci-architecture.md` (~line 108) | Bring the required-check narrative into line |

Rationale to record at each site, matching the house pattern ("required because it is the only
gate that…"): **required because it is the only gate that exercises the Windows timeout path —
`cmd /C` process spawning and `TerminateProcess`-based kill, which unlike the Unix path has no
process-group semantics and reports a real exit code.**

**Applying the ruleset is a live repo-settings change.** The JSON is a declaration;
`scripts/apply-repo-config.sh` is what pushes it to GitHub via the rulesets API. That is an
outward-facing, immediately-effective change to branch protection, so it is **not** run as part
of implementation — it is surfaced for Sven to run (or to explicitly authorise) once the PR's
Windows leg is observed green. Merging the JSON without applying it leaves the declaration and
the live config out of sync, so this step must not be silently skipped either.

## Accepted gaps (not fixed here)

- **Windows has no process-subtree kill.** After this change, a timed-out Windows run reports
  "killed by timeout, no meaningful exit code" while grandchildren spawned by `cmd.exe` keep
  running to completion. The fix makes the *reported state* consistent while leaving the
  containment gap untouched — and slightly less visible. The real equivalent of the Unix
  process-group kill is a Job Object (`CreateJobObject` +
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`). Follow-up ticket, not this PR. (The repo already uses
  this "Accepted gap" idiom — see `crates/paigasus-helikon-tools/src/exec/forkd.rs:554-558`.)
- **`HostBackend`'s default `env_allowlist` is Unix-shaped.** `["PATH", "HOME"]`
  (`exec/host.rs:102`) — `HOME` is meaningless on Windows and `SystemRoot`/`PATHEXT` are
  missing. This test is the first thing in the repo to expose it. Making the default
  platform-aware is a real product fix but is scope creep here; the test carries its own
  allowlist. Follow-up ticket.

## Scope

**In:** the `spawn_capped` timeout branch; the `exit_code` doc comment; one new portable
regression test; promoting `test (windows-latest, stable)` to a required context (ruleset JSON
plus the three docs that mirror it).

**Out:** version bumps (release-plz owns them); the other backends (`ForkdBackend` already
returns `None` on timeout at `exec/forkd.rs:576-582`, with
`tests/forkd_backend.rs:106-107` already asserting it); the two accepted gaps below.

**mdBook / crate README — checked, not assumed.** Per CLAUDE.md this must be a conscious call:
neither `exit_code` nor `timed_out` appears anywhere in `docs/book/` or in
`crates/paigasus-helikon-tools/README.md`, and the public API is unchanged (only a doc comment
and an internal code path move). No book or README edit needed.

**Companion plan:** `docs/superpowers/plans/2026-09-04-exec-timeout-exit-code.md`, per CLAUDE.md's
spec/plan pairing convention. (An earlier draft of this spec argued the change was small enough to
skip one; Gate 1 widened the scope to include the required-check promotion, which spans four more
files and a gated live-apply step, so a plan earns its place.)

## Acceptance criteria

- [ ] A timed-out execution reports `exit_code: None` on both Unix and Windows.
- [ ] Regression coverage on both platforms, with the discriminating (Windows) leg promoted to
      a **required** status check so a future refactor cannot silently reintroduce the bug.
- [ ] The child is still reaped, not leaked; the grace-period wait stays, only its return
      value is discarded.
