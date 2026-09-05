# SMA-613 — Kill the whole process subtree on a Windows exec timeout

**Status:** revised after adversarial challenge
**Date:** 2026-09-05
**Crate:** `paigasus-helikon-tools`
**Linear:** [SMA-613](https://linear.app/smaschek/issue/SMA-613/exec-timeout-does-not-kill-the-process-subtree-on-windows-so)
**Split out of:** SMA-569, where it was recorded as an accepted gap.
**Base:** `bd742ac` (after SMA-614 and the 0.2.18 release merged).

## Problem

`spawn_capped` (`crates/paigasus-helikon-tools/src/exec/mod.rs`) kills the whole
process subtree when a command exceeds its timeout — but only on unix, via a new
process group:

```rust
#[cfg(unix)]
{
    // New process group so a timeout can kill the whole subtree.
    cmd.process_group(0);
}
```

and, on the timeout path, `kill(-pgid, SIGKILL)`.

Windows has no equivalent. The timeout path falls through to `child.start_kill()`,
which is `TerminateProcess` against `cmd.exe` **alone**. Every process `cmd.exe`
spawned keeps running to completion.

After SMA-569 a timed-out Windows run reports `timed_out: true` and
`exit_code: None`, which is correct — and which makes the containment gap *less*
visible, because the reported state now looks clean while the workload is still
running.

### Measured evidence

The portable regression test from SMA-569
(`crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`) uses a 200 ms
backend timeout:

| Platform | Wall time |
| -- | -- |
| macOS (local) | 0.21 s |
| `test (windows-latest, stable)` | 4.06 s |

4.06 s is the lifetime of that test's `ping -n 5 127.0.0.1` hang command. The call
did not return shortly after the 200 ms timeout on Windows; it waited out the
orphaned grandchild inside the reader drain's 5 s grace.

**This evidence also disproves a comment currently in the tree.**
`tests/exec_timeout_portable.rs:18-21` claims the `>NUL` redirect stops the
grandchild inheriting our stdout/stderr pipe handles. It does not: Windows
`CreateProcess` is called with `bInheritHandles = TRUE`, so every *inheritable*
handle in `cmd.exe` — including the pipe write ends std marked inheritable when it
spawned `cmd.exe` — is duplicated into `ping` regardless of where `cmd.exe` points
`ping`'s `hStdOutput`. The 4.06 s figure *is* the drain waiting on that inherited
writer. Correcting that comment is in scope for this PR (see "Documentation").

## Goals

1. A timed-out execution on Windows terminates the whole spawned subtree.
2. A regression test proves a grandchild does not survive the timeout, and cannot
   pass vacuously.
3. No behaviour change on unix. The `process_group(0)` path stays as-is.

## Non-goals

- **No `CREATE_SUSPENDED`.** See Decision 2.
- **No `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.** See Decision 3.
- **No `tracing` dependency.** See Decision 5.
- No change to `OsSandboxBackend` or `ForkdBackend`. Neither spawns a local
  process on Windows: `os_sandbox` is Linux/macOS-gated, `forkd` is a REST client.
- No new public API. `SandboxGuarantees` has axes for filesystem, network and
  syscalls; process-subtree containment is not one of them and this change does
  not add one.

## Constraints discovered

On stable Rust with `tokio::process`. The two "no" rows were **verified by
compiling a probe** against the dev host's stable toolchain (`rustc 1.98.0`,
`--target x86_64-pc-windows-gnu`), not inferred from documentation:

| API | Available on stable 1.98? |
| -- | -- |
| `tokio::process::Command::creation_flags(u32)` | yes (delegates to `std::os::windows::process::CommandExt`) |
| `tokio::process::Child::raw_handle() -> Option<RawHandle>` | yes |
| `std::os::windows::io::OwnedHandle` | yes — and it is `Send + Sync` |
| `std::os::windows::process::ProcThreadAttributeList` (for `PROC_THREAD_ATTRIBUTE_JOB_LIST`) | **no** — `error[E0658]`, [rust#114854](https://github.com/rust-lang/rust/issues/114854) |
| `std::os::windows::process::ChildExt::main_thread_handle()` | **no** — `error[E0658]`, [rust#96723](https://github.com/rust-lang/rust/issues/96723) |

The consequence is that the Microsoft-recommended
["direct and mistake-free"](https://devblogs.microsoft.com/oldnewthing/20230209-00/?p=107812)
route — creating the process *already inside* the job via an attribute list — is
unavailable, and so is undoing a `CREATE_SUSPENDED` through `std`.

## Decisions

### Decision 1 — Job Objects, hand-rolled against `windows-sys`

Build the fix rather than documenting the gap harder. Use `windows-sys` directly,
mirroring how the unix arm talks to `libc` directly.

`windows-sys 0.61.2` is **already in `Cargo.lock`** transitively (tokio, mio and
others), so this adds a dependent edge, not a package. Its `rust-version` is
`1.71`, clearing the workspace MSRV of `1.94`; its licence is MIT/Apache-2.0,
clearing `deny.toml`.

**Rejected: `taskkill /T /F /PID <pid>`.** The zero-`unsafe`, zero-dependency
alternative, and it walks the tree for us. Rejected because it spawns a process on
the kill path (which can itself fail, hang, or be missing from a scrubbed `PATH` —
and `spawn_capped` calls `env_clear()`); because it resolves the tree by parent-PID
at kill time, which is racy under PID reuse once the intermediate is dead; and
because it closes no more of the assignment window than the Job Object does. A
kill mechanism that depends on successfully spawning another process is weaker than
one the kernel already holds a handle to.

### Decision 2 — assign immediately after spawn; accept the residual race

`spawn()`, then `AssignProcessToJobObject` on `child.raw_handle()`, as the first
statement after spawn.

There is a window between `CreateProcessW` returning and the assignment in which
`cmd.exe` is running and could spawn a grandchild that never lands in the job, and
so survives `TerminateJobObject`. In practice the child must still be scheduled
and run the loader before it can spawn anything, so the window is microseconds
against milliseconds — but it is not zero, and Microsoft warns against exactly
this ordering.

Closing it would require `CREATE_SUSPENDED` plus resuming the child's initial
thread through a `CreateToolhelp32Snapshot` walk (since `main_thread_handle` is
nightly-only, verified above), roughly doubling the `unsafe` surface and adding a
failure mode where a child can be left permanently suspended.

**Accepted gap**, recorded in the repo's existing idiom (see
`crates/paigasus-helikon-tools/src/exec/forkd.rs:554-558`): a grandchild spawned
in the microseconds before assignment escapes the job. This must be stated in the
**rustdoc**, not only in the book, so the API doc does not over-promise. Revisit if
`windows_process_extensions_raw_attribute` stabilises, which removes the trade-off
entirely.

### Decision 3 — terminate on timeout only; mirror unix

Create the job with **no limit flags**, and call `TerminateJobObject` only on the
timeout path — exactly where unix sends `SIGKILL` to the process group.

| Path | unix (today) | Windows (after this change) |
| -- | -- | -- |
| timeout | `kill(-pgid, SIGKILL)` — subtree dies | `TerminateJobObject` — subtree dies |
| normal completion | process group untouched; survivors live | job handle closes; survivors live |
| `run()` future dropped | process group untouched; survivors live | job handle closes; survivors live |

Setting `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` was considered and rejected. It would
reap survivors of a *normally completed* run when the handle drops, which unix does
not do — a silent asymmetry — and it would not improve latency, because the reader
drain has already spent its grace by then. Dropping the flag also removes
`SetInformationJobObject` and `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` from the FFI
surface entirely.

**On future cancellation** (third row): if a caller drops the `run()` future
between assign and timeout, `JobObject` drops, `CloseHandle` runs, and an anonymous
job can never be reopened — the subtree becomes unkillable by this process. That is
**parity, not a regression**: tokio's `Child` defaults to `kill_on_drop(false)`, so
dropping the future on unix likewise orphans the process group with no one left to
signal it. Both platforms leak the subtree on cancellation today, and this change
neither improves nor worsens that. Out of scope; worth its own ticket if it ever
matters.

### Decision 4 — degrade on failure, never fail the run

If **any** of the three Win32 calls fails, fall back to `child.start_kill()`, which
is exactly today's behaviour. Strictly no worse than the status quo. A job-object
hiccup must not turn a working `run()` into an error.

This explicitly includes **`TerminateJobObject` failing**, which the first draft of
this spec missed. `terminate()` returns `bool`; on `false` the timeout arm falls
back to `start_kill()`. Without that fallback a failed terminate would kill
*nothing* — not even `cmd.exe`, which `start_kill()` always kills today — leaving
the run to burn the full 5 s `GRACE` reap and return with a live subtree. That
would be strictly *worse* than the status quo, contradicting this decision's own
invariant.

### Decision 5 — no `tracing`; the degrade path is silent

`paigasus-helikon-tools` has no `tracing` dependency today. Adding one to log a
containment downgrade is not a one-liner in this repo: `tests/workspace-lints/`
requires every span/event to carry an explicit `target: "paigasus::tools::<subsystem>"`
literal (`tracing_target_coverage.rs`) *and* a matching row in the mdBook component
table (`tracing_target_docs.rs`).

Decision: **do not add it here.** The degrade path is Windows-only and expected to
be unreachable on supported runners; the fallback is exactly today's behaviour, so
a silent degrade returns the system to its current, documented state rather than to
an unknown one. Recorded as a known limitation rather than an omission.

This is the one decision in this spec taken on scope grounds rather than on
correctness grounds, and is the natural candidate to overturn if observability of
containment matters more than PR size.

## Design

### `src/exec/job_object.rs` (new, `#[cfg(windows)]`)

One RAII type over an anonymous job handle:

```rust
/// Owns the *job* handle (never the process handle).
pub(crate) struct JobObject(OwnedHandle);

impl JobObject {
    /// Create an anonymous job object and assign `process` to it.
    /// `process` is **borrowed** from tokio and must never be closed here.
    pub(crate) fn assign(process: RawHandle) -> std::io::Result<Self>;

    /// Terminate every process in the job. `false` if the call failed.
    pub(crate) fn terminate(&self) -> bool;
}
```

`OwnedHandle` replaces the first draft's hand-written `Drop` *and* its
`unsafe impl Send`. Verified by compiling a probe: `OwnedHandle` is `Send + Sync`,
so `struct JobObject(OwnedHandle)` is auto-`Send` with no `unsafe impl`, and
`OwnedHandle`'s own `Drop` calls `CloseHandle`. The `Send` bound is genuinely
required — `spawn_capped`'s future holds the binding across `.await` and
`ExecutionBackend::run` is `#[async_trait]`, which `Send`-bounds it — so getting it
for free from std rather than by hand-audited assertion is a real reduction in
`unsafe` surface. The only remaining `unsafe` is the three FFI calls plus one
`OwnedHandle::from_raw_handle`.

**Construction order matters.** Wrap the handle in `OwnedHandle` *immediately*
after `CreateJobObjectW` succeeds, and only then attempt the assign. The first
draft created the job, assigned, and constructed `Self` last — so a failing assign
returned `Err` having never built the RAII wrapper, leaking the job handle once per
failing run. Since Decision 4 expects assignment failure to be the *reachable*
error path in a long-lived agent process, that leak was on the live path.

Four imported symbols: `CreateJobObjectW`, `AssignProcessToJobObject`,
`TerminateJobObject` (`Win32_System_JobObjects`) and `CloseHandle`
(`Win32_Foundation`, used only via `OwnedHandle`'s `Drop`).

Win32 conventions to honour:

- `CreateJobObjectW` returns **`NULL`** on failure, not `INVALID_HANDLE_VALUE`.
- Both `BOOL`-returning calls report failure as `0`.
- `TerminateJobObject(hjob, uexitcode)` takes an exit code. Pass `1`. It becomes
  every member's exit code and is harmless only because the timeout arm hard-codes
  `exit_code: None` regardless.
- Pass `NULL` for `lpJobAttributes`. That is what leaves `bInheritHandle = FALSE`;
  an inheritable job handle would leak into every subsequent spawn, because std
  spawns with `bInheritHandles = TRUE`.
- Every failure surfaces as `std::io::Error::last_os_error()`.

### `src/exec/mod.rs`

Module wiring — both lines must be `cfg`-gated, since an ungated `use` of a
Windows-only module is a hard error elsewhere:

```rust
#[cfg(windows)]
mod job_object;
#[cfg(windows)]
use job_object::JobObject;
```

Immediately after spawn, parallel to the existing unix binding:

```rust
let mut child = cmd.spawn()?;

#[cfg(unix)]
let pgid = child.id();
#[cfg(windows)]
let job = child.raw_handle().and_then(|h| JobObject::assign(h).ok());
```

Assignment comes before the `stdout`/`stderr` pipes are taken, so the race window
of Decision 2 stays as small as the API allows. `raw_handle()` returns `None` once
the child has exited; a child that exited between `spawn()` and the very next
statement is a legitimate (if vanishing) case, and it degrades per Decision 4.

The timeout arm becomes three-way, replacing today's `unix` / `not(unix)` split:

```rust
#[cfg(windows)]
{
    // Decision 4: any Win32 failure, including a failed terminate, degrades
    // to exactly today's behaviour rather than killing nothing.
    match &job {
        Some(j) if j.terminate() => {}
        _ => { let _ = child.start_kill(); }
    }
}
#[cfg(not(any(unix, windows)))]
{
    let _ = child.start_kill();
}
```

The `not(any(unix, windows))` arm preserves today's fallback for any other target.

The existing grace-period reap and its comment are unchanged: a killed process
still has no meaningful exit code, so `exit_code` stays `None`. `TerminateJobObject`
does not change that — the reap's result is discarded either way.

### Fix: pre-existing `-D warnings` failure in the Windows build

Making cross-target clippy a documented gate (see "Local verification") surfaced a
warning that has been latent on `main`:

```text
error: unused variable: `limits`
  --> crates/paigasus-helikon-tools/src/exec/host.rs:132:13
```

`let limits = self.limits.clone();` is consumed only inside the `#[cfg(unix)]`
block of the closure below it, so on Windows it is unused. CI cannot see this:
`clippy` runs on `ubuntu-latest` only (`ci.yml:43-44`), and the Windows matrix leg
runs `cargo test`, which does not apply `-D warnings`. Fix by `cfg`-gating the
binding:

```rust
#[cfg(unix)]
let limits = self.limits.clone();
```

In scope because the new verification step is unusable while it fails.

### Manifest

Root `Cargo.toml`:

```toml
[workspace.dependencies]
windows-sys = { version = "0.61", features = [
  "Win32_Foundation",
  "Win32_Security",          # CreateJobObjectW is #[cfg(feature = "Win32_Security")]
  "Win32_System_JobObjects",
] }
```

`crates/paigasus-helikon-tools/Cargo.toml`:

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { workspace = true }
```

`Win32_Security` is the non-obvious one and is genuinely required:
`CreateJobObjectW` is gated `#[cfg(feature = "Win32_Security")]`
(`windows-sys-0.61.2/src/Windows/Win32/System/JobObjects/mod.rs:4-5`) because its
signature names `SECURITY_ATTRIBUTES`. Without it the crate does not compile on
Windows.

`Win32_Foundation` is, by contrast, **redundant** — `Win32_System_JobObjects →
Win32_System → Win32 → Win32_Foundation` (`windows-sys-0.61.2/Cargo.toml:216, 178,
56). The first draft justified listing it with a claim that was simply wrong. It is
listed anyway, deliberately, because the code names `Win32_Foundation` types
directly and an explicit entry documents that; the rationale is self-documentation,
not necessity.

The feature list in `[workspace.dependencies]` is **minimal-for-tools**, not a
pre-emptive union. Cargo unifies features across the graph, so a future consumer
needing more adds them here and every consumer gets them; that is the accepted cost
of CLAUDE.md making this table the single source of truth.

**Side effects to expect.** A new dependency *edge* rewrites `Cargo.lock`, which
must be committed. And touching root `Cargo.toml` matches the `sessions-it` path
filter (`ci.yml:208`, `Cargo.toml`/`Cargo.lock`), so that required job will spin up
Postgres and Redis for a Windows-only change. Expected, not a fault.

## Testing

### New: `timeout_kills_the_whole_subtree`

Portable, added to `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`
(the one real-process exec test file that is not `cfg`-gated).

**Two sentinels, not one.** A test that only asserts "the `alive` sentinel is
absent" passes for free whenever the grandchild never launched at all — wrong cwd,
`.cmd` misparsed, `sh` not found, script not written. On a Windows-only path whose
sole behavioural gate is one CI job, that false green would ship the containment
gap believed-fixed. So the grandchild writes `started` **immediately** and `alive`
only after its delay, and the test asserts:

- `started` **exists** — positive control: the grandchild really ran.
- `alive` **never appears** — the actual guard: it was killed before its delay
  elapsed.

**No `start /B`.** `cmd.exe`'s `START` is the documented case for
`CREATE_BREAKAWAY_FROM_JOB`, and a job created without `JOB_OBJECT_LIMIT_BREAKAWAY_OK`
(Decision 3: no limit flags) is exactly the configuration that interacts with it —
plus `START`'s first-quoted-token-is-a-window-title rule sits badly behind Rust's
own `cmd /C` quoting. Use a plain nested `cmd`: `build_command` turns the command
string into `cmd /C "cmd /C grandchild.cmd"`, so the outer `cmd.exe` spawns an inner
`cmd.exe` that runs the batch and waits. `TerminateProcess` on the outer leaves the
inner alive — verified today by SMA-569, where `ping` survives as a plain child —
so the grandchild-survives property is unchanged, with no `START` involved.

| | command | script |
| -- | -- | -- |
| unix | `sh grandchild.sh; true` | `echo started > started`; `sleep 4`; `echo alive > alive` |
| Windows | `cmd /C grandchild.cmd` | `echo started>started`; `ping -n 5 127.0.0.1 >NUL`; `echo alive>alive` |

The unix `; true` is load-bearing: without it `sh -c` applies its single-command
`exec` optimisation and replaces the outer shell, collapsing the tree so the
sentinel-writer is a *child*, not a grandchild. With it, both platforms are a
genuine two-level tree and the test is a real two-level guard on both.

**Timings**, chosen so nothing is a wall-clock coin-flip:

| Quantity | Value | Why |
| -- | -- | -- |
| backend timeout | 1 s | Not 200 ms. The `started` positive control must be written before the kill; 1 s is generous margin for process launch on a loaded runner. |
| grandchild delay | 4 s | Comfortably beyond the 1 s timeout. |
| wait after `run()` returns | 6 s | Past the moment `alive` would have appeared. |
| outer guard | 30 s | Failure case is ~6 s (1 s timeout + 5 s drain `GRACE`) + 6 s ≈ 12 s. |

The failure-case budget is derived from the *corrected* pipe-inheritance fact
above: if the fix regresses, the surviving grandchild holds the inherited pipe write
end, `join_reader` spends its full 5 s `GRACE` (`mod.rs:321`), and `run()` returns
at ~6 s — **not** ~1 s. Sizing the guard off the first draft's incorrect
"redirected to `NUL`, so no influence on the drain" claim would have produced a
budget that fails the very case it exists to catch.

No env allowlist is needed: as of SMA-614 the Windows `DEFAULT_ENV_ALLOWLIST` is
`PATH`, `SystemRoot`, `PATHEXT`, `TEMP`, `TMP`, `USERPROFILE`, `APPDATA`,
`LOCALAPPDATA`, and SMA-614's own `windows_default_env_runs_a_networked_command`
proves `ping` works under it on `windows-latest`.

Other mechanics:

- The `.cmd` body is written with **CRLF** line endings explicitly. Nothing in
  `.gitattributes` covers a file created at runtime, and `cmd.exe` batch parsing is
  the wrong place to discover an LF assumption.
- Sentinels are asserted at `tmp.path().join(...)`. `ExecConfig::cwd` comes from
  `Sandbox::root()`, and on macOS `/var` → `/private/var`; `Path::exists()` follows
  symlinks so the comparison holds, but the implementer should confirm rather than
  assume.
- On unix this newly guards the `process_group(0)` kill, which has no test of its
  own today.

**The implementer must observe the new test fail against unpatched code before
landing it** (stash the `mod.rs` change, run the test on Windows, see it red). A
regression test never seen red is a hypothesis, not a guard — and on this path CI
is the only place it can be observed at all.

### Secondary signal (observed, not asserted)

`timeout_reports_no_exit_code`'s Windows wall time should fall from ~4.06 s to
roughly the unix figure, because terminating the job kills `ping`, which closes the
inherited pipe write end, which lets the reader drain return immediately instead of
waiting out the grandchild. Deliberately **not** asserted — a wall-clock bound on a
CI runner is a flake source.

### Local verification

`x86_64-pc-windows-gnu` is an installed target on the dev host. **Verified to
actually run** (the whole dev-dependency graph, `rcgen` and `wiremock` included,
checks for that target — `clippy` and `check` do not link, so no mingw toolchain is
needed):

```bash
cargo clippy --target x86_64-pc-windows-gnu -p paigasus-helikon-tools \
  --all-targets -- -D warnings
```

**Clippy, not `check`** — and this is the point. `clippy` runs on `ubuntu-latest`
only (`ci.yml:43-44`), so `-D warnings`, a required gate, structurally cannot reach
`#[cfg(windows)] mod job_object`. The Windows matrix leg runs `cargo test`, which
does not apply it. Without this step the new module gets no lint coverage anywhere,
ever. `--all-targets` matters too: it is what compiles the new *test*, the code with
no other pre-CI feedback loop.

This is a documented pre-PR step, not a suggestion. It cannot *run* the test:
`test (windows-latest, stable)` in CI remains the only execution gate.

## Documentation

- `ExecOutput::timed_out` — state the subtree contract (process group on unix, Job
  Object on Windows) **and Decision 2's accepted race**, matching how
  `ExecOutput::exit_code` already documents its cross-platform guarantee.
- `spawn_capped`'s doc comment — "killing the whole process group on timeout" is
  now true on both platforms but unix-specific in wording; make it platform-explicit.
- `crates/paigasus-helikon-tools/src/exec/host.rs:2` — the module doc says
  "a timeout (process-group kill)" on the **all-platforms** default backend.
- `crates/paigasus-helikon-tools/src/exec/host.rs:35` — public rustdoc on
  `HostBackendBuilder::timeout`: "Wall-clock timeout before the process group is
  killed (default 30s)". Both must stop saying "process group" unconditionally, or
  the crate ships a Windows subtree kill while its own docs describe a unix
  mechanism. (`os_sandbox.rs:59` and `os_sandbox_seatbelt.rs:58` carry the same
  wording but are correctly unix-only — leave them.)
- `tests/exec_timeout_portable.rs:18-21` — the `HANG` comment's claim that `>NUL`
  prevents the grandchild inheriting our pipe handles is **false** (see "Measured
  evidence"). Correct it in this PR; it is the rationale a future reader would
  otherwise reuse.
- `docs/book/src/concepts/tools.md`, `HostBackend` section — add a short paragraph
  on timeout semantics including the accepted gap. The book says nothing today.
- `crates/paigasus-helikon-tools/README.md` — **no edit.** It does not discuss
  timeouts or exec containment, and this change adds no public API and no feature
  flag. Conscious call per CLAUDE.md.
- No manual version bump and no CHANGELOG edit: no `paigasus-helikon-core` API is
  added here, so release-plz handles the bump on merge.

## Risks

- **Nested job objects — with a decided contingency.** If the runner already places
  us in a job, assignment relies on Windows 8+ nested-job support. If
  `test (windows-latest, stable)` shows assignment is blocked, the plan is to
  **revert the PR and reopen SMA-613 with the runner constraint documented** — not
  to weaken the test until it passes. Landing the mechanism behind an `#[ignore]`d
  or probe-gated test would mean shipping an unverified containment claim, which is
  the failure mode this whole spec is built to avoid. Note the first draft ended
  this risk at "the first CI run is the real check", which is an observation, not a
  plan.
- **Windows-only code path with one execution gate.** Cross-target clippy covers
  compilation and lints; behaviour is verified only by
  `test (windows-latest, stable)`.
- **Silent degrade.** Per Decision 5 there is no signal when containment falls back
  to `start_kill()`. Known limitation, taken on scope grounds.
- **SMA-614 overlap — resolved.** SMA-614 merged as `2ede539` and this branch is
  rebased past it. Its `DEFAULT_ENV_ALLOWLIST` work removed the bespoke
  `ENV_ALLOWLIST` from `exec_timeout_portable.rs` and added a `#[cfg(test)] mod
  tests` at the end of `exec/mod.rs`; neither collides with this change, which edits
  the timeout arm. Release PR #235 has since landed as `bd742ac` (tools 0.2.18,
  facade 0.5.19, both published), so `main` is settled and no further rebase is
  pending.
- **`test (windows-latest, stable)` costs ~14 min.** It is the only execution gate
  for this work, and it ran green on both SMA-614 and the release PR under the new
  8-variable Windows default — so a red result on this PR indicts this change, not
  the repo. Budget for slow iteration: every behavioural correction is a full CI
  round trip.
