//! Integration tests for [`AgentServer`].

mod support;

use std::sync::Arc;

use paigasus_helikon_runtime_axum::{AgentServer, ServerError};

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

/// Adding two agents with the same name must cause [`build`] to return a
/// [`ServerError::BadRequest`] naming the duplicate.
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
    let err = b.build().err().expect("duplicate name must fail the build");
    match err {
        ServerError::BadRequest(msg) => {
            assert!(
                msg.contains("duplicate agent name"),
                "expected a duplicate-name message, got: {msg}"
            );
        }
        other => panic!("expected ServerError::BadRequest, got: {other}"),
    }
}

/// Omitting a context provider must cause [`build`] to return a
/// [`ServerError::Internal`] explaining the missing provider, not panic.
#[test]
fn build_without_context_provider_errors() {
    let b = AgentServer::<String>::builder().agent(Arc::new(support::ScriptedAgent {
        name: "x".into(),
        events: vec![],
    }));
    let err = b
        .build()
        .err()
        .expect("missing context must fail the build");
    match err {
        ServerError::Internal(msg) => {
            assert!(
                msg.contains("context provider"),
                "expected a missing-context message, got: {msg}"
            );
        }
        other => panic!("expected ServerError::Internal, got: {other}"),
    }
}

/// Building the default in-memory session store with a zero session cap must
/// return [`ServerError::BadRequest`] rather than panicking inside
/// `InMemorySessionProvider::new(0)`.
#[test]
fn max_sessions_zero_is_build_error() {
    let b = AgentServer::<()>::builder()
        .with_default_context()
        .max_sessions(0)
        .agent(Arc::new(support::ScriptedAgent {
            name: "x".into(),
            events: vec![],
        }));
    let err = b
        .build()
        .err()
        .expect("max_sessions(0) must fail the build");
    match err {
        ServerError::BadRequest(msg) => {
            assert!(
                msg.contains("max_sessions"),
                "expected a max_sessions message, got: {msg}"
            );
        }
        other => panic!("expected ServerError::BadRequest, got: {other}"),
    }
}
