#![allow(missing_docs)]
#![cfg(feature = "microvm")]

use std::time::Duration;

use paigasus_helikon_tools::{ExecRequest, ForkdBackend};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn forks_execs_and_destroys() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{"id":"sb-1"}])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes/sb-1/exec"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"stdout":"hello\n","stderr":"","exit_code":0})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/sb-1"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("test-token")
        .snapshot("snap-1")
        .build()
        .expect("builds");
    let out = backend
        .run(ExecRequest::new("echo hello"))
        .await
        .expect("runs");
    assert_eq!(out.stdout, "hello\n");
    assert_eq!(out.exit_code, Some(0));
    assert!(!out.timed_out);
    assert!(!out.truncated);
}

#[tokio::test]
async fn controller_5xx_maps_to_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("test-token")
        .snapshot("snap-1")
        .build()
        .unwrap();
    let err = backend.run(ExecRequest::new("echo hi")).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("HTTP 500"), "unexpected error: {msg}");
    // Token hygiene: auth material never appears in the error text.
    assert!(!msg.contains("test-token"), "token leaked: {msg}");
}

#[tokio::test]
async fn exec_timeout_reports_timed_out_and_still_destroys() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{"id":"sb-2"}])))
        .mount(&server)
        .await;
    // Exec hangs well past the command timeout.
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes/sb-2/exec"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(30))
                .set_body_json(serde_json::json!({"stdout":"","stderr":"","exit_code":0})),
        )
        .mount(&server)
        .await;
    // Scoped destroy mock asserts teardown fired exactly once on the timeout path.
    let destroy = Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/sb-2"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount_as_scoped(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("test-token")
        .snapshot("snap-1")
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();
    let out = backend.run(ExecRequest::new("sleep 30")).await.unwrap();
    assert!(out.timed_out);
    assert_eq!(out.exit_code, None);
    // Dropping the scoped mock verifies the .expect(1) on destroy.
    drop(destroy);
}

#[tokio::test]
async fn output_over_cap_is_truncated() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{"id":"sb-3"}])))
        .mount(&server)
        .await;
    let big = "x".repeat(50);
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes/sb-3/exec"))
        .and(header("authorization", "Bearer t"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"stdout": big, "stderr":"", "exit_code":0})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/sb-3"))
        .and(header("authorization", "Bearer t"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("t")
        .snapshot("s")
        .max_output_bytes(10)
        .build()
        .unwrap();
    let out = backend.run(ExecRequest::new("yes")).await.unwrap();
    assert_eq!(out.stdout.len(), 10);
    assert!(out.truncated);
}

#[tokio::test]
async fn enforce_egress_reports_proxied_when_proxy_reachable() {
    let proxy = MockServer::start().await;
    // The reachability probe hits the proxy endpoint; any TCP-accepting server passes.
    let backend = ForkdBackend::builder("http://127.0.0.1:1") // controller unused here
        .bearer_token("t")
        .snapshot("s")
        .enforce_egress(proxy.uri())
        .build()
        .expect("builds when proxy reachable");
    assert_eq!(
        backend.guarantees().network,
        paigasus_helikon_tools::Isolation::Proxied
    );
}

#[tokio::test]
async fn enforce_egress_fails_closed_when_proxy_unreachable() {
    // Port 1 on loopback refuses; build() must fail rather than report Proxied.
    let err = ForkdBackend::builder("https://127.0.0.1:8080")
        .bearer_token("t")
        .snapshot("s")
        .enforce_egress("http://127.0.0.1:1")
        .build();
    assert!(err.is_err(), "unreachable proxy must fail closed");
}
