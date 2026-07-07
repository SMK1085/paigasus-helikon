//! RhaiTool execution tests.

use paigasus_helikon_cli::rhai_tool::RhaiTool;
use paigasus_helikon_core::{RunContext, Tool};

fn tool(source: &str) -> RhaiTool {
    RhaiTool::new(
        "t",
        "test tool",
        serde_json::json!({"type":"object"}),
        source,
    )
    .unwrap()
}

#[tokio::test]
async fn runs_script_and_maps_json() {
    let t = tool("fn run(args) { #{ doubled: args.n * 2 } }");
    let ctx: RunContext<()> = RunContext::ephemeral(());
    let out = t
        .invoke(&ctx.to_tool_context(), serde_json::json!({"n": 21}))
        .await
        .unwrap();
    assert_eq!(out.content, serde_json::json!({"doubled": 42}));
}

#[tokio::test]
async fn script_error_is_tool_error_not_panic() {
    let t = tool("fn run(args) { missing_fn() }");
    let ctx: RunContext<()> = RunContext::ephemeral(());
    assert!(t
        .invoke(&ctx.to_tool_context(), serde_json::json!({}))
        .await
        .is_err());
}

#[tokio::test]
async fn operation_limit_stops_runaway_scripts() {
    let t = tool("fn run(args) { let x = 0; loop { x += 1; } }");
    let ctx: RunContext<()> = RunContext::ephemeral(());
    assert!(t
        .invoke(&ctx.to_tool_context(), serde_json::json!({}))
        .await
        .is_err());
}

#[test]
fn compile_error_surfaces_at_construction() {
    assert!(RhaiTool::new("t", "d", serde_json::json!({}), "fn run( {").is_err());
}
