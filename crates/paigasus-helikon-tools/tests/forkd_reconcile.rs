#![allow(missing_docs)]
#![cfg(feature = "microvm")]

use std::time::{SystemTime, UNIX_EPOCH};

use paigasus_helikon_tools::ForkdBackend;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test]
async fn reconcile_reaps_only_old_tag_matching() {
    let server = MockServer::start().await;
    let now = now_secs();
    let old = now - 600; // older than the 300s default reap_age
                         // LIST: old tag-match (reap), young tag-match (keep), old other-tag (keep),
                         // old tag-match with NO created_at_unix (skip → skipped_unageable).
    Mock::given(method("GET"))
        .and(path("/v1/sandboxes"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":"old-match",   "snapshot_tag":"snap-1", "created_at_unix": old},
            {"id":"young-match", "snapshot_tag":"snap-1", "created_at_unix": now},
            {"id":"old-other",   "snapshot_tag":"other",  "created_at_unix": old},
            {"id":"no-ts",       "snapshot_tag":"snap-1"}
        ])))
        .mount(&server)
        .await;
    // Only old-match may be deleted — scoped + expect(1).
    let del = Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/old-match"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount_as_scoped(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("test-token")
        .snapshot("snap-1")
        .build_backend()
        .expect("builds");
    let report = backend.reconcile().await.expect("reconcile ok");

    assert_eq!(report.scanned, 4);
    assert_eq!(report.reaped, vec!["old-match".to_string()]);
    assert!(report.failed.is_empty(), "no failures: {:?}", report.failed);
    assert_eq!(report.skipped_unageable, 1);
    drop(del); // verifies the DELETE fired exactly once
}

#[tokio::test]
async fn reconcile_list_failure_is_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("test-token")
        .snapshot("snap-1")
        .build_backend()
        .unwrap();
    let err = backend.reconcile().await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("HTTP 500"), "unexpected error: {msg}");
    assert!(!msg.contains("test-token"), "token leaked: {msg}");
}

#[tokio::test]
async fn reconcile_list_decode_error_is_error() {
    // A 200 LIST whose body is not the expected array (e.g. the wire contract
    // drifted to a wrapped `{"sandboxes": …}` object) must surface as a decode
    // Err — not a silent empty sweep — and must not leak the bearer token.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sandboxes"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"sandboxes": []})),
        )
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("test-token")
        .snapshot("snap-1")
        .build_backend()
        .unwrap();
    let err = backend.reconcile().await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("decode"),
        "expected a decode error, got: {msg}"
    );
    assert!(!msg.contains("test-token"), "token leaked: {msg}");
}

#[tokio::test]
async fn reconcile_delete_failure_is_nonfatal() {
    let server = MockServer::start().await;
    let old = now_secs() - 600;
    Mock::given(method("GET"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":"old-match","snapshot_tag":"snap-1","created_at_unix": old}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/old-match"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("t")
        .snapshot("snap-1")
        .build_backend()
        .unwrap();
    let report = backend
        .reconcile()
        .await
        .expect("reconcile ok despite delete 500");
    assert_eq!(report.failed, vec!["old-match".to_string()]);
    assert!(report.reaped.is_empty());
}

#[tokio::test]
async fn reconcile_already_gone_is_idempotent() {
    let server = MockServer::start().await;
    let old = now_secs() - 600;
    Mock::given(method("GET"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":"old-match","snapshot_tag":"snap-1","created_at_unix": old}
        ])))
        .mount(&server)
        .await;
    // 404 = already gone → idempotent success.
    Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/old-match"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("t")
        .snapshot("snap-1")
        .build_backend()
        .unwrap();
    let report = backend.reconcile().await.expect("reconcile ok");
    assert_eq!(report.reaped, vec!["old-match".to_string()]);
    assert!(report.failed.is_empty());
}

#[tokio::test]
async fn reconcile_empty_list_reaps_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("t")
        .snapshot("snap-1")
        .build_backend()
        .unwrap();
    let report = backend.reconcile().await.expect("reconcile ok");
    assert_eq!(report.scanned, 0);
    assert!(report.reaped.is_empty());
    assert!(report.failed.is_empty());
    assert_eq!(report.skipped_unageable, 0);
}
