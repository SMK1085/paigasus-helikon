//! Exec tests that spawn a **real** child through `spawn_capped` and are NOT
//! `cfg`-gated. Every other real-process exec test in this crate is unix-only
//! (`tests/host_backend.rs` is file-level `#![cfg(unix)]`); `tests/exec_backend.rs`
//! is ungated but drives a `MockBackend` and never spawns anything. This file is
//! the one that must compile and pass on Windows too — keep it that way.
#![allow(missing_docs)]

use std::path::Path;
use std::time::Duration;

use paigasus_helikon_tools::{ExecRequest, HostBackend, Sandbox};

/// A command that blocks well past the backend timeout. Per-platform because
/// `spawn_capped` runs `sh -c` on unix and `cmd /C` on Windows.
#[cfg(unix)]
const HANG: &str = "sleep 5";

/// `ping` ships with every Windows install; `-n 5` blocks ~4s, 20x the 200ms
/// timeout. Output goes to `NUL` to keep the captured streams clean.
///
/// Note this redirect does **not** stop `ping` holding our stdout/stderr pipe
/// write ends: `CreateProcess` runs with `bInheritHandles = TRUE`, so every
/// inheritable handle in `cmd.exe` is duplicated into `ping` regardless of where
/// `cmd.exe` points its `hStdOutput`. Before SMA-613 that inherited writer is
/// exactly why this test took ~4s on Windows against ~0.2s on unix — the reader
/// drain waited out the orphaned grandchild.
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

/// Builds the script body that writes `started` immediately, then `alive` only
/// after a delay that outlives the backend timeout. Two sentinels, not one: a
/// test that asserted only "`alive` is absent" would pass for free every time
/// the grandchild failed to launch at all (wrong cwd, script not written,
/// `.cmd` misparsed) — a false green on a Windows-only path whose sole
/// behavioural gate is one CI job.
///
/// `started` and `alive` are taken as ABSOLUTE paths and baked into the script
/// with `format!`, rather than written as bare relative filenames the script
/// would resolve against its own working directory. `Sandbox::open` canonicalizes
/// its root (`src/sandbox.rs`) and that canonical path becomes the child's cwd;
/// on Windows `std::fs::canonicalize` returns a verbatim `\\?\C:\...` path, and
/// `cmd.exe` can mistake a cwd starting `\\` for a UNC path, print "UNC paths are
/// not supported. Defaulting to Windows directory," and silently reset its
/// working directory to `%SystemRoot%`. A relative sentinel path would then
/// resolve nowhere near the sandbox, and the test would fail on its positive
/// control (`started`) for a reason that has nothing to do with the subtree
/// kill under test. Absolute paths make the script's behaviour independent of
/// whatever `cmd.exe` decides its cwd is.
#[cfg(unix)]
fn grandchild_script(started: &Path, alive: &Path) -> String {
    format!(
        "echo started > \"{}\"\nsleep 4\necho alive > \"{}\"\n",
        started.display(),
        alive.display()
    )
}

/// CRLF is explicit: the file is created at runtime, so nothing in
/// `.gitattributes` governs it, and `cmd.exe` batch parsing is the wrong place
/// to discover an LF assumption. See [`grandchild_script`] (unix) for why
/// `started`/`alive` are absolute paths baked in with `format!` rather than bare
/// relative names.
///
/// The redirection targets are quoted (`echo started>"<path>"`) so a temp
/// directory containing a space in one of its components — e.g. a Windows user
/// profile path with a space in the username — does not get split by `cmd.exe`
/// into a bogus extra token.
#[cfg(windows)]
fn grandchild_script(started: &Path, alive: &Path) -> String {
    format!(
        "@echo off\r\necho started>\"{}\"\r\nping -n 5 127.0.0.1 >NUL\r\necho alive>\"{}\"\r\n",
        started.display(),
        alive.display()
    )
}

/// A command whose sentinel-writer is a **grandchild** of the shell `spawn_capped`
/// spawns, so killing only the direct child leaves it running.
///
/// `script` is invoked by ABSOLUTE path (quoted, for the same reason as the
/// sentinel paths above: `cmd.exe`'s UNC misdetection on a canonicalized
/// Windows cwd would otherwise leave `cmd /C <relative-name>` unable to find
/// the file at all) rather than relying on the child's working directory.
///
/// The trailing `; true` is load-bearing: without it `sh -c` applies its
/// single-command `exec` optimisation and replaces the outer shell, collapsing
/// the tree so the writer is a child, not a grandchild — and the test would
/// silently stop guarding two levels.
#[cfg(unix)]
fn spawns_grandchild(script: &Path) -> String {
    format!("sh \"{}\"; true", script.display())
}

/// `build_command` turns this into `cmd /C "cmd /C \"<script>\""`, so the outer
/// `cmd.exe` spawns an inner `cmd.exe` that runs the batch and waits.
///
/// Deliberately not `start /B`: `START` is the documented `CREATE_BREAKAWAY_FROM_JOB`
/// case, and a job created with no limit flags is exactly the configuration it
/// interacts with — a test that passed because `start` broke away would be worse
/// than no test. A plain nested `cmd` needs no such escape.
#[cfg(windows)]
fn spawns_grandchild(script: &Path) -> String {
    format!("cmd /C \"{}\"", script.display())
}

/// A timed-out run kills the whole spawned subtree, not just the direct child.
///
/// Regression guard for SMA-613. On unix this guards the long-standing
/// `process_group(0)` + `SIGKILL` path, which had no test of its own. On Windows
/// it guards the Job Object kill that replaced a bare `TerminateProcess` against
/// `cmd.exe`, which left every grandchild running to completion.
#[tokio::test]
async fn timeout_kills_the_whole_subtree() {
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join(GRANDCHILD_SCRIPT_NAME);
    let started_path = tmp.path().join("started");
    let alive_path = tmp.path().join("alive");
    std::fs::write(&script_path, grandchild_script(&started_path, &alive_path)).unwrap();

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
        backend.run(ExecRequest::new(spawns_grandchild(&script_path))),
    )
    .await
    .expect("run must return promptly, not hang")
    .unwrap();

    assert!(
        out.timed_out,
        "the command must outlive the 1s timeout; stdout={:?} stderr={:?}",
        out.stdout, out.stderr
    );

    // Wait past the moment a surviving grandchild would have written `alive`.
    tokio::time::sleep(Duration::from_secs(6)).await;

    assert!(
        started_path.exists(),
        "positive control failed: the grandchild never ran, so the `alive` \
         assertion below would have passed for free; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
    assert!(
        !alive_path.exists(),
        "the grandchild outlived the timeout: the subtree was not killed; \
         stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
}
