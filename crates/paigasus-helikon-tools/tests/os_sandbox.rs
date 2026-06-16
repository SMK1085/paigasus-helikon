#![allow(missing_docs)]
#![cfg(all(
    feature = "os-sandbox",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

// `ExecutionBackend` is not imported: `build()` returns `Arc<dyn ExecutionBackend>`,
// so `run`/`guarantees` are called on a trait object (no trait import needed).
use paigasus_helikon_tools::{Isolation, OsSandboxBackend, Sandbox};

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
    // Create an AF_INET socket directly (no connect, so no external dependency):
    // the seccomp filter must reject socket(2) with EPERM. We assert on the
    // specific EPERM signature so the test cannot pass vacuously — if python3
    // were missing the output would say "not found" (not "Operation not
    // permitted"), and if the seccomp socket rule regressed the call would
    // succeed and exit 0. (Earlier this used `sh`'s `/dev/tcp`, a bash-ism that
    // dash — CI's `/bin/sh` — fails with ENOENT before ever calling socket(2),
    // which made the test pass even with seccomp removed.)
    let out = backend
        .run(paigasus_helikon_tools::ExecRequest::new(
            "python3 -c 'import socket; socket.socket(socket.AF_INET, socket.SOCK_STREAM)' 2>&1",
        ))
        .await
        .unwrap();
    assert_ne!(
        out.exit_code,
        Some(0),
        "socket(AF_INET) must be denied under default-deny; got: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Operation not permitted") || out.stdout.contains("PermissionError"),
        "expected an EPERM from the seccomp socket filter, not some other failure; got: {}",
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
    assert_eq!(backend.guarantees().network, Isolation::None);
    // With network allowed, creating an AF_INET socket succeeds. Pairs with the
    // deny test to pin behavior in both directions: a seccomp regression that
    // wrongly allowed or wrongly blocked socket(2) fails one of the two.
    let out = backend
        .run(paigasus_helikon_tools::ExecRequest::new(
            "python3 -c 'import socket; s = socket.socket(socket.AF_INET, socket.SOCK_STREAM); s.close(); print(\"ok\")' 2>&1",
        ))
        .await
        .unwrap();
    assert_eq!(
        out.exit_code,
        Some(0),
        "socket(AF_INET) must succeed under allow_network; got: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("ok"),
        "expected ok; got: {}",
        out.stdout
    );
}
