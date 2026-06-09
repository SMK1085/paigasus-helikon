//! Client-side integration tests against the in-process fixture server.

mod support;

use paigasus_helikon_core::ToolEffect;
use paigasus_helikon_mcp::McpConnectOptions;

#[tokio::test]
async fn eager_tools_expose_names_schemas_and_effects() {
    let handle = support::connect_fixture(McpConnectOptions::new()).await;
    let tools = handle.tools::<()>();
    assert_eq!(tools.len(), 4);

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
async fn lazy_description_hint_uses_prefixed_search_tool_name() {
    let handle =
        support::connect_fixture(McpConnectOptions::new().lazy(true).tool_prefix("fs_")).await;
    let tools = handle.tools::<()>();
    let echo = tools.iter().find(|t| t.name() == "fs_echo").unwrap();
    assert!(echo.description().contains("fs_search_tools"));
}

#[tokio::test]
async fn invoke_aborts_when_run_cancel_fires() {
    use std::time::Duration;
    let handle = support::connect_fixture(McpConnectOptions::new()).await;
    let tools = handle.tools::<()>();
    let sleepy = tools.iter().find(|t| t.name() == "sleepy").unwrap();

    let cancel = paigasus_helikon_core::CancellationToken::new();
    let ctx = std::sync::Arc::new(());
    let run_ctx = paigasus_helikon_core::RunContext::new(
        ctx,
        std::sync::Arc::new(paigasus_helikon_core::MemorySession::new())
            as std::sync::Arc<dyn paigasus_helikon_core::Session>,
        paigasus_helikon_core::HookRegistry::new(),
        paigasus_helikon_core::TracerHandle::builder().build(),
        cancel.clone(),
    );
    let tool_ctx = run_ctx.to_tool_context();

    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel2.cancel();
    });

    let started = std::time::Instant::now();
    let err = sleepy
        .invoke(&tool_ctx, serde_json::json!({"msg": "zzz"}))
        .await
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "did not cancel promptly"
    );
    assert!(err.to_string().contains("cancelled"));
}

#[tokio::test]
async fn tool_prefix_is_applied() {
    let handle = support::connect_fixture(McpConnectOptions::new().tool_prefix("fs_")).await;
    let tools = handle.tools::<()>();
    assert!(tools.iter().any(|t| t.name() == "fs_echo"));
    assert!(tools.iter().all(|t| t.name().starts_with("fs_")));
}
