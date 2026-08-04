//! Integration tests for the `GET /openapi.json` endpoint.
//!
//! Gated on the `openapi` crate feature so this binary is only compiled and
//! linked when that feature is active.

#![cfg(feature = "openapi")]

mod support;

/// `GET /openapi.json` returns a valid OpenAPI document whose paths cover the
/// mounted routes and whose `info.description` names the mounted agent.
#[tokio::test]
async fn openapi_json_returns_valid_spec() {
    let base = support::spawn_echo_server();

    let resp = reqwest::get(format!("{base}/openapi.json"))
        .await
        .expect("GET /openapi.json");

    assert_eq!(resp.status(), 200, "expected 200 OK");

    let body: serde_json::Value = resp.json().await.expect("response body must be valid JSON");

    assert!(
        body["paths"]["/agents"].is_object(),
        "spec must document '/agents'; got: {body}"
    );
    assert!(
        body["paths"]["/agents/{name}/runs"].is_object(),
        "spec must document '/agents/{{name}}/runs'; got: {body}"
    );
    assert!(
        body["paths"]["/agents/{name}/runs/{id}/events"].is_object(),
        "spec must document '/agents/{{name}}/runs/{{id}}/events'; got: {body}"
    );

    let description = body["info"]["description"]
        .as_str()
        .expect("info.description must be a string");
    assert!(
        description.contains("echo"),
        "spec description must mention the mounted agent 'echo'; got: {description}"
    );
}
