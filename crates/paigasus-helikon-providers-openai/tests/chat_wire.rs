//! Wire-format integration tests for the Chat Completions backend.
//!
//! These tests stand up a wiremock server, point an `OpenAiModel` at
//! `base_url(server.uri())`, and assert on the SSE bytes the provider sees.
//!
//! Wiremock serves the entire SSE fixture as one body — these tests prove
//! byte-level correctness of the translator, not resilience to slow chunk
//! delivery.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_openai::OpenAiModel;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn user_msg(text: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
    }
}

#[tokio::test]
async fn happy_path_text_completion() {
    let server = MockServer::start().await;

    // SSE body: a content-delta chunk then a finish chunk with usage.
    // async-openai 0.40 requires `id`, `created`, `model`, and `object` on
    // every chunk. Usage arrives on the same chunk as `finish_reason` (per
    // OpenAI's `stream_options.include_usage: true` behaviour).
    let body = concat!(
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],",
        "\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1,\"total_tokens\":4}}\n\n",
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
    req.messages = vec![user_msg("hi")];

    let stream = model.invoke(req, CancellationToken::new()).await.unwrap();

    let events: Vec<ModelEvent> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|r| r.expect("event was Err"))
        .collect();

    // First event must be the text token.
    assert!(
        matches!(events[0], ModelEvent::TokenDelta { ref text } if text == "hello"),
        "expected TokenDelta(\"hello\") at events[0], got {:?}",
        events[0]
    );

    // A Usage event must be present somewhere.
    assert!(
        events.iter().any(|e| matches!(
            e,
            ModelEvent::Usage {
                input_tokens: 3,
                output_tokens: 1,
                ..
            }
        )),
        "expected a Usage {{ input_tokens: 3, output_tokens: 1 }} event, events = {events:?}"
    );

    // Finish(Stop) must be the last event.
    assert!(
        matches!(
            events.last().unwrap(),
            ModelEvent::Finish {
                reason: FinishReason::Stop
            }
        ),
        "expected Finish(Stop) as last event, got {:?}",
        events.last()
    );
}

#[tokio::test]
async fn rate_limited_response_maps_to_rate_limited_or_other() {
    let server = MockServer::start().await;

    let body =
        r#"{"error":{"message":"rate limit exceeded","type":"rate_limit_error","code":"429"}}"#;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_string(body))
        .mount(&server)
        .await;

    let model = OpenAiModel::chat("gpt-4o")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();

    let mut req = ModelRequest::new();
    req.messages = vec![user_msg("hi")];

    let stream_result = model.invoke(req, CancellationToken::new()).await;

    // The error should surface either as Err on invoke() or as the first
    // stream event.  Both are acceptable per the Model trait contract.
    match stream_result {
        Err(paigasus_helikon_core::ModelError::RateLimited { .. }) => {}
        Err(paigasus_helikon_core::ModelError::Other(_)) => {} // acceptable if mapping degrades
        Ok(mut s) => {
            let first = s
                .next()
                .await
                .expect("stream should yield at least one event");
            assert!(
                matches!(
                    first,
                    Err(paigasus_helikon_core::ModelError::RateLimited { .. })
                        | Err(paigasus_helikon_core::ModelError::Other(_))
                ),
                "expected RateLimited or Other, got {first:?}"
            );
        }
        Err(other) => panic!("expected RateLimited or Other, got {other:?}"),
    }
}
