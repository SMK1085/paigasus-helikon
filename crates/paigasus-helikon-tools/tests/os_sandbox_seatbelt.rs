#![allow(missing_docs)]
#![cfg(all(feature = "os-sandbox", target_os = "macos"))]

// `ExecutionBackend` is not imported: `build()` returns `Arc<dyn ExecutionBackend>`,
// so trait methods resolve on the trait object without the trait in scope.
use paigasus_helikon_tools::{Isolation, OsSandboxBackend, Sandbox};

/// Skip (loudly) when Seatbelt can't be established here — UNLESS
/// `HELIKON_REQUIRE_SANDBOX=1`, in which case an unavailable sandbox is a hard
/// failure, so a CI runner that stops enforcing turns the build red, never green.
/// Returns true if the caller should `return`.
fn seatbelt_unavailable(root: &std::path::Path) -> bool {
    if OsSandboxBackend::builder(Sandbox::open(root).unwrap())
        .build()
        .is_ok()
    {
        return false;
    }
    if std::env::var("HELIKON_REQUIRE_SANDBOX").as_deref() == Ok("1") {
        panic!("HELIKON_REQUIRE_SANDBOX=1 but Seatbelt could not be established on this host");
    }
    eprintln!("SKIP: Seatbelt unavailable on this host; os-sandbox AC not exercised");
    true
}

#[tokio::test]
async fn os_sandbox_builds_and_reports_guarantees() {
    let tmp = tempfile::tempdir().unwrap();
    if seatbelt_unavailable(tmp.path()) {
        return;
    }
    let backend = OsSandboxBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .build()
        .expect("Seatbelt available");
    let g = backend.guarantees();
    assert_eq!(g.filesystem, Isolation::OsKernel);
    assert_eq!(g.syscalls, Isolation::None);
    assert_eq!(g.network, Isolation::OsKernel); // default deny
    assert_eq!(g.label, "os-sandbox (seatbelt)");
}
