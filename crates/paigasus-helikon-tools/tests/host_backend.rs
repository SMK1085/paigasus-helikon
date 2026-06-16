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
