#![allow(missing_docs)]

use paigasus_helikon_tools::{Sandbox, SandboxError};

#[test]
fn open_succeeds_on_existing_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::open(tmp.path()).expect("open sandbox");
    assert_eq!(sandbox.root(), tmp.path().canonicalize().unwrap());
}

#[test]
fn open_fails_on_missing_dir() {
    let err = Sandbox::open("/no/such/dir/anywhere-xyz").unwrap_err();
    assert!(matches!(err, SandboxError::Open { .. }));
}

#[test]
fn sandbox_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Sandbox>();
}
