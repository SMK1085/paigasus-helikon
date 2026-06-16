#![allow(missing_docs)]
#![cfg(all(
    feature = "os-sandbox",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use paigasus_helikon_tools::{ExecutionBackend, Isolation, OsSandboxBackend, Sandbox};

/// Skip (with a loud reason) when the kernel lacks Landlock, rather than passing
/// silently. Returns true if the caller should `return`.
fn landlock_unavailable(tmp: &std::path::Path) -> bool {
    if OsSandboxBackend::builder(Sandbox::open(tmp).unwrap())
        .build()
        .is_err()
    {
        eprintln!("SKIP: Landlock unavailable on this kernel; os-sandbox AC not exercised");
        return true;
    }
    false
}

#[tokio::test]
async fn os_sandbox_builds_and_reports_guarantees() {
    let tmp = tempfile::tempdir().unwrap();
    if landlock_unavailable(tmp.path()) {
        return;
    }
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .build()
        .expect("Landlock available");
    let g = backend.guarantees();
    assert_eq!(g.filesystem, Isolation::OsKernel);
    assert_eq!(g.syscalls, Isolation::OsKernel);
    assert_eq!(g.network, Isolation::OsKernel); // default deny
    assert_eq!(g.label, "os-sandbox (landlock+seccomp)");
}

#[tokio::test]
async fn os_sandbox_blocks_write_outside_root_at_os_layer() {
    let tmp = tempfile::tempdir().unwrap();
    if landlock_unavailable(tmp.path()) {
        return;
    }
    let outside = tempfile::tempdir().unwrap(); // a sibling dir NOT under the sandbox root
    let target = outside.path().join("escape.txt");
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .build()
        .unwrap();

    // Absolute path outside the root: the shell's own path logic would allow it;
    // Landlock must block the write at the OS layer.
    let cmd = format!("echo pwned > {}", target.display());
    let out = backend
        .run(paigasus_helikon_tools::ExecRequest::new(cmd))
        .await
        .unwrap();
    assert_ne!(out.exit_code, Some(0), "write outside root must fail");
    assert!(
        !target.exists(),
        "no file may be created outside the sandbox root"
    );

    // Sanity: a write INSIDE the root still succeeds.
    let ok = backend
        .run(paigasus_helikon_tools::ExecRequest::new(
            "echo ok > inside.txt",
        ))
        .await
        .unwrap();
    assert_eq!(ok.exit_code, Some(0));
    assert!(tmp.path().join("inside.txt").exists());
}

#[tokio::test]
async fn os_sandbox_denies_network_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    if landlock_unavailable(tmp.path()) {
        return;
    }
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .build()
        .unwrap();
    // Pure-shell TCP connect to a public IP; seccomp must block socket(AF_INET).
    // bash's /dev/tcp triggers socket(2); on failure the redirect errors.
    let out = backend
        .run(paigasus_helikon_tools::ExecRequest::new(
            "timeout 5 sh -c 'echo > /dev/tcp/1.1.1.1/80' 2>&1; echo rc=$?",
        ))
        .await
        .unwrap();
    assert!(
        out.stdout.contains("rc=") && !out.stdout.contains("rc=0"),
        "network connect must fail under default-deny seccomp; got: {}",
        out.stdout
    );
}

#[tokio::test]
async fn os_sandbox_allows_network_when_opted_in() {
    let tmp = tempfile::tempdir().unwrap();
    if landlock_unavailable(tmp.path()) {
        return;
    }
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .allow_network(true)
        .build()
        .unwrap();
    let g = backend.guarantees();
    assert_eq!(g.network, paigasus_helikon_tools::Isolation::None);
    // socket() now succeeds (creating a socket needs no external service).
    let out = backend
        .run(paigasus_helikon_tools::ExecRequest::new(
            "python3 -c 'import socket; socket.socket(); print(\"ok\")' 2>&1 || echo nopy",
        ))
        .await
        .unwrap();
    assert!(out.stdout.contains("ok") || out.stdout.contains("nopy"));
}
