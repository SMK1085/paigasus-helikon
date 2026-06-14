#![allow(missing_docs)]
#![cfg(feature = "web")]

use std::sync::Arc;

use paigasus_helikon_core::{CancellationToken, Tool, ToolContext, ToolError, TracerHandle};
use paigasus_helikon_tools::WebFetchTool;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ctx() -> ToolContext<()> {
    ToolContext::new(
        Arc::new(()),
        TracerHandle::default(),
        CancellationToken::new(),
        0,
        8,
    )
}

#[tokio::test]
async fn denies_blocked_domain_without_network() {
    let tool = WebFetchTool::builder()
        .deny_domains(["example.com"])
        .build::<()>();
    let err = tool
        .invoke(
            &ctx(),
            serde_json::json!({ "url": "https://example.com/page" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }), "got {err:?}");
}

#[tokio::test]
async fn denies_non_http_scheme() {
    let tool = WebFetchTool::builder().build::<()>();
    let err = tool
        .invoke(&ctx(), serde_json::json!({ "url": "file:///etc/passwd" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }), "got {err:?}");
}

#[tokio::test]
async fn ssrf_guard_denies_metadata_ip_by_default() {
    let tool = WebFetchTool::builder().build::<()>();
    let err = tool
        .invoke(
            &ctx(),
            serde_json::json!({ "url": "http://169.254.169.254/latest/meta-data/" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }), "got {err:?}");
}

#[tokio::test]
async fn fetches_text_when_private_ips_allowed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_string("hello world"),
        )
        .mount(&server)
        .await;

    let tool = WebFetchTool::builder()
        .allow_private_ips(true)
        .build::<()>();
    let out = tool
        .invoke(&ctx(), serde_json::json!({ "url": server.uri() }))
        .await
        .unwrap();
    assert_eq!(out.content["status"], 200);
    assert_eq!(out.content["format"], "text");
    assert_eq!(out.content["content"], "hello world");
    assert_eq!(out.content["truncated"], false);
}

#[tokio::test]
async fn truncates_body_at_cap() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_string("0123456789ABCDEF"),
        )
        .mount(&server)
        .await;

    let tool = WebFetchTool::builder()
        .allow_private_ips(true)
        .max_body_bytes(10)
        .build::<()>();
    let out = tool
        .invoke(&ctx(), serde_json::json!({ "url": server.uri() }))
        .await
        .unwrap();
    assert_eq!(out.content["truncated"], true);
    assert_eq!(out.content["content"].as_str().unwrap().len(), 10);
}

#[tokio::test]
async fn denies_redirect_chain_over_cap() {
    let server = MockServer::start().await;
    let target = server.uri(); // self-redirect loop
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", target.as_str()))
        .mount(&server)
        .await;

    // allow_private_ips so the loopback target is reachable; the redirect cap
    // (5) must still fire on the self-loop.
    let tool = WebFetchTool::builder()
        .allow_private_ips(true)
        .build::<()>();
    let err = tool
        .invoke(&ctx(), serde_json::json!({ "url": server.uri() }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }), "got {err:?}");
}

#[tokio::test]
async fn max_uses_caps_fetches_per_run() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_string("ok"),
        )
        .mount(&server)
        .await;

    let tool = WebFetchTool::builder()
        .allow_private_ips(true)
        .max_uses(1)
        .build::<()>();
    // One ToolContext shared across both invocations == one agent run.
    let run = ctx();
    let first = tool
        .invoke(&run, serde_json::json!({ "url": server.uri() }))
        .await
        .unwrap();
    assert_eq!(first.content["status"], 200);
    let err = tool
        .invoke(&run, serde_json::json!({ "url": server.uri() }))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Denied { .. }),
        "2nd fetch capped; got {err:?}"
    );
}
