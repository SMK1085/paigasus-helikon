# SMA-710 stop the process group before the timeout kill — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On a unix exec timeout, send `SIGSTOP` and then `SIGKILL` to the
process group, so that the death of one member cannot wake another member into
user code before that member is also killed.

**Architecture:** One private, unix-only function `kill_process_group` in
`crates/paigasus-helikon-tools/src/exec/mod.rs` replaces the single
`libc::kill(-pgid, SIGKILL)` call in the timeout arm of `spawn_capped`. The
existing subtree test gets a `trap '' HUP` and a stdout marker, so that it
detects a survivor that stays stopped and a "kill only the direct child"
regression. The rustdoc contract and the mdBook get the new mechanism and the
unix accepted gaps.

**Tech Stack:** Rust 1.94 (MSRV), `libc` (already a `cfg(unix)` dependency),
`tokio`, `tracing`.

**Spec:** `docs/superpowers/specs/2026-09-27-sma-710-exec-timeout-stop-then-kill-design.md`

## Global Constraints

- Work in the worktree
  `/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/3d698441-d9e1-441b-b390-fb03f37af62e/scratchpad/wt-sma-710`
  on branch `feature/sma-710-exec-timeout-on-macos-a-grandchild-can-run-one-more-command`.
  Use absolute paths under this root for every Read, Edit, and Write.
- Do not run a git command that moves HEAD or changes the branch (`checkout`,
  `switch`, `reset`, `rebase`, `stash`). Commit with explicit paths. Never
  `git add -A` (the repo does not ignore `.env`).
- Run every `cargo` command in the foreground and wait for it to finish. Do not
  end your turn while a build or test runs.
- Do not send `SIGCONT`. Always send both signals. No `.await`, lock, or other
  work between the two `kill` calls.
- The Windows path, `process_group(0)`, the `GRACE` reap, the reader drain, and
  the `ExecOutput::exit_code` contract (`None` when `timed_out`) must not
  change.
- Every unix-only item, call site, and `use` in the test file has
  `#[cfg(unix)]`. `libc` is not available on Windows, so an ungated `libc::`
  is a compile error on the required `test (windows-latest, stable)` gate.
- Commit messages: `<type>(tools): SMA-710 <lowercase subject>` or
  `docs(book): …`. End every commit message with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Before each commit that touches Rust: `cargo fmt --all` and
  `cargo clippy -p paigasus-helikon-tools --all-features --all-targets -- -D warnings`.
- Do not hand-bump any crate version. release-plz does it.

## Review Focus

1. **Windows build of the test file.** A reasonable person expects the
   required Windows gate to stay green. Nothing on macOS compiles the Windows
   arm. Owner: Task 1, Step 6 (a review check of every `cfg`).
2. **A command that ends at the same moment as the timeout.** The group can be
   gone when `kill_process_group` runs. Expected: `timed_out: true`,
   `exit_code: None`, no warning, no panic. Owner: Task 2, Step 5 (the existing
   `exec_timeout_portable` tests plus the full crate test run).
3. **The Seatbelt backend on macOS.** `sandbox-exec` replaces itself with
   `sh`, so `sh` is the leader. Expected: the timeout still kills the subtree.
   Owner: Task 2, Step 5 (run the full `paigasus-helikon-tools` tests with
   `--all-features`, which include the Seatbelt tests on macOS).
4. **A survivor that stays stopped.** Expected: the test fails, and the
   failure message says that the marker is missing. Owner: Task 2, Step 6
   (mutation check 1).
5. **A regression that kills only the direct child.** Expected: the test fails
   on macOS, even though the orphaned-group `SIGHUP` would kill a grandchild
   without the trap. Owner: Task 1, Step 4 and Task 2, Step 6 (mutation
   check 2).

---

### Task 1: Make the subtree test detect stopped survivors and a direct-child-only kill

**Files:**
- Modify: `crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`
  (the unix `grandchild_script` near line 88, its doc comment near line 68,
  the test doc comment near line 167, and the end of
  `timeout_kills_the_whole_subtree` near line 234)

**Interfaces:**
- Consumes: nothing new. `HostBackend`, `ExecRequest`, and `ExecOutput` as
  today.
- Produces: `const GRANDCHILD_MARKER: &str = "grandchild-started";`
  (`#[cfg(unix)]`, test-file private). Task 2 relies on the test failing with
  the message text `the grandchild marker is missing from stdout`.

This task changes only the test. The current code (group `SIGKILL` only) must
still pass it. The RED step is a temporary mutation of the production code,
which you revert before the commit.

- [ ] **Step 1: Add the marker constant**

Below the `GRANDCHILD_SCRIPT_NAME` constants, add:

```rust
/// Printed to stdout by the unix grandchild next to `started`. A process of the
/// subtree that survives the timeout — stopped or running — keeps the inherited
/// stdout pipe open, so the reader never sees EOF, `join_reader` gives up after
/// `GRACE`, and the captured stdout is empty. So "the marker is in stdout"
/// proves that every process of the subtree is gone (SMA-710).
#[cfg(unix)]
const GRANDCHILD_MARKER: &str = "grandchild-started";
```

- [ ] **Step 2: Change the unix grandchild script**

Replace the body of the unix `grandchild_script` with:

```rust
#[cfg(unix)]
fn grandchild_script(started: &Path, alive: &Path) -> String {
    format!(
        "trap '' HUP\necho {GRANDCHILD_MARKER}\necho started > \"{}\"\nsleep 4\necho alive > \"{}\"\n",
        started.display(),
        alive.display()
    )
}
```

Update the doc comment above the unix and Windows `grandchild_script` (the one
that starts "Builds the script body that writes `started` immediately") by
adding this paragraph after its first paragraph:

```rust
/// On unix the script also starts with `trap '' HUP` and prints
/// `GRANDCHILD_MARKER` to stdout. The trap matters: when the timeout kill
/// reaches the group leader before a stopped member, the group becomes orphaned
/// with a stopped member, and the kernel sends it `SIGHUP` then `SIGCONT`. With
/// the default `SIGHUP` action that kills the grandchild, which would hide a
/// regression that kills only the direct child. `sleep` inherits the ignored
/// `SIGHUP` (SMA-710).
```

- [ ] **Step 3: Add the marker assertion**

At the end of `timeout_kills_the_whole_subtree`, after the existing `alive`
assertion, add:

```rust
    // SMA-710: a survivor that stays stopped never writes `alive`, so the
    // assertion above cannot see it. It does keep the stdout pipe open, which
    // empties the captured stdout (see `GRANDCHILD_MARKER`).
    #[cfg(unix)]
    assert!(
        out.stdout.contains(GRANDCHILD_MARKER),
        "the grandchild marker is missing from stdout: a process of the subtree \
         survived the timeout and held the pipe open (stopped or running); \
         stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
```

Update the test's doc comment. Replace the sentence "On unix this guards the
long-standing `process_group(0)` + `SIGKILL` path, which had no test of its
own." with:

```rust
/// On unix this guards the `process_group(0)` + group `SIGSTOP`, then
/// `SIGKILL`, path (SMA-710): the `alive` sentinel catches a survivor that runs,
/// and the stdout marker catches one that stays stopped.
```

- [ ] **Step 4: Verify that the test passes now, and fails for a direct-child-only kill (RED by mutation)**

Run: `cargo test -p paigasus-helikon-tools --test exec_timeout_portable`
Expected: PASS, all tests (about 13 s for the subtree test).

Then temporarily edit `crates/paigasus-helikon-tools/src/exec/mod.rs` near
line 362. Change `libc::kill(-(pid as i32), libc::SIGKILL)` to
`libc::kill(pid as i32, libc::SIGKILL)` (no minus sign: only the direct child).

Run: `cargo test -p paigasus-helikon-tools --test exec_timeout_portable timeout_kills_the_whole_subtree`
Expected: FAIL. The `alive` assertion or the marker assertion fails.

Revert the mutation: `git diff crates/paigasus-helikon-tools/src/exec/mod.rs`
must print nothing.

- [ ] **Step 5: fmt and clippy**

Run: `cargo fmt --all && cargo clippy -p paigasus-helikon-tools --all-features --all-targets -- -D warnings`
Expected: no diff from fmt after the run, clippy clean.

- [ ] **Step 6: Review the `cfg` gating**

Run: `grep -n 'GRANDCHILD_MARKER\|libc' crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs`
Expected: every line that defines or uses `GRANDCHILD_MARKER` is inside a
`#[cfg(unix)]` item or statement, and there is no `libc` use.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-tools/tests/exec_timeout_portable.rs
git commit -m "test(tools): SMA-710 detect stopped survivors and a direct-child-only kill

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Stop the group before the kill, and document the mechanism

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs` (the timeout arm
  near line 355-364, the reap comment near lines 398-404, the
  `ExecOutput::timed_out` rustdoc near lines 147-161, and a new function
  after `build_command`)
- Modify: `docs/book/src/concepts/tools.md` (lines 383-394)

**Interfaces:**
- Consumes: the Task 1 test and its failure text
  `the grandchild marker is missing from stdout`.
- Produces: `#[cfg(unix)] fn kill_process_group(pgid: u32)` (module
  private).

- [ ] **Step 1: Add `kill_process_group`**

In `exec/mod.rs`, directly after the `build_command` function, add:

```rust
/// Kill every process in the process group `pgid` on a timeout (SMA-710).
///
/// Stops the group first, then kills it, so that the death of one member cannot
/// wake another member into user code before that member is also killed. A
/// single group `SIGKILL` is not enough on macOS: the kernel signals members
/// one at a time, and a parent woken from `wait4` by a dead child could run its
/// next command before its own `SIGKILL` took effect.
///
/// The two `kill` calls must stay back-to-back and synchronous: never put an
/// `.await`, a lock, or other work between them. No `SIGCONT` is sent: `SIGKILL`
/// terminates a stopped process.
#[cfg(unix)]
fn kill_process_group(pgid: u32) {
    // pid < 4_194_304 on supported platforms, so the cast and negation are valid.
    let target = -(pgid as i32);
    // The result is deliberately ignored: `SIGKILL` below is sent whatever
    // happens here, so the kill is never weaker than a bare `SIGKILL`.
    // SAFETY: `kill` has no memory-safety preconditions.
    let _ = unsafe { libc::kill(target, libc::SIGSTOP) };
    // SAFETY: as above.
    if unsafe { libc::kill(target, libc::SIGKILL) } != 0 {
        let err = std::io::Error::last_os_error();
        // ESRCH: the group is already gone. EPERM on macOS: only zombies remain
        // in the group. Neither leaves a live survivor.
        if !matches!(err.raw_os_error(), Some(libc::ESRCH) | Some(libc::EPERM)) {
            tracing::warn!(
                target: "paigasus::tools::exec",
                error = %err,
                pgid,
                "process group SIGKILL failed after SIGSTOP; processes of the timed-out \
                 command may remain stopped"
            );
        }
    }
}
```

- [ ] **Step 2: Call it from the timeout arm**

Replace this block in the timeout arm:

```rust
            #[cfg(unix)]
            {
                if let Some(pid) = pgid {
                    // SAFETY: pid < 4_194_304 on supported platforms, so the cast
                    // and negation are valid; ESRCH (group already gone) is benign.
                    let _ = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
                }
            }
```

with:

```rust
            #[cfg(unix)]
            {
                if let Some(pid) = pgid {
                    kill_process_group(pid);
                }
            }
```

In the reap comment below it, change "on unix the child can still win the race
to exit normally before our SIGKILL lands" to "on unix the child can still win
the race to exit normally before the group `SIGSTOP`/`SIGKILL` lands".

- [ ] **Step 3: Update the `ExecOutput::timed_out` rustdoc**

Replace the paragraph

```rust
    /// On unix and Windows a timeout kills the whole spawned subtree, not just
    /// the direct child: a process group `SIGKILL` on unix, a Job Object
    /// termination on Windows. On any other target no subtree mechanism is
    /// available and only the direct child is killed.
```

with:

```rust
    /// On unix and Windows a timeout kills the whole spawned subtree, not just
    /// the direct child. On unix the process group gets `SIGSTOP`, then
    /// `SIGKILL`, so that the death of one member cannot wake another member into
    /// user code before that member is also killed. On Windows the Job Object is
    /// terminated. On any other target no subtree mechanism is available and only
    /// the direct child is killed.
    ///
    /// Accepted gaps on unix: a `ptrace` tracer or a waiter that uses
    /// `WUNTRACED` sees the stop and can act on it; on macOS, a process that a
    /// member forks between the two signals is not stopped and can run until the
    /// `SIGKILL` reaches it; a process that leaves the group (`setpgid`,
    /// `setsid`) survives; and if the `SIGKILL` fails, the survivors stay stopped
    /// and a warning is emitted on the `paigasus::tools::exec` target.
```

Do not add an intra-doc link to `kill_process_group` (a private item; the
`-D warnings` docs gate rejects it).

- [ ] **Step 4: Update the mdBook**

In `docs/book/src/concepts/tools.md`, replace

```markdown
When a command exceeds its timeout the **whole spawned subtree** is killed on unix
and Windows, not just the shell: a process-group `SIGKILL` on unix, a Job Object
termination on Windows. On any other target there is no subtree mechanism and only
the direct child is killed. `ExecOutput::timed_out` is `true` and `exit_code` is
`None` on every platform — a killed process has no meaningful exit code.
```

with:

```markdown
When a command exceeds its timeout the **whole spawned subtree** is killed on unix
and Windows, not just the shell. On unix the process group gets `SIGSTOP`, then
`SIGKILL`, so that the death of one member cannot wake another member into user
code before that member is also killed. On Windows the Job Object is terminated.
On any other target there is no subtree mechanism and only the direct child is
killed. `ExecOutput::timed_out` is `true` and `exit_code` is `None` on every
platform — a killed process has no meaningful exit code.

Accepted gaps on unix: a `ptrace` tracer or a waiter that uses `WUNTRACED` sees
the stop and can act on it; on macOS, a process that a member forks between the
two signals is not stopped and can run until the `SIGKILL` reaches it; a process
that leaves the group (`setpgid`, `setsid`) survives; and if the `SIGKILL` fails,
the survivors stay stopped and a warning is emitted on the
`paigasus::tools::exec` target.
```

Leave the Windows gap paragraph that follows unchanged.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p paigasus-helikon-tools --test exec_timeout_portable`
Expected: PASS, all tests.

Run: `cargo test -p paigasus-helikon-tools --all-features`
Expected: PASS (this includes the Seatbelt backend tests on macOS and
`bash.rs`, `host_backend.rs`).

- [ ] **Step 6: Mutation checks (record the output for the PR)**

Do each mutation, run the subtree test, record the result, then revert.

1. Delete the `SIGKILL` call and its `if` block (leave only `SIGSTOP`).
   Run: `cargo test -p paigasus-helikon-tools --test exec_timeout_portable timeout_kills_the_whole_subtree`
   Expected: FAIL with `the grandchild marker is missing from stdout`.
   Afterwards, make sure no stopped `sleep 4` or `sh …grandchild.sh` process is
   left: `pgrep -fl 'grandchild.sh|sleep 4'`, and kill any with `kill -9`.
2. Change the `SIGKILL` target from `target` to `pgid as i32` (direct child
   only; keep the group `SIGSTOP`).
   Run the same test. Expected: FAIL (marker missing or `alive` present).
   Clean up with `pgrep` as in 1.
3. Delete the `SIGSTOP` call (the old behavior).
   Run the same test. Expected: PASS. This is expected: the race does not
   reproduce locally, so the test does not guard the race itself.

After all three: `git diff crates/paigasus-helikon-tools/src/exec/mod.rs`
shows only the Step 1-3 changes.

- [ ] **Step 7: Gates**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-tools --all-features --no-deps
mdbook build docs/book
npx markdownlint-cli2
```

Expected: all clean. (`mdbook` runs the link check with
`warning-policy = "error"`.)

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-tools/src/exec/mod.rs docs/book/src/concepts/tools.md
git commit -m "fix(tools): SMA-710 stop the process group before the timeout kill

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
