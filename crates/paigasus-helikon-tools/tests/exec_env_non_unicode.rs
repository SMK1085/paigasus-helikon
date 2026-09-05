//! Regression guard: `spawn_capped` must pass an allowlisted variable through
//! even when its value is not valid Unicode. `std::env::var` returns `Err` for
//! those and the value is silently dropped — the same silent-no-op failure mode
//! that made `HOME` a no-op on Windows (SMA-614).
//!
//! This lives in its own file on purpose: it calls `std::env::set_var`, which is
//! not thread-safe, and one test per binary means no concurrent reader.
#![cfg(unix)]
#![allow(missing_docs)]

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use paigasus_helikon_tools::{ExecRequest, HostBackend, Sandbox};

#[tokio::test]
async fn non_unicode_env_value_reaches_the_child() {
    // 0xFF is not valid UTF-8, so `std::env::var` would return Err here.
    std::env::set_var("SMA614_NON_UNICODE", OsString::from_vec(vec![0xff]));

    let tmp = tempfile::tempdir().unwrap();
    let backend = HostBackend::builder(Sandbox::open(tmp.path()).unwrap())
        .env_allowlist(["PATH", "SMA614_NON_UNICODE"])
        .build();

    let out = backend
        .run(ExecRequest::new(r#"test -n "$SMA614_NON_UNICODE""#))
        .await
        .unwrap();

    assert_eq!(
        out.exit_code,
        Some(0),
        "a non-Unicode value must not be silently dropped; stderr: {}",
        out.stderr
    );
}
