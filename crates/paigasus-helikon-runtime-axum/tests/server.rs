//! Integration tests for [`AgentServer`].

mod support;

use std::sync::Arc;

use paigasus_helikon_runtime_axum::AgentServer;

/// `GET /agents` returns the list of mounted agents as JSON.
#[tokio::test]
async fn lists_mounted_agents() {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(support::ScriptedAgent {
            name: "echo".into(),
            events: support::echo_script(),
        }))
        .build()
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        server.serve_with_listener(listener).await.unwrap();
    });

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/agents"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body[0]["name"], "echo");
}

/// Adding two agents with the same name must cause [`build`] to return `Err`.
#[test]
fn duplicate_agent_name_is_build_error() {
    let b = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(support::ScriptedAgent {
            name: "x".into(),
            events: vec![],
        }))
        .agent(Arc::new(support::ScriptedAgent {
            name: "x".into(),
            events: vec![],
        }));
    assert!(b.build().is_err());
}

/// Omitting a context provider must cause [`build`] to return `Err`, not panic.
#[test]
fn build_without_context_provider_errors() {
    let b = AgentServer::<String>::builder().agent(Arc::new(support::ScriptedAgent {
        name: "x".into(),
        events: vec![],
    }));
    assert!(b.build().is_err());
}
