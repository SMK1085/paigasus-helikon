//! Lazy mode: placeholder schemas + `search_tools` meta-tool.

mod support;

use paigasus_helikon_core::ToolEffect;
use paigasus_helikon_mcp::McpConnectOptions;

#[tokio::test]
async fn lazy_tools_advertise_placeholder_schema_plus_search_tool() {
    let handle = support::connect_fixture(McpConnectOptions::new().lazy(true)).await;
    let tools = handle.tools::<()>();
    // 4 fixture tools + search_tools
    assert_eq!(tools.len(), 5);

    let echo = tools.iter().find(|t| t.name() == "echo").unwrap();
    assert_eq!(
        echo.schema(),
        &serde_json::json!({"type": "object", "additionalProperties": true})
    );
    assert!(echo.description().contains("search_tools"));

    let search = tools.iter().find(|t| t.name() == "search_tools").unwrap();
    assert!(matches!(search.effect(), ToolEffect::ReadOnly));
}

#[tokio::test]
async fn search_tools_returns_real_schemas() {
    let handle = support::connect_fixture(McpConnectOptions::new().lazy(true)).await;
    let tools = handle.tools::<()>();
    let search = tools.iter().find(|t| t.name() == "search_tools").unwrap();

    let out = search
        .invoke(&support::tool_ctx(), serde_json::json!({"query": "echo"}))
        .await
        .unwrap();
    let matches = out.content.as_array().expect("array of matches");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["name"], "echo");
    assert_eq!(
        matches[0]["input_schema"]["properties"]["msg"]["type"],
        "string"
    );
}

#[tokio::test]
async fn search_tools_matches_descriptions_case_insensitively() {
    let handle = support::connect_fixture(McpConnectOptions::new().lazy(true)).await;
    let tools = handle.tools::<()>();
    let search = tools.iter().find(|t| t.name() == "search_tools").unwrap();

    let out = search
        .invoke(
            &support::tool_ctx(),
            serde_json::json!({"query": "STRUCTURED"}),
        )
        .await
        .unwrap();
    let matches = out.content.as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["name"], "shape");
}

#[tokio::test]
async fn search_tools_rejects_missing_query() {
    let handle = support::connect_fixture(McpConnectOptions::new().lazy(true)).await;
    let tools = handle.tools::<()>();
    let search = tools.iter().find(|t| t.name() == "search_tools").unwrap();

    let err = search
        .invoke(&support::tool_ctx(), serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        paigasus_helikon_core::ToolError::InvalidArgs { .. }
    ));
}

#[tokio::test]
async fn lazy_with_prefix_prefixes_search_tool_and_results() {
    let handle =
        support::connect_fixture(McpConnectOptions::new().lazy(true).tool_prefix("fs_")).await;
    let tools = handle.tools::<()>();
    let search = tools
        .iter()
        .find(|t| t.name() == "fs_search_tools")
        .unwrap();
    let out = search
        .invoke(&support::tool_ctx(), serde_json::json!({"query": "echo"}))
        .await
        .unwrap();
    assert_eq!(out.content.as_array().unwrap()[0]["name"], "fs_echo");
}
