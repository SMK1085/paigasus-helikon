//! Client-side integration tests against the in-process fixture server.

mod support;

use paigasus_helikon_core::ToolEffect;
use paigasus_helikon_mcp::McpConnectOptions;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn tool_ctx_with_cancel(
    cancel: paigasus_helikon_core::CancellationToken,
) -> paigasus_helikon_core::ToolContext<()> {
    paigasus_helikon_core::RunContext::new(
        std::sync::Arc::new(()),
        std::sync::Arc::new(paigasus_helikon_core::MemorySession::new())
            as std::sync::Arc<dyn paigasus_helikon_core::Session>,
        paigasus_helikon_core::HookRegistry::new(),
        paigasus_helikon_core::TracerHandle::builder().build(),
        cancel,
    )
    .to_tool_context()
}

fn tool_ctx() -> paigasus_helikon_core::ToolContext<()> {
    tool_ctx_with_cancel(paigasus_helikon_core::CancellationToken::new())
}

// ---------------------------------------------------------------------------
// Existing tests
// ---------------------------------------------------------------------------

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
    let tool_ctx = tool_ctx_with_cancel(cancel.clone());

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

// ---------------------------------------------------------------------------
// New: invoke round-trips, error mapping, close semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invoke_round_trips_text() {
    let handle = support::connect_fixture(McpConnectOptions::new()).await;
    let tools = handle.tools::<()>();
    let echo = tools.iter().find(|t| t.name() == "echo").unwrap();
    let out = echo
        .invoke(&tool_ctx(), serde_json::json!({"msg": "hi"}))
        .await
        .unwrap();
    assert_eq!(out.content, serde_json::json!("hi"));
}

#[tokio::test]
async fn invoke_surfaces_structured_content() {
    let handle = support::connect_fixture(McpConnectOptions::new()).await;
    let tools = handle.tools::<()>();
    let shape = tools.iter().find(|t| t.name() == "shape").unwrap();
    let out = shape
        .invoke(&tool_ctx(), serde_json::json!({"msg": "x"}))
        .await
        .unwrap();
    assert_eq!(out.content, serde_json::json!({"ok": true}));
}

#[tokio::test]
async fn is_error_result_becomes_tool_error() {
    let handle = support::connect_fixture(McpConnectOptions::new()).await;
    let tools = handle.tools::<()>();
    let boom = tools.iter().find(|t| t.name() == "boom").unwrap();
    let err = boom
        .invoke(&tool_ctx(), serde_json::json!({"msg": "x"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("kaboom"));
}

#[tokio::test]
async fn non_object_args_are_invalid() {
    let handle = support::connect_fixture(McpConnectOptions::new()).await;
    let tools = handle.tools::<()>();
    let echo = tools.iter().find(|t| t.name() == "echo").unwrap();
    let err = echo
        .invoke(&tool_ctx(), serde_json::json!([1, 2]))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        paigasus_helikon_core::ToolError::InvalidArgs { .. }
    ));
}

#[tokio::test]
async fn calls_after_close_fail() {
    use std::time::Duration;
    let handle = support::connect_fixture(McpConnectOptions::new()).await;
    let tools = handle.tools::<()>();
    let echo = tools.iter().find(|t| t.name() == "echo").unwrap();
    handle.close();
    // The cancellation is fire-and-forget; poll until the transport tears
    // down (up to 2 s) so we don't race the close against the call.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        tokio::task::yield_now().await;
        let result = echo
            .invoke(&tool_ctx(), serde_json::json!({"msg": "x"}))
            .await;
        if result.is_err() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "invoke still succeeded 2 s after close"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
