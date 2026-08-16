//! Cancellation is honoured before the request future resolves.
//!
//! A true mid-stream cancel is deliberately not tested: wiremock serves the
//! whole body in one `set_body_raw`, so there is no pacing to interrupt — the
//! same limitation the OpenAI provider's streaming tests document.

use futures_util::StreamExt;
use paigasus_helikon_core::{CancellationToken, ContentPart, Item, Model, ModelRequest};
use paigasus_helikon_providers_litellm::LiteLlmModel;
use std::time::Duration;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn cancel_before_response_yields_an_empty_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_delay(Duration::from_secs(30))
                .set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();

    let mut req = ModelRequest::new();
    req.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text { text: "hi".into() }],
    }];

    let cancel = CancellationToken::new();
    let mut s = model.invoke(req, cancel.clone()).await.unwrap();
    cancel.cancel();

    // Per core's contract, a cancelled stream ends WITHOUT emitting Finish.
    let next = tokio::time::timeout(Duration::from_secs(5), s.next())
        .await
        .expect("cancellation must not hang");
    assert!(next.is_none(), "cancelled stream must end immediately");
}
