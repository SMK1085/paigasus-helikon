#![cfg(feature = "microvm")]
#![allow(missing_docs)]

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
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"stdout":"hello\n","stderr":"","exit_code":0})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/sb-1"))
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
