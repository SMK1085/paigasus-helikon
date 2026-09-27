# SMA-710: stop the process group before the timeout kill

- **Linear:** [SMA-710](https://linear.app/smaschek/issue/SMA-710)
- **Crate:** `paigasus-helikon-tools` (unix exec timeout path)
- **Related:** SMA-613 (the Windows subtree kill), SMA-569 (the `exit_code: None` contract)

## Problem

On a timeout, `spawn_capped` (`crates/paigasus-helikon-tools/src/exec/mod.rs`)
sends one `SIGKILL` to the process group of the shell:
`libc::kill(-(pid as i32), libc::SIGKILL)`. The group comes from
`cmd.process_group(0)`.

The test `timeout_kills_the_whole_subtree` failed one time on
`test (macos-latest, 1.94)` (run 36300504591, attempt 1, job 108567149965). A
re-run of the same commit passed. The stderr of the failed run was:

```text
…/grandchild.sh: line 2: 52012 Killed: 9               sleep 4
sh: line 1: 52011 Killed: 9               sh "…/grandchild.sh"
```

The `alive` file existed. This sequence occurred:

1. `sleep` (52012) died from `SIGKILL`.
2. The grandchild shell (52011) was still alive. Its `wait4` returned, it printed
   the report for `sleep`, and it ran line 3 (`echo alive > …`).
3. After that, the grandchild shell died from `SIGKILL`.

A timed-out command did one more action after the timeout. That is a
containment gap.

## Evidence for the cause

### No reproduction

A subagent ran 24,120 trials on 2026-09-27. The race did not occur once:

| Surface | Host | Trials | Failures |
|---|---|---|---|
| C reproducer, `SIGKILL` only, idle + CPU load | macOS 26.6.2 (25G83), 18 cores | 4,000 | 0 |
| C reproducer, `SIGSTOP` then `SIGKILL`, idle + load | same | 4,000 | 0 |
| C reproducer, `SIGSTOP`, `SIGKILL`, `SIGCONT`, idle + load | same | 4,000 | 0 |
| Same three variants | Linux, Docker `gcc:13` (Debian 12), 8 CPUs | 12,000 | 0 |
| Real test binary, idle + load | macOS, as above | 120 | 0 |

The C reproducer mirrors the production shape: `fork`, `setpgid(0,0)`,
`sh -c 'sh grandchild.sh; true'`, then `kill(-pgid, …)`.

### Source evidence

Sven decided on 2026-09-27 to fix the gap on this evidence, without a
reproduction:

- **The CI stderr** (above). It shows that the two deliveries of `SIGKILL`
  were not one atomic event. The grandchild shell ran user code between them.
- **XNU `bsd/kern/kern_sig.c`** (the public `apple-oss-distributions/xnu`
  mirror; it is not confirmed to be byte-identical to build 25G83).
  `killpg1()` calls `pgrp_iterate(…, killpg1_callback, …)`, and the callback
  calls `psignal(p, signum)` on one member at a time. For `SIGKILL`,
  `psignal_internal` only arms an AST on the target thread
  (`act_set_astbsd`, `thread_abort`). The thread dies later, when it runs.

**Inference (not observed in a trace):** `killpg1` signals `sleep` first.
`sleep` dies, and its death wakes the grandchild shell from `wait4` through
the child-exit path. The grandchild shell runs user code before its own
`SIGKILL` arrives or is enforced. A narrow scheduling window like this is more
probable on an oversubscribed, virtualized CI runner than on a dedicated host.

### Limit of the acceptance criteria

The issue asks for "the loop reproduction shows 0 failures". The loop shows 0
failures before the fix too, so it cannot show a difference before and after
the fix. This spec does not claim that it does.

## Design

### Approaches considered

1. **`SIGSTOP` the group, then `SIGKILL` the group.** Chosen.
2. **Send `SIGKILL` in a loop until `ESRCH`.** Rejected. It makes sure that the
   group dies at the end, but a member can still act between two deliveries.
3. **Freeze the group with an OS tool** (the Linux cgroup freezer). Rejected.
   macOS has no equivalent, and the change is much larger.

### Why `SIGSTOP` first closes the window

- The `SIGSTOP` pass kills no process. Thus no member exits, and no parent
  wakes from `wait4` during this pass. A non-interactive `sh` does not wait
  with `WUNTRACED`, so a stopped child does not wake it either.
- `kill(-pgid, SIGSTOP)` returns only after `killpg1` has posted `SIGSTOP` to
  every member. Thus, when the `SIGKILL` pass starts, every member is stopped or
  has a `SIGSTOP` pending.
- In the `SIGKILL` pass, the death of one member can wake its parent. The
  parent must process its pending `SIGSTOP` before it returns to user code, so
  it stops and cannot run user code. `SIGKILL` then terminates it.
- `SIGSTOP` cannot be caught, blocked, or ignored. POSIX lets the kernel
  discard some stop signals sent to an orphaned process group, but only
  `SIGTSTP`, `SIGTTIN`, and `SIGTTOU`. `SIGSTOP` is not in that list. (The
  group is also not orphaned: its leader's parent is our process, in the same
  session.)
- POSIX requires `SIGKILL` to terminate a stopped process. The reproducer
  confirmed this without `SIGCONT`: the direct child always ended with
  `WIFSIGNALED && WTERMSIG == SIGKILL`, with 0 hangs in 4,000 macOS runs and
  4,000 Linux runs.

### Code change

Only `crates/paigasus-helikon-tools/src/exec/mod.rs` changes.

A new private, unix-only function replaces the single `libc::kill` call in
the timeout arm:

```rust
/// Kill every process in the group `pgid`, with no chance for a member to run
/// user code between the deliveries (SMA-710).
#[cfg(unix)]
fn kill_process_group(pgid: u32) {
    let target = -(pgid as i32);
    // SAFETY: …
    if unsafe { libc::kill(target, libc::SIGSTOP) } != 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return; // The group is already gone.
    }
    // SAFETY: …
    let _ = unsafe { libc::kill(target, libc::SIGKILL) };
}
```

Rules:

- If the `SIGSTOP` call fails with `ESRCH`, the group is gone. Return.
- On any other `SIGSTOP` result (success or a different error), always send
  `SIGKILL`. Thus the new behavior is never weaker than the old behavior.
- Do not send `SIGCONT`. It is not necessary (see above), and it adds a
  delivery that could let an unstopped straggler run.
- Do not log. The current path does not log, and a timed-out run never
  returns `Err`.

These do not change: `process_group(0)`, the reap with `GRACE`, the reader
drain, the Windows path, and the `ExecOutput::exit_code` contract (a timed-out
run reports `exit_code: None` and `timed_out: true`).

The host, Seatbelt, and Linux sandbox backends all use `spawn_capped`, so all
three get the fix.

### Contract documentation

Update the rustdoc on `ExecOutput::timed_out` (`exec/mod.rs`, near line 150).
Change "a process group `SIGKILL` on unix" to "a process group `SIGSTOP`,
then `SIGKILL`, on unix, so that no member can run user code between the two
deliveries". Add the unix accepted gaps below to the same comment.

### Accepted gaps (unix, out of scope)

- **A fork between the two passes.** A member that is inside `fork()` when the
  `SIGSTOP` pass runs can create a child after the pass. The child is not
  stopped and can run until the `SIGKILL` pass reaches it. The window is two
  consecutive syscalls. An equivalent gap exists today for a fork inside the
  single `SIGKILL` pass.
- **A member that leaves the group.** A member that calls `setpgid` or `setsid`
  escapes both passes. This gap exists today.

## Testing

No test can fail before the fix and pass after it, because the race does not
reproduce. The testing therefore protects the new mechanism against
regressions.

### Strengthen `timeout_kills_the_whole_subtree`

File: `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`.

**New hole to close:** if the code regresses to "`SIGSTOP` only", the stopped
processes never write `alive`, so the current test passes. The test must also
prove that the grandchild shell is dead.

- On unix, the grandchild script writes its own PID (`$$`) into the `started`
  file. The Windows script does not change.
- After the existing 6 s wait, on unix, the test reads the PID and asserts that
  the process is gone. Use `libc::kill(pid, 0)`: `ESRCH` means gone. If the
  call succeeds, check the process state with `ps -o stat= -p <pid>`: a state
  that starts with `Z` (a zombie) counts as dead, because the process can run
  no more code. Any other state fails the test with the PID and the state in the
  message. (`libc` is already a unix dependency of the crate, so the
  integration test can use it.)
- The existing `started` positive control and the `alive` assertion stay.

### Mutation checks (run locally, report the output in the PR)

1. Remove the `SIGKILL` call (leave only `SIGSTOP`). The strengthened test
   must fail on the new PID assertion.
2. Remove the `SIGSTOP` call (the old behavior). The test must still pass.
   This is expected, because the race does not reproduce. Record it in the PR
   so that the reader knows that the test does not guard the race itself.

### Gates

- `cargo test -p paigasus-helikon-tools --test exec_timeout_portable` passes
  locally on macOS.
- The full CI matrix passes, including `test (ubuntu-latest, stable)`,
  `test (macos-latest, stable)`, and `test (windows-latest, stable)`. The
  Windows leg compiles the changed test file; the unix-only code must be
  `cfg`-gated so that the Windows build has no unused-item warnings.
- `cargo clippy --workspace --all-features --all-targets -- -D warnings` and
  `cargo fmt --all -- --check` pass.

## Documentation and release

- **mdBook and READMEs:** no change. The public API and the usage do not
  change. This is a conscious decision, not a skip.
- **Commit type:** `fix(tools): SMA-710 …`. release-plz makes a patch bump of
  `paigasus-helikon-tools` and cascades it to the facade. No hand-bump of a
  version.
