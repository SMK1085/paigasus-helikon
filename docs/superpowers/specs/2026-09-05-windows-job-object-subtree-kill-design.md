# SMA-613 — Kill the whole process subtree on a Windows exec timeout

**Status:** approved (design)
**Date:** 2026-09-05
**Crate:** `paigasus-helikon-tools`
**Linear:** [SMA-613](https://linear.app/smaschek/issue/SMA-613/exec-timeout-does-not-kill-the-process-subtree-on-windows-so)
**Split out of:** SMA-569, where it was recorded as an accepted gap.

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

## Goals

1. A timed-out execution on Windows terminates the whole spawned subtree.
2. A regression test proves a grandchild does not survive the timeout.
3. No behaviour change on unix. The `process_group(0)` path stays as-is.

## Non-goals

- **No `CREATE_SUSPENDED`.** See "Decision 2".
- **No `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.** See "Decision 3".
- No change to `OsSandboxBackend` or `ForkdBackend`. Neither spawns a local
  process on Windows: `os_sandbox` is Linux/macOS-gated, `forkd` is a REST client.
- No new public API. `SandboxGuarantees` has axes for filesystem, network and
  syscalls; process-subtree containment is not one of them and this change does
  not add one.

## Constraints discovered

On stable Rust with `tokio::process`:

| API | Available? |
| -- | -- |
| `tokio::process::Command::creation_flags(u32)` | yes (delegates to `std::os::windows::process::CommandExt`) |
| `tokio::process::Child::raw_handle() -> Option<RawHandle>` | yes |
| `std::os::windows::process::ProcThreadAttributeList` (for `PROC_THREAD_ATTRIBUTE_JOB_LIST`) | **nightly only** — `windows_process_extensions_raw_attribute` |
| `std::os::windows::process::ChildExt::main_thread_handle()` | **nightly only** — [rust#96723](https://github.com/rust-lang/rust/issues/96723) |

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
nightly-only), roughly doubling the `unsafe` surface and adding a failure mode
where a child can be left permanently suspended.

**Accepted gap**, recorded in the repo's existing idiom (see
`crates/paigasus-helikon-tools/src/exec/forkd.rs:554-558`): a grandchild spawned
in the microseconds before assignment escapes the job. Revisit if
`windows_process_extensions_raw_attribute` stabilises, which removes the trade-off
entirely.

### Decision 3 — terminate on timeout only; mirror unix

Create the job with **no limit flags**, and call `TerminateJobObject` only on the
timeout path — exactly where unix sends `SIGKILL` to the process group.

| Path | unix (today) | Windows (after this change) |
| -- | -- | -- |
| timeout | `kill(-pgid, SIGKILL)` — subtree dies | `TerminateJobObject` — subtree dies |
| normal completion | process group untouched; survivors live | job handle closes; survivors live |

Setting `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` was considered and rejected. It would
reap survivors of a *normally completed* run when the handle drops, which unix does
not do — a silent asymmetry — and it would not improve latency, because the reader
drain has already spent its grace by then. Dropping the flag also removes
`SetInformationJobObject` and `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` from the FFI
surface entirely.

### Decision 4 — degrade on failure, never fail the run

If `CreateJobObjectW` or `AssignProcessToJobObject` fails — a locked-down runner,
an outer job refusing nesting — the job binding is `None` and the timeout path
falls back to `child.start_kill()`, which is exactly today's behaviour. Strictly no
worse than the status quo. A job-object hiccup must not turn a working `run()`
into an error.

## Design

### `src/exec/job_object.rs` (new, `#[cfg(windows)]`)

One RAII type over an anonymous job handle:

```rust
pub(crate) struct JobObject(HANDLE);

impl JobObject {
    /// Create an anonymous job object and assign `process` to it.
    pub(crate) fn assign(process: HANDLE) -> std::io::Result<Self>;
    /// Terminate every process in the job.
    pub(crate) fn terminate(&self);
}

impl Drop for JobObject {
    // CloseHandle. With no limit flags set this has no effect on live members.
}

// SAFETY: Win32 kernel handles are process-wide and not thread-affine;
// TerminateJobObject and CloseHandle are thread-safe.
unsafe impl Send for JobObject {}
```

The `unsafe impl Send` is load-bearing, not incidental. `spawn_capped`'s future
holds the binding across `.await`, and `ExecutionBackend::run` is `#[async_trait]`,
which bounds that future `Send`. A bare `HANDLE` is `*mut c_void`, which would
poison it. `Sync` is neither needed nor claimed.

Four imported symbols: `CreateJobObjectW`, `AssignProcessToJobObject`,
`TerminateJobObject` (`Win32_System_JobObjects`) and `CloseHandle`
(`Win32_Foundation`).

`CreateJobObjectW` returns `NULL` on failure (not `INVALID_HANDLE_VALUE`); both
`BOOL`-returning calls report failure as `0`. Every failure is surfaced as
`std::io::Error::last_os_error()`.

### `src/exec/mod.rs`

Immediately after spawn, parallel to the existing unix binding:

```rust
let mut child = cmd.spawn()?;

#[cfg(unix)]
let pgid = child.id();
#[cfg(windows)]
let job = child.raw_handle().and_then(|h| JobObject::assign(h).ok());
```

Assignment comes before the `stdout`/`stderr` pipes are taken, so the race window
of Decision 2 stays as small as the API allows.

The timeout arm becomes three-way, replacing today's `unix` / `not(unix)` split:

```rust
#[cfg(windows)]
{
    match &job {
        Some(j) => j.terminate(),
        None => { let _ = child.start_kill(); }
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

The job binding lives until the end of `spawn_capped`, so the handle outlives both
the kill and the reader drain.

### Manifest

Root `Cargo.toml`:

```toml
[workspace.dependencies]
windows-sys = { version = "0.61", features = [
  "Win32_Foundation",
  "Win32_Security",          # SECURITY_ATTRIBUTES, referenced by the JobObjects bindings
  "Win32_System_JobObjects",
] }
```

`crates/paigasus-helikon-tools/Cargo.toml`:

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { workspace = true }
```

`Win32_Security` is required and easy to miss: `Win32_System_JobObjects` enables
only `Win32_System`, but the generated `JobObjects` module references
`Security::SECURITY_ATTRIBUTES` in `CreateJobObjectW`'s signature. Without it the
crate does not compile on Windows.

## Testing

### New: `timeout_kills_the_whole_subtree`

Portable, added to `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`
(the one real-process exec test file that is not `cfg`-gated).

The test writes a small script into the sandbox and launches it **detached**, so
the sentinel-writer is a genuine grandchild of `cmd.exe`/`sh`. Using a script file
rather than inlining the grandchild command avoids nested-quoting failures in
`start /B cmd /C "…"`.

| | grandchild launch | script body |
| -- | -- | -- |
| unix | `sh grandchild.sh & sleep 10` | `sleep 2` then `: > sentinel` |
| Windows | `start /B cmd /C grandchild.cmd >NUL 2>&1 & ping -n 10 127.0.0.1 >NUL` | `ping -n 3 127.0.0.1 >NUL` then `echo alive>sentinel` |

Then: 200 ms backend timeout, assert `timed_out`, wait past the grandchild's delay,
and assert **the sentinel never appears**.

The assertion is on a file, not on wall time, so a slow runner cannot make it
flaky. Grandchild output is redirected to `NUL`/`/dev/null` so a surviving pipe
writer cannot influence the reader drain either way.

On unix this newly guards the `process_group(0)` kill, which has no test of its own
today.

### Secondary signal (observed, not asserted)

`timeout_reports_no_exit_code`'s Windows wall time should fall from ~4.06 s to
roughly the unix figure, because terminating the job kills `ping`, which closes the
pipe, which lets the reader drain return immediately instead of waiting out the
grandchild. Deliberately **not** asserted — a wall-clock bound on a CI runner is a
flake source.

### Local verification

`x86_64-pc-windows-gnu` is an installed target on the dev host, so the Windows arm
compiles locally:

```bash
cargo check --target x86_64-pc-windows-gnu -p paigasus-helikon-tools --all-targets
```

This catches feature-gate and symbol mistakes without a CI round trip. It cannot
*run* the test: `test (windows-latest, stable)` in CI is the only execution gate,
and it is a required check.

## Documentation

- `ExecOutput::timed_out` — state the subtree contract (process group on unix, Job
  Object on Windows), matching how `ExecOutput::exit_code` already documents its
  cross-platform guarantee.
- `spawn_capped`'s doc comment claims "killing the whole process group on timeout".
  Now true on both platforms, but the wording is unix-specific; make it
  platform-explicit.
- `docs/book/src/concepts/tools.md`, `HostBackend` section — add a short paragraph
  on timeout semantics, including the accepted gap. The book says nothing about
  them today.
- `crates/paigasus-helikon-tools/README.md` — **no edit.** It does not discuss
  timeouts or exec containment, and this change adds no public API and no feature
  flag. Conscious call per CLAUDE.md.
- No manual version bump and no CHANGELOG edit: no `paigasus-helikon-core` API is
  added here, so release-plz handles the bump on merge.

## Risks

- **Nested job objects.** If the CI runner already places us in a job, assignment
  relies on Windows 8+ nested-job support. Expected to work on `windows-latest`; if
  a runner's job policy blocks it we degrade per Decision 4 and the new test fails
  *loudly* rather than silently passing. The first CI run is the real check.
- **Windows-only code path with one execution gate.** Local `cargo check` covers
  compilation; behaviour is verified only by `test (windows-latest, stable)`.
- **Merge overlap with SMA-614 (PR #234, open).** That branch also edits
  `exec/mod.rs` (the env-var loop and `DEFAULT_ENV_ALLOWLIST`) and
  `exec_timeout_portable.rs` (it deletes that file's local `ENV_ALLOWLIST` const in
  favour of the new platform-aware default). The regions differ from this change's,
  so conflicts should be mechanical, but whichever lands second must rebase. On this
  base the new test supplies its own Windows env allowlist, as the existing test
  does.
