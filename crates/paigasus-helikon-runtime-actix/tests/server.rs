//! End-to-end integration tests for the actix-web runtime's HTTP surface.
//!
//! Builder-error unit tests live alongside `AgentServer` in
//! `src/server.rs::tests` (Task 5); this file covers actual HTTP behaviour
//! against a server booted via [`support::spawn_actix_server`].

mod support;

/// `GET /agents` lists every agent mounted on the server.
#[tokio::test]
async fn lists_mounted_agents() {
    let base = support::spawn_echo_server();

    let v: serde_json::Value = reqwest::get(format!("{base}/agents"))
        .await
        .expect("request")
        .json()
        .await
        .expect("json body");

    let agents = v.as_array().expect("array response");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["name"], "echo");
    assert_eq!(agents[0]["description"], "scripted test agent");
}
