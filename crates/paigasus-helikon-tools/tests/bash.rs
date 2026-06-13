#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use paigasus_helikon_core::{
    CancellationToken, HookRegistry, MemorySession, RunContext, Tool, ToolContext, ToolEffect,
    ToolError, TracerHandle,
};
use paigasus_helikon_tools::{BashTool, Sandbox};

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

#[cfg(unix)]
#[tokio::test]
async fn bash_runs_in_sandbox_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("marker.txt"), "x").unwrap();
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap()).build();
    assert_eq!(tool.name(), "Bash");
    assert_eq!(tool.effect(), ToolEffect::SideEffect);

    let out = tool
        .invoke(&tool_ctx(), serde_json::json!({ "command": "ls" }))
        .await
        .unwrap();
    assert!(out.content["stdout"]
        .as_str()
        .unwrap()
        .contains("marker.txt"));
    assert_eq!(out.content["exit_code"], 0);
    assert_eq!(out.content["timed_out"], false);
}

#[cfg(unix)]
#[tokio::test]
async fn bash_times_out() {
    let tmp = tempfile::tempdir().unwrap();
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(Duration::from_millis(200))
        .build();
    let out = tool
        .invoke(&tool_ctx(), serde_json::json!({ "command": "sleep 5" }))
        .await
        .unwrap();
    assert_eq!(out.content["timed_out"], true);
}

#[cfg(unix)]
#[tokio::test]
async fn bash_truncates_output() {
    let tmp = tempfile::tempdir().unwrap();
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap())
        .max_output_bytes(16)
        .build();
    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "command": "printf 'abcdefghijklmnopqrstuvwxyz'" }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["truncated"], true);
    assert!(out.content["stdout"].as_str().unwrap().len() <= 16);
}

#[cfg(unix)]
#[tokio::test]
async fn bash_denies_blocked_command() {
    let tmp = tempfile::tempdir().unwrap();
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap())
        .deny_commands(["rm"])
        .build();
    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "command": "rm -rf /" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[cfg(unix)]
#[tokio::test]
async fn bash_timeout_with_background_process_does_not_hang() {
    let tmp = tempfile::tempdir().unwrap();
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap())
        .timeout(Duration::from_millis(300))
        .build();
    // The foreground `sleep 30` outlives the timeout; the backgrounded one
    // would, before the group-kill fix, hold the stdout pipe open and hang
    // invoke forever. The outer timeout guards against a regression.
    // Worst case with the concurrent drain: tool timeout (0.3s) + GRACE reap
    // (5s) + one GRACE reader drain (5s) ≈ 10.3s. 20s leaves headroom for slow
    // CI / slow SIGKILL delivery without masking a real hang.
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        tool.invoke(
            &tool_ctx(),
            serde_json::json!({ "command": "sleep 30 & sleep 30" }),
        ),
    )
    .await;
    let out = result
        .expect("invoke must return promptly, not hang")
        .unwrap();
    assert_eq!(out.content["timed_out"], true);
}

#[cfg(unix)]
#[tokio::test]
async fn bash_allow_commands_blocks_unlisted() {
    let tmp = tempfile::tempdir().unwrap();
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap())
        .allow_commands(["echo"])
        .build();
    let ok = tool
        .invoke(&tool_ctx(), serde_json::json!({ "command": "echo hi" }))
        .await
        .unwrap();
    assert_eq!(ok.content["exit_code"], 0);
    let err = tool
        .invoke(&tool_ctx(), serde_json::json!({ "command": "ls" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[cfg(unix)]
#[tokio::test]
async fn bash_env_is_scrubbed() {
    let tmp = tempfile::tempdir().unwrap();
    // Override the allowlist to PATH only, so HOME (set in the parent env) is
    // scrubbed from the child.
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap())
        .env_allowlist(["PATH"])
        .build();
    let out = tool
        .invoke(
            &tool_ctx(),
            serde_json::json!({ "command": "echo \"home=[$HOME]\"" }),
        )
        .await
        .unwrap();
    assert_eq!(out.content["stdout"].as_str().unwrap().trim(), "home=[]");
}

#[cfg(unix)]
#[tokio::test]
async fn bash_reports_nonzero_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let tool: BashTool = BashTool::builder(Sandbox::open(tmp.path()).unwrap()).build();
    let out = tool
        .invoke(&tool_ctx(), serde_json::json!({ "command": "exit 3" }))
        .await
        .unwrap();
    assert_eq!(out.content["exit_code"], 3);
    assert_eq!(out.content["timed_out"], false);
}
