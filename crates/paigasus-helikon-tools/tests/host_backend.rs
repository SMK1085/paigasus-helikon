#![allow(missing_docs)]
#![cfg(unix)]

use paigasus_helikon_tools::{ExecRequest, HostBackend, Isolation, Sandbox};

#[tokio::test]
async fn host_backend_runs_command_in_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("marker.txt"), "x").unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap()).build();

    let out = backend.run(ExecRequest::new("ls")).await.unwrap();
    assert!(out.stdout.contains("marker.txt"));
    assert_eq!(out.exit_code, Some(0));
    assert!(!out.timed_out);
}

#[tokio::test]
async fn host_backend_guarantees_report_no_containment() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap()).build();
    let g = backend.guarantees();
    assert_eq!(g.filesystem, Isolation::None);
    assert_eq!(g.network, Isolation::None);
    assert_eq!(g.syscalls, Isolation::None);
    assert_eq!(g.label, "host (no containment)");
}

#[tokio::test]
async fn host_backend_env_is_scrubbed_to_allowlist() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .env_allowlist(["PATH"]) // drop HOME
        .build();
    let out = backend
        .run(ExecRequest::new("echo HOME=$HOME"))
        .await
        .unwrap();
    assert!(out.stdout.contains("HOME="));
    assert!(!out.stdout.contains("HOME=/")); // HOME unset → empty
}

#[tokio::test]
async fn host_backend_rlimit_cpu_kills_spin_loop() {
    let tmp = tempfile::tempdir().unwrap();
    // Generous wall timeout so the CPU limit (not the timeout) is what fires.
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(std::time::Duration::from_secs(60))
        .rlimits(paigasus_helikon_tools::ResourceLimits {
            cpu_seconds: Some(1),
            file_size_bytes: None,
            address_space_bytes: None,
        })
        .build();
    // Busy loop: with RLIMIT_CPU=1 the kernel sends SIGXCPU within ~1s.
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        backend.run(ExecRequest::new("while true; do :; done")),
    )
    .await
    .expect("must return well under the 60s wall timeout")
    .unwrap();
    // Killed by signal → no clean exit code, and not via our wall-timeout path.
    assert_eq!(out.exit_code, None);
    assert!(!out.timed_out, "CPU rlimit, not wall timeout, should fire");
}

#[tokio::test]
async fn host_backend_rlimit_fsize_blocks_large_write() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .rlimits(paigasus_helikon_tools::ResourceLimits {
            cpu_seconds: None,
            file_size_bytes: Some(1024), // 1 KiB cap
            address_space_bytes: None,
        })
        .build();
    // Writing 1 MiB exceeds the 1 KiB RLIMIT_FSIZE → SIGXFSZ / write error.
    let out = backend
        .run(ExecRequest::new("head -c 1048576 /dev/zero > big.bin"))
        .await
        .unwrap();
    assert_ne!(out.exit_code, Some(0), "the oversized write must fail");
    let written = std::fs::metadata(tmp.path().join("big.bin"))
        .map(|m| m.len())
        .unwrap_or(0);
    assert!(written <= 1024, "file must be capped at the rlimit");
}
