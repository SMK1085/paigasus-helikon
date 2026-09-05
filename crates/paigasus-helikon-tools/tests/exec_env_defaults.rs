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

/// Running `env` and checking the exported name set proves the no-leak half of
/// the default allowlist: nothing beyond the allowlist (plus what `sh` injects
/// on its own) reaches the child. It only spot-checks the other half — that
/// `PATH` itself arrives — rather than requiring every allowlisted name to be
/// present, because the *parent* running this test is not guaranteed to have
/// `HOME` set (e.g. some CI/sandboxed invocations), and a name absent from the
/// parent is legitimately absent from the child too (see
/// `HostBackendBuilder::env_allowlist`'s "dropped without diagnostic" note).
/// Requiring full presence would make the test fail for a reason unrelated to
/// SMA-614.
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
/// it cannot expand, so a missing entry shows up as an unexpanded `%NAME%` token
/// in stdout. This fails deterministically without the SMA-614 fix and does not
/// depend on any hypothesis about Winsock.
///
/// The probe is built from [`DEFAULT_ENV_ALLOWLIST`] itself, and each entry is
/// checked individually for its own unexpanded `%NAME%` token (rather than one
/// blanket `stdout.contains('%')`), so: (a) the test can never drift out of
/// sync if the const's entries change, and (b) it stays correct now that
/// `%PATH%` is included — `PATH` is long, machine-dependent, and could in
/// principle contain a literal `%`, which would false-fail a blanket check but
/// can never match a specific `%PATH%` token unless `PATH` itself failed to
/// expand.
#[tokio::test]
#[cfg(windows)]
async fn windows_default_env_expands_every_allowlisted_name() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(Duration::from_secs(10))
        .build();

    let command = format!(
        "echo {}",
        DEFAULT_ENV_ALLOWLIST
            .iter()
            .map(|name| format!("[%{name}%]"))
            .collect::<Vec<_>>()
            .join("")
    );
    let out = backend.run(ExecRequest::new(command)).await.unwrap();

    assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
    for name in DEFAULT_ENV_ALLOWLIST {
        let token = format!("%{name}%");
        assert!(
            !out.stdout.contains(&token),
            "{token} was not expanded, so {name} never reached the child: {}",
            out.stdout.trim()
        );
    }
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
