//! Regression test for the mid-stream error-frame heuristic.
//!
//! A JSON-null `error` field alongside a normal `choices` payload is a shape
//! several OpenAI-compatible backends emit on an otherwise healthy chunk. It
//! must not be treated as fatal — see SMA-451 Task 9 review, finding
//! "Important 2".

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelError, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_litellm::LiteLlmModel;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn run(sse: &'static str) -> Vec<Result<ModelEvent, ModelError>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(sse, "text/event-stream"),
        )
        .mount(&server)
        .await;
    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut r = ModelRequest::new();
    r.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text { text: "hi".into() }],
    }];
    let mut s = model.invoke(r, CancellationToken::new()).await.unwrap();
    let mut out = Vec::new();
    while let Some(ev) = s.next().await {
        out.push(ev);
    }
    out
}

/// A JSON-null `error` alongside normal `choices` must not abort the stream.
///
/// Regression for the pre-fix behavior: `v.get("error").is_some()` matched
/// `Value::Null` too, so a perfectly healthy chunk carrying `"error": null`
/// was misread as a fatal error — the stream died mid-generation with a
/// fabricated, retryable `ModelError::Unavailable` and no diagnostic. This
/// test is confirmed to FAIL against the pre-fix code (see the task-9 fix
/// report for the observed failure).
#[tokio::test]
async fn null_error_field_alongside_choices_is_not_fatal() {
    let evs = run(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}],\"error\":null}\n\n",
    )
    .await;

    assert!(
        evs.iter().all(Result::is_ok),
        "a null `error` field must not produce an Err event, got {evs:?}"
    );
    assert!(
        matches!(evs.first(), Some(Ok(ModelEvent::TokenDelta { text })) if text == "hi"),
        "expected the chunk's content to still be translated into a TokenDelta, got {evs:?}"
    );
}
