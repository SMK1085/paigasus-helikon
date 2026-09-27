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
4. The outer shell (the group leader) printed the report for 52011. So the
   leader also ran user code after another member died.

A timed-out command did one more action after the timeout. That is a
containment gap.

## Evidence for the cause

### No reproduction

A subagent ran these trials on 2026-09-27. The race did not occur once:

| Surface | Host | Trials | Failures |
|---|---|---|---|
| C reproducer, `SIGKILL` only, idle + CPU load | macOS 26.6.2 (25G83), 18 cores, physical | 4,000 | 0 |
| Real test binary (old code), idle + load | same | 120 | 0 |

The C reproducer uses the production shape for the old kill: `fork`,
`setpgid(0,0)`, `sh -c 'sh grandchild.sh; true'`, then `kill(-pgid, SIGKILL)`.
The CI failure occurred on a small, virtualized runner. The local host is an
18-core physical machine, so a failure rate of 0 on it says little about the
runner.

Linux trials (12,000, Docker `gcc:13`) are not evidence either way. The
reason below comes from the spec challenger's reading of the Linux source. It
was not checked independently. On Linux,
`exit_notify` runs `do_notify_parent` under `write_lock(tasklist_lock)`, and
the group kill holds `tasklist_lock` for reading. Thus no member can see the
death of a child in `wait4` before the whole group has `SIGKILL`. The observed
race cannot occur on Linux through `wait4`.

### Source evidence

Sven decided on 2026-09-27 to fix the gap on this evidence, without a
reproduction:

- **The CI stderr** (above). The deliveries of `SIGKILL` to the members were
  not one atomic event. Two members ran user code between them.
- **XNU `bsd/kern/kern_sig.c`** (the public `apple-oss-distributions/xnu`
  mirror; it is not confirmed to be byte-identical to build 25G83).
  `killpg1()` calls `pgrp_iterate(…, killpg1_callback, …)`, and the callback
  calls `psignal(p, signum)` on one member at a time. For `SIGKILL`,
  `psignal_internal` only arms an AST on the target thread
  (`act_set_astbsd`, `thread_abort`). The thread dies later, when it runs.
- **Inference (kern_sig.c, not observed in a trace):** `proc_signalstart`
  can put the killing thread to sleep (`msleep`) while another thread delivers
  a signal to the same target (`P_LINSIGNAL`). The exit of `sleep` sends
  `SIGCHLD` to the grandchild shell. If `killpg1` reaches the grandchild shell
  at that moment, the killer sleeps and the grandchild shell can run first. On
  a loaded VM, this window can be milliseconds long.

**Inference (not observed in a trace):** `killpg1` signals `sleep` first.
`sleep` dies, and its death wakes the grandchild shell from `wait4`. The
grandchild shell runs user code before its own `SIGKILL` is enforced.

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

The names differ per kernel. XNU uses `killpg1` → `pgrp_iterate`. Linux uses
`kill_something_info` → `__kill_pgrp_info`.

**Common to both kernels:**

- The `SIGSTOP` pass kills no process, so no member exits and no child
  "ends". `SIGSTOP` cannot be caught, blocked, or ignored.
- The `SIGSTOP` pass does wake parents. The kernel sends `SIGCHLD` with
  `CLD_STOPPED` to each parent, unless the parent set `SA_NOCLDSTOP`. A parent
  with a `SIGCHLD` handler runs that handler. But a non-job-control shell waits
  without `WUNTRACED`, so its `wait4` does not return for a stopped child. The
  shell goes back to sleep and cannot move to its next command.
- `kill(-pgid, SIGSTOP)` returns only after the kernel has processed
  `SIGSTOP` for every member. So when the `SIGKILL` pass starts, every member
  is stopped or is about to stop.
- POSIX requires `SIGKILL` to terminate a stopped process. No `SIGCONT` is
  necessary.

**XNU:** `psignal_internal` processes a default-action `SIGSTOP` in the
sender. It clears the signal, sets `SSTOP`, and calls `stop()`, which calls
`task_suspend_internal` (kern_sig.c). No `SIGSTOP` stays pending. When the
`SIGKILL` pass wakes a parent (through the death of its child), the parent's
thread cannot return to user mode while its task is suspended. *Inference:*
Mach task suspension holds each thread at its return to user mode
(`thread_hold`, `AST_APC`); that code is in osfmk and was not read.

**Linux** (from the spec challenger's reading of the Linux source; not checked
independently): `SIGSTOP` stays pending until the target runs. `get_signal`
processes pending signals before the return to user mode, and it checks
`SIGNAL_GROUP_EXIT` (set by `SIGKILL`) first. So a woken member dies or stops
before it runs user code.

### Orphaned process group

**The risk.** POSIX says what happens when a process group becomes orphaned
while a member is stopped: the kernel sends `SIGHUP`, then `SIGCONT`, to every
member. Our group becomes orphaned when the leader exits, because only the
leader has a parent (our process) outside the group in the same session. XNU
does this in `proc_exit` → `fixjobc` → `orphanpg` (`kern_exit.c:2365`,
`kern_proc.c:3013-3070`, `kern_proc.c:3159-3224`). `orphanpg` sends
`SIGHUP` and `SIGCONT` if any member has `p_stat == SSTOP`. Suppose the leader
got `SIGKILL` and exited before `killpg1` reached a stopped member. Then that
member would resume, and a member that ignores `SIGHUP` would run user code.

**Why it does not occur (evidence, XNU source):**

- `pgrp_add_member` inserts at the head: `LIST_INSERT_HEAD(&pgrp->pg_members,
  …)` (`kern_proc.c:2608`). A forked child joins its parent's group through
  this function (`kern_proc.c:2931-2932`).
- `pgrp_iterate` takes a snapshot of the member PIDs under `pgrp_lock` with a
  head-first `LIST_FOREACH`, then calls the callback for each PID in that order
  (`kern_proc.c:4266-4349`, `proc_internal.h:999-1000`).
- So `kill(-pgid, …)` signals the members newest first. The leader is the
  oldest member, so it is always signalled **last**.
- For `SIGKILL`, `psignal_internal` sets `p_stat = SRUN` synchronously, in the
  sender (`kern_sig.c:2434`).
- Thus, before the leader can start `proc_exit`, every other member already
  has `p_stat == SRUN`. `orphanpg` finds no stopped member and sends nothing.

**Limit.** This holds while the leader is the oldest member. That is true for
every process that joins by fork. A process that joins the group later with an
explicit `setpgid(…, pgid)` is newer than the leader too, so it is also
signalled before the leader. Only a leader that moves to a different group
and then joins again could change the order. That needs explicit `setpgid`
calls in the command, and the gap "a member that leaves the group" already
covers it.

**Linux** (inference, source not read): the group kill holds `tasklist_lock`
for reading, and `exit_notify` needs it for writing. So the leader cannot run
the orphan check until the kill pass is complete.

**Experiments (2026-09-27).** The grandchild script started with
`trap '' HUP`, so an orphaned-group `SIGHUP` could not hide a survivor. The
harness recorded the PIDs of the grandchild shell and of `sleep`, and checked
after the kill that they were gone (with an identity check).

| Kill sequence | Host | Trials | Survivors or `alive` |
|---|---|---|---|
| `SIGSTOP` group, `SIGKILL` group, back-to-back (the design) | macOS, idle + load | 4,000 | 0 |
| same | Linux (Docker, 8 vCPU), idle + load | 4,000 | 0 |
| `SIGKILL` group only (the old code) | macOS, idle + load | 4,000 | 0 |
| `SIGSTOP` group, `SIGKILL` to the leader only (mutation) | macOS | 500 | 500 (resumed, running) |
| same | Linux | 500 | 500 (stopped) |
| same mutation, **without** `trap '' HUP` | macOS | 500 | 0 (`SIGHUP` killed them) |
| same | Linux | 500 | 499 (stopped) |

The mutation rows show that the harness can see the orphaned-group resume.
They also show that a test without `trap '' HUP` does not detect that
regression on macOS.

### Trade-off (not "never weaker")

The new code always sends `SIGKILL`, as the old code does. But the
`SIGSTOP` pass creates new events: `CLD_STOPPED` notifications, parent
wakeups on XNU, and stop reports to a `ptrace` tracer or a `WUNTRACED`
waiter. A member that reacts to a child *stopping* gets a small new window on
both kernels.

The gains:

- **macOS:** it closes the observed `wait4` race.
- **Linux:** the observed race cannot occur, but a similar one can.
  `exit_files` runs before `exit_notify`, so pipe EOF or an `flock` release
  can wake a member while the kill pass is still running. The `SIGSTOP` pass
  closes that window.

The net result is positive.

### Code change

Change `crates/paigasus-helikon-tools/src/exec/mod.rs` only (code). A new
private, unix-only function replaces the single `libc::kill` call in the
timeout arm:

```rust
/// Kill every process in the group `pgid` (SMA-710).
///
/// Stops the group first, so that the death of one member cannot wake another
/// member into user code before that member is also killed. The two `kill`
/// calls must stay back-to-back and synchronous: never put an `.await`, a lock,
/// or other work between them.
#[cfg(unix)]
fn kill_process_group(pgid: u32) {
    let target = -(pgid as i32);
    // SAFETY: …
    let _ = unsafe { libc::kill(target, libc::SIGSTOP) };
    // SAFETY: …
    if unsafe { libc::kill(target, libc::SIGKILL) } != 0 {
        let err = std::io::Error::last_os_error();
        // ESRCH: the group is gone. EPERM on macOS: only zombies remain.
        if !matches!(err.raw_os_error(), Some(libc::ESRCH) | Some(libc::EPERM)) {
            tracing::warn!(/* target, error, message: the subtree may stay stopped */);
        }
    }
}
```

Rules:

- Send both signals every time. There is no early return after `SIGSTOP`:
  the leader is not reaped yet, so the pgid cannot be reused, and an early
  return prevents nothing.
- Do not send `SIGCONT`.
- If `SIGKILL` fails with an error other than `ESRCH` or `EPERM`, emit
  `tracing::warn!` on the `paigasus::tools::exec` target. In that case the
  survivors stay stopped (they hold pipes, locks, and memory), which is a new
  failure mode. The Windows path already warns when it degrades.
  *(EPERM: on macOS, `killpg1` returns `EPERM` for a group that has only
  zombies. EPERM from a real permission denial is not expected, because
  `SIGSTOP` and `SIGKILL` pass the same permission check, except under an
  unusual SELinux policy.)*

These do not change: `process_group(0)`, the reap with `GRACE`, the reader
drain, the Windows path, and the `ExecOutput::exit_code` contract (a timed-out
run reports `exit_code: None` and `timed_out: true`).

The host, Seatbelt, and Linux os-sandbox backends all call `spawn_capped`, so
all three get the fix. The sandboxes do not change signal delivery: the
unsandboxed parent sends the signals, `sandbox-exec` replaces itself with `sh`
(so `sh` is the leader), seccomp filters only the child's own syscalls, and
Landlock scoping limits only a sandboxed sender.

### Comments to update

- `exec/mod.rs`, the rustdoc on `ExecOutput::timed_out` (near line 150).
- `exec/mod.rs` near lines 402-403 ("before our SIGKILL lands").
- `tests/exec_timeout_portable.rs` near line 169 ("`process_group(0)` +
  `SIGKILL` path") and near line 68 (the script "writes `started`").
- `docs/book/src/concepts/tools.md` lines 383-394 ("a process-group `SIGKILL`
  on unix", and the accepted-gap paragraph).

New text for the rustdoc and the book:

> a process-group `SIGSTOP`, then `SIGKILL`, on unix, so that the death of one
> member cannot wake another member into user code before that member is also
> killed.

Add the unix accepted gaps below to both places.

### Accepted gaps (unix, out of scope)

- **A process that watches for stops.** A `ptrace` tracer (`strace -f`,
  `lldb`) or a waiter that uses `WUNTRACED` sees the `SIGSTOP` and can act on
  it. The Linux os-sandbox blocks `ptrace`, so this applies to the host and
  Seatbelt backends.
- **A fork between the two passes (macOS only).** A member that is inside
  `fork()` when the `SIGSTOP` pass runs can create a child after the pass. The
  child is not stopped and can run until the `SIGKILL` pass reaches it. On
  Linux, `copy_process` handles a group signal during `fork`, so this gap does
  not exist there.
- **A member that leaves the group.** A member that calls `setpgid` or `setsid`
  escapes both passes. This gap exists today.
- **A failed `SIGKILL`.** The survivors stay stopped, and a warning is
  emitted.

## Testing

No test can fail before the fix and pass after it, because the race does not
reproduce. The testing therefore protects the new mechanism against
regressions.

### Strengthen `timeout_kills_the_whole_subtree`

File: `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`.

**Holes to close:**

1. If the code regresses to "`SIGSTOP` only", the stopped processes never
   write `alive`, so the current test passes.
2. If the code regresses to "`SIGKILL` the direct child only" (for example
   `child.start_kill()`), the leader dies and the group becomes orphaned with
   stopped members. The kernel then sends `SIGHUP` and `SIGCONT`, and the
   grandchild shell dies of `SIGHUP`. The current test passes.

**Changes to the unix grandchild script:**

- The first line is `trap '' HUP`. `sleep` inherits the ignored `SIGHUP`. So
  an orphaned-group `SIGHUP` can no longer kill the grandchild and hide a
  regression.
- Next to `echo started > …`, the script prints a fixed marker to stdout, for
  example `echo grandchild-started`.

**New assertion (unix only):** `out.stdout` contains the marker. A stopped
survivor keeps the stdout pipe open. Then `read_capped` gets no EOF,
`join_reader` times out after `GRACE`, and `join_reader` returns an empty
string. So the marker is missing if any process of the subtree survives,
stopped or running. A survivor that runs also writes `alive`, which the
existing assertion catches.

This design needs no PID, no `ps`, and no zombie handling. The existing
`started` positive control and the `alive` assertion stay. The Windows script
and the Windows assertions do not change.

**Windows hygiene:** the Windows test leg runs `cargo test` without
`-D warnings`, and clippy runs only on ubuntu. So no CI gate catches an unused
item on Windows. Put `#[cfg(unix)]` on every unix-only item, call site, and
`use`, and check this in review.

### Mutation checks (run locally on macOS, report the output in the PR)

1. Remove the `SIGKILL` call (leave only `SIGSTOP`). The test must fail on the
   marker assertion.
2. Replace the group `SIGKILL` with `SIGKILL` to the direct child only. The
   test must fail.
3. Remove the `SIGSTOP` call (the old behavior). The test must still pass.
   This is expected, because the race does not reproduce. Record it in the PR,
   so that the reader knows that the test does not guard the race itself.

### Gates

- `cargo test -p paigasus-helikon-tools --test exec_timeout_portable` passes
  locally on macOS.
- The full CI matrix passes, including `test (ubuntu-latest, stable)`,
  `test (macos-latest, stable)`, and `test (windows-latest, stable)`.
- `cargo clippy --workspace --all-features --all-targets -- -D warnings`,
  `cargo fmt --all -- --check`, and `mdbook build docs/book` pass.

### Not in scope

- A subtree timeout test for the os-sandbox backends. They call the same
  `spawn_capped` function, and they do not change signal delivery (see
  above).
- A loop on a GitHub `macos-latest` runner. Sven decided on 2026-09-27 to fix
  on the source evidence.

## Documentation and release

- **mdBook:** update `docs/book/src/concepts/tools.md` (see "Comments to
  update").
- **READMEs:** no change. No README describes the kill mechanism.
- **Commit type:** `fix(tools): SMA-710 …`. release-plz makes a patch bump of
  `paigasus-helikon-tools` and cascades it to the facade. No hand-bump of a
  version.
