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
    // Output is now {"tools": [...], "total_matches": N}
    let obj = out.content.as_object().expect("output must be an object");
    let matches = obj["tools"].as_array().expect("tools must be an array");
    assert_eq!(matches.len(), 1);
    assert_eq!(obj["total_matches"], 1);
    assert!(
        obj.get("truncated").is_none_or(|v| v != true),
        "truncated must be absent or false for untruncated results"
    );
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
    let obj = out.content.as_object().expect("output must be an object");
    let matches = obj["tools"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(obj["total_matches"], 1);
    assert!(
        obj.get("truncated").is_none_or(|v| v != true),
        "truncated must be absent or false for untruncated results"
    );
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
    let paigasus_helikon_core::ToolError::InvalidArgs { schema_errors } = err else {
        panic!("expected InvalidArgs, got {err:?}");
    };
    assert_eq!(schema_errors.len(), 1);
    assert_eq!(schema_errors[0], "missing required field `query`");
}

#[tokio::test]
async fn search_tools_rejects_empty_query() {
    let handle = support::connect_fixture(McpConnectOptions::new().lazy(true)).await;
    let tools = handle.tools::<()>();
    let search = tools.iter().find(|t| t.name() == "search_tools").unwrap();

    let err = search
        .invoke(&support::tool_ctx(), serde_json::json!({"query": ""}))
        .await
        .unwrap_err();
    let paigasus_helikon_core::ToolError::InvalidArgs { schema_errors } = err else {
        panic!("expected InvalidArgs, got {err:?}");
    };
    assert_eq!(schema_errors.len(), 1);
    assert_eq!(schema_errors[0], "`query` must be a non-empty string");
}

#[tokio::test]
async fn search_tools_rejects_non_string_query() {
    let handle = support::connect_fixture(McpConnectOptions::new().lazy(true)).await;
    let tools = handle.tools::<()>();
    let search = tools.iter().find(|t| t.name() == "search_tools").unwrap();

    let err = search
        .invoke(&support::tool_ctx(), serde_json::json!({"query": 42}))
        .await
        .unwrap_err();
    let paigasus_helikon_core::ToolError::InvalidArgs { schema_errors } = err else {
        panic!("expected InvalidArgs, got {err:?}");
    };
    assert_eq!(schema_errors.len(), 1);
    assert_eq!(schema_errors[0], "`query` must be a string, got 42");
}

#[tokio::test]
async fn search_results_untruncated_when_below_cap() {
    // Fixture has 4 tools; query "e" matches echo (name) + boom? let's check:
    // echo: name contains "e" ✓
    // boom: name no "e", description "Always fails" no "e" — no match
    // shape: name has "e" (shap_e_) ✓, and "Returns structured content" has "e" ✓
    // sleepy: name contains "e" (sl_e_epy) ✓
    // 3 matches, well below cap of 20 → truncated must be absent.
    let handle = support::connect_fixture(McpConnectOptions::new().lazy(true)).await;
    let tools = handle.tools::<()>();
    let search = tools.iter().find(|t| t.name() == "search_tools").unwrap();

    let out = search
        .invoke(&support::tool_ctx(), serde_json::json!({"query": "e"}))
        .await
        .unwrap();
    let obj = out.content.as_object().expect("output must be an object");
    let tool_list = obj["tools"].as_array().unwrap();
    let total = obj["total_matches"].as_u64().unwrap() as usize;
    assert_eq!(
        tool_list.len(),
        total,
        "len must equal total_matches when untruncated"
    );
    assert!(
        obj.get("truncated").is_none_or(|v| v != true),
        "truncated must be absent or false for untruncated results"
    );
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
    let obj = out.content.as_object().expect("output must be an object");
    let matches = obj["tools"].as_array().unwrap();
    assert_eq!(matches[0]["name"], "fs_echo");
}
