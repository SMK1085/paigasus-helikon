//! Binary smoke tests for `helikon mcp serve` error paths (the happy path
//! blocks serving stdio/HTTP indefinitely and isn't exercised here).

use std::path::Path;
use std::process::Command;

fn fixtures() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"))
}

#[test]
fn mcp_serve_unknown_agent_fails_cleanly() {
    let out = Command::new(env!("CARGO_BIN_EXE_helikon"))
        .current_dir(fixtures())
        .args(["mcp", "serve", "--agent", "nope"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error:"), "stderr:\n{stderr}");
    assert!(stderr.contains("nope"), "stderr:\n{stderr}");
}

#[test]
fn mcp_serve_missing_sidecar_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_helikon"))
        .current_dir(dir.path())
        .args(["mcp", "serve", "--agent", "triage"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error:"), "stderr:\n{stderr}");
}
