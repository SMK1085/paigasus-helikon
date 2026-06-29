//! Integration tests for the `GET /openapi.json` endpoint.
//!
//! Gated on the `openapi` crate feature so this binary is only compiled and
//! linked when that feature is active.

#![cfg(feature = "openapi")]

mod support;

use support::spawn_echo_server;

/// `GET /openapi.json` returns a valid OpenAPI document that includes
/// the mounted agent name from the runtime configuration.
#[tokio::test]
async fn openapi_json_returns_valid_spec() {
    let addr = spawn_echo_server().await;

    let resp = reqwest::Client::new()
        .get(format!("http://{addr}/openapi.json"))
        .send()
        .await
        .expect("GET /openapi.json");

    assert_eq!(resp.status(), 200, "expected 200 OK");

    let body: serde_json::Value = resp.json().await.expect("response body must be valid JSON");

    // Must contain the top-level "openapi" version string (e.g. "3.1.0").
    assert!(
        body.get("openapi").is_some(),
        "spec must have an 'openapi' field; got: {body}"
    );

    // Must expose either the POST runs path or the AgentInfo schema.
    let body_str = body.to_string();
    let has_runs_path = body_str.contains("/agents/{name}/runs");
    let has_agent_info_schema = body_str.contains("AgentInfo");
    assert!(
        has_runs_path || has_agent_info_schema,
        "spec must contain the '/agents/{{name}}/runs' path or the 'AgentInfo' schema; \
         got: {body_str}"
    );

    // The spec must mention the runtime-mounted agent name ("echo").
    assert!(
        body_str.contains("echo"),
        "spec must contain the mounted agent name 'echo'; got: {body_str}"
    );
}
