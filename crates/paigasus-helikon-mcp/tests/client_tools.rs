//! Client-side integration tests against the in-process fixture server.

mod support;

use paigasus_helikon_core::ToolEffect;
use paigasus_helikon_mcp::McpConnectOptions;

#[tokio::test]
async fn eager_tools_expose_names_schemas_and_effects() {
    let handle = support::connect_fixture(McpConnectOptions::new()).await;
    let tools = handle.tools::<()>();
    assert_eq!(tools.len(), 3);

    let echo = tools
        .iter()
        .find(|t| t.name() == "echo")
        .expect("echo tool");
    assert_eq!(echo.description(), "Echo a message back");
    assert_eq!(echo.schema()["properties"]["msg"]["type"], "string");
    assert!(matches!(echo.effect(), ToolEffect::ReadOnly));

    let boom = tools
        .iter()
        .find(|t| t.name() == "boom")
        .expect("boom tool");
    assert!(matches!(boom.effect(), ToolEffect::SideEffect));
}

#[tokio::test]
async fn tool_prefix_is_applied() {
    let handle = support::connect_fixture(McpConnectOptions::new().tool_prefix("fs_")).await;
    let tools = handle.tools::<()>();
    assert!(tools.iter().any(|t| t.name() == "fs_echo"));
    assert!(tools.iter().all(|t| t.name().starts_with("fs_")));
}
