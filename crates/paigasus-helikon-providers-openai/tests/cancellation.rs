//! Cancellation: the stream must terminate without emitting Finish when
//! the CancellationToken fires mid-flight.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_openai::OpenAiModel;
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

#[tokio::test]
async fn cancellation_before_first_chunk_yields_no_events() {
    let server = MockServer::start().await;

    // Delay the response so cancellation fires first.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .set_body_raw(b"data: [DONE]\n\n" as &[u8], "text/event-stream"),
        )
        .mount(&server)
        .await;

    let model = OpenAiModel::chat("gpt-4o")
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

    // Start the timer before invoke() so a hang inside invoke() is also detected.
    let start = std::time::Instant::now();
    let stream_result = model.invoke(req, cancel).await;

    // Either: invoke() returns an error (transport-style cancellation), OR the stream
    // ends quickly with no Finish. Both are acceptable per the Model trait's
    // cancellation contract. The point is: we don't hang for 5 seconds.
    match stream_result {
        Ok(mut s) => {
            let mut emitted = Vec::new();
            while let Some(item) = s.next().await {
                if let Ok(ev) = item {
                    emitted.push(ev);
                }
            }
            // No Finish should have been emitted before cancellation.
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

/// SMA-522: the finish reason is buffered until end-of-stream, so a stream
/// that completes normally must still emit exactly one Finish — the buffer
/// is neither dropped nor double-flushed.
///
/// The complementary case (cancel firing AFTER the finish chunk but BEFORE
/// EOF) is deliberately not tested here: wiremock's `set_delay` delays the
/// whole response, so that interleaving is unreachable and any test of it
/// would assert whatever the scheduler happened to do. It is guaranteed
/// structurally instead — the `tokio::select!` cancel arms in
/// `backend/chat.rs` `return` without calling `translator.finish()`.
#[tokio::test]
async fn uncancelled_stream_emits_exactly_one_finish() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{}}],\"usage\":{\"prompt_tokens\":1,",
        "\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.as_bytes(), "text/event-stream"))
        .mount(&server)
        .await;

    let model = OpenAiModel::chat("gpt-4o")
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
