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

/// `HostBackend`'s default allowlist is `["PATH", "HOME"]` — unix-shaped.
#[cfg(unix)]
const ENV_ALLOWLIST: &[&str] = &["PATH", "HOME"];

/// On Windows `HOME` does not exist, and `spawn_capped` calls `env_clear()`, so
/// without `SystemRoot` Winsock cannot resolve its provider DLLs: `ping` would
/// exit in milliseconds, before the timeout ever fires.
#[cfg(windows)]
const ENV_ALLOWLIST: &[&str] = &["PATH", "SystemRoot", "windir", "PATHEXT", "TEMP", "TMP"];

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
        .env_allowlist(ENV_ALLOWLIST.iter().copied())
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
