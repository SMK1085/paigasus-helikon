//! Cancellation: the stream must terminate without emitting Finish when the
//! CancellationToken fires mid-flight.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_anthropic::AnthropicModel;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn user(text: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
    }
}

/// SMA-531: cancellation must end the stream without a terminal `Finish`, as
/// `paigasus_helikon_core::Model::invoke` mandates.
///
/// The complementary case (cancel firing AFTER `message_delta` buffered a stop
/// reason but BEFORE EOF) is deliberately not tested here: wiremock's
/// `set_delay` delays the whole response, so that interleaving is unreachable
/// and any test of it would assert whatever the scheduler happened to do. It is
/// guaranteed structurally instead — the `tokio::select!` cancel arm in
/// `model.rs` `return`s without calling `translator.finish()`.
#[tokio::test]
async fn cancellation_before_first_chunk_emits_no_finish() {
    let server = MockServer::start().await;

    // Delay the response so cancellation fires first.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(include_str!("fixtures/text_only.txt"), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });

    let mut req = ModelRequest::new();
    req.messages = vec![user("hi")];

    // Start the timer before invoke() so a hang inside invoke() is also caught.
    let start = std::time::Instant::now();
    let stream_result = model.invoke(req, cancel).await;

    // Either invoke() returns an error, or the stream ends quickly with no
    // Finish. Both satisfy the Model trait's cancellation contract.
    match stream_result {
        Ok(mut s) => {
            let mut emitted = Vec::new();
            while let Some(item) = s.next().await {
                if let Ok(ev) = item {
                    emitted.push(ev);
                }
            }
            assert!(
                !emitted
                    .iter()
                    .any(|e| matches!(e, ModelEvent::Finish { .. })),
                "stream emitted Finish after cancellation: {emitted:#?}"
            );
        }
        Err(_) => { /* acceptable */ }
    }

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(4),
        "cancellation took too long: {elapsed:?}"
    );
}

/// The control for the test above. Without it, that assertion would pass
/// against a build that emits no events at all — the exact vacuity this
/// ticket's acceptance criteria call out.
#[tokio::test]
async fn uncancelled_stream_emits_exactly_one_finish() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(include_str!("fixtures/text_only.txt"), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();

    let mut req = ModelRequest::new();
    req.messages = vec![user("hi")];

    let mut s = model
        .invoke(req, CancellationToken::new())
        .await
        .expect("invoke should succeed");

    let mut emitted = Vec::new();
    while let Some(item) = s.next().await {
        emitted.push(item.expect("no error expected"));
    }

    let finishes = emitted
        .iter()
        .filter(|e| matches!(e, ModelEvent::Finish { .. }))
        .count();
    assert_eq!(finishes, 1, "expected exactly one Finish, got {emitted:#?}");
    assert!(
        matches!(emitted.last().unwrap(), ModelEvent::Finish { .. }),
        "Finish must be last, got {emitted:#?}"
    );
}
