#![allow(missing_docs)]
#![cfg(all(feature = "os-sandbox", target_os = "linux"))]

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
