#![allow(missing_docs)]

use paigasus_helikon_core::{
    CancellationToken, HookRegistry, MemorySession, RunContext, Tool, ToolContext, ToolEffect,
    ToolError, TracerHandle,
};
use paigasus_helikon_tools::{Sandbox, SandboxError};
use std::sync::Arc;

/// Build a `ToolContext<()>` for calling a tool's `invoke` directly.
fn tool_ctx() -> ToolContext<()> {
    let run_ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );
    run_ctx.to_tool_context()
}

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

#[tokio::test]
async fn read_returns_file_content() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "hello sandbox").unwrap();
    let sandbox = Sandbox::open(tmp.path()).unwrap();
    let tool: ReadTool = ReadTool::new(sandbox);

    assert_eq!(tool.name(), "Read");
    assert_eq!(tool.effect(), ToolEffect::ReadOnly);

    let out = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "notes.txt" }))
        .await
        .unwrap();
    assert_eq!(out.content["content"], "hello sandbox");
}

#[tokio::test]
async fn read_rejects_parent_escape() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "../secret" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn read_rejects_absolute_path() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "/etc/passwd" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn read_non_utf8_is_denied() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("bad.bin"), [0xff, 0xfe, 0x00]).unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "bad.bin" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn read_missing_file_is_other() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "nope.txt" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Other(_)));
}

#[cfg(unix)]
#[tokio::test]
async fn read_rejects_escaping_symlink() {
    use paigasus_helikon_tools::ReadTool;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        tmp.path().join("link.txt"),
    )
    .unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "link.txt" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn read_with_offset_and_limit_windows_lines() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("multi.txt"), "l1\nl2\nl3\nl4\n").unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());
    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "multi.txt", "offset": 2, "limit": 2 }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["content"], "l2\nl3");
}

#[tokio::test]
async fn read_offset_beyond_eof_returns_empty() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("multi.txt"), "l1\nl2\n").unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());
    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "multi.txt", "offset": 100 }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["content"], "");
}

#[tokio::test]
async fn read_full_preserves_trailing_newline_but_window_normalizes() {
    use paigasus_helikon_tools::ReadTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("t.txt"), "a\nb\n").unwrap();
    let tool: ReadTool = ReadTool::new(Sandbox::open(tmp.path()).unwrap());
    let full = tool
        .invoke(&tool_ctx(), serde_json::json!({ "path": "t.txt" }))
        .await
        .unwrap();
    assert_eq!(full.content["content"], "a\nb\n");
    let win = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "t.txt", "limit": 2 }),
        )
        .await
        .unwrap();
    assert_eq!(win.content["content"], "a\nb");
}

#[tokio::test]
async fn write_creates_file_and_parents() {
    use paigasus_helikon_tools::WriteTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: WriteTool = WriteTool::new(Sandbox::open(tmp.path()).unwrap());
    assert_eq!(tool.effect(), ToolEffect::Write);

    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "sub/dir/out.txt", "content": "data" }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["bytes_written"], 4);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("sub/dir/out.txt")).unwrap(),
        "data"
    );
}

#[tokio::test]
async fn write_rejects_escape() {
    use paigasus_helikon_tools::WriteTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: WriteTool = WriteTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "../evil.txt", "content": "x" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn write_overwrites_existing_file() {
    use paigasus_helikon_tools::WriteTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "original").unwrap();
    let tool: WriteTool = WriteTool::new(Sandbox::open(tmp.path()).unwrap());
    tool.invoke(
        &tool_ctx(),
        serde_json::json!({ "path": "f.txt", "content": "replaced" }),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "replaced"
    );
}

#[tokio::test]
async fn write_top_level_file_no_parent() {
    use paigasus_helikon_tools::WriteTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: WriteTool = WriteTool::new(Sandbox::open(tmp.path()).unwrap());
    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "top.txt", "content": "hi" }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["bytes_written"], 2);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("top.txt")).unwrap(),
        "hi"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn write_rejects_escaping_symlink() {
    use paigasus_helikon_tools::WriteTool;
    let outside = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path().join("pwn.txt"), tmp.path().join("link.txt"))
        .unwrap();
    let tool: WriteTool = WriteTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "link.txt", "content": "x" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
    // The write must NOT have escaped the sandbox.
    assert!(!outside.path().join("pwn.txt").exists());
}

#[tokio::test]
async fn edit_replaces_unique_string() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "alpha beta gamma").unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());
    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "f.txt", "old_string": "beta", "new_string": "BETA" }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["replacements"], 1);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "alpha BETA gamma"
    );
}

#[tokio::test]
async fn edit_not_found_is_denied() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "alpha").unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "f.txt", "old_string": "zzz", "new_string": "x" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn edit_non_unique_without_replace_all_is_denied() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "x x x").unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "f.txt", "old_string": "x", "new_string": "y" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn edit_replace_all_replaces_every_occurrence() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "x x x").unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());
    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({
                "path": "f.txt", "old_string": "x", "new_string": "y", "replace_all": true
            }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["replacements"], 3);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "y y y"
    );
}

#[tokio::test]
async fn edit_rejects_parent_escape() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "../evil.txt", "old_string": "a", "new_string": "b" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn edit_missing_file_is_other() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "nope.txt", "old_string": "a", "new_string": "b" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Other(_)));
}

#[tokio::test]
async fn edit_empty_old_string_is_denied() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "abc").unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "f.txt", "old_string": "", "new_string": "X", "replace_all": true }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "abc"
    );
}

#[tokio::test]
async fn edit_preserves_trailing_newline() {
    use paigasus_helikon_tools::EditTool;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "alpha\nbeta\n").unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());
    tool.invoke(
        &tool_ctx(),
        serde_json::json!({ "path": "f.txt", "old_string": "beta", "new_string": "BETA" }),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
        "alpha\nBETA\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn edit_rejects_escaping_symlink() {
    use paigasus_helikon_tools::EditTool;
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "secret data").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        tmp.path().join("link.txt"),
    )
    .unwrap();
    let tool: EditTool = EditTool::new(Sandbox::open(tmp.path()).unwrap());
    let err = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "path": "link.txt", "old_string": "secret", "new_string": "PWNED" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
    assert_eq!(
        std::fs::read_to_string(outside.path().join("secret.txt")).unwrap(),
        "secret data"
    );
}
