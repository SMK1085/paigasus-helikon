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
