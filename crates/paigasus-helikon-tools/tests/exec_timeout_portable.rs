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

/// A timed-out run reports `exit_code: None` on every platform, per the contract
/// documented on `ExecOutput::exit_code`.
///
/// Regression guard for SMA-569: the grace-period reap used to return the child's
/// own code. On unix SIGKILL masks that (`ExitStatus::code()` is `None` for a
/// signal-terminated process), but on Windows `start_kill()` is `TerminateProcess`,
/// which assigns a real code — so a timed-out run reported `timed_out: true`
/// alongside a non-null `exit_code`.
#[tokio::test]
async fn timeout_reports_no_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(Duration::from_millis(200))
        .build();

    // Budget mirrors `tests/bash.rs` — backend timeout (0.2s) + GRACE reap (5s) +
    // one GRACE reader drain (5s) is ~10.2s worst case; 20s leaves CI headroom
    // without masking a real hang.
    let out = tokio::time::timeout(Duration::from_secs(20), backend.run(ExecRequest::new(HANG)))
        .await
        .expect("run must return promptly, not hang")
        .unwrap();

    assert!(out.timed_out, "the command must outlive the 200ms timeout");
    assert_eq!(
        out.exit_code, None,
        "a process killed by the timeout has no meaningful exit code"
    );
}

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
