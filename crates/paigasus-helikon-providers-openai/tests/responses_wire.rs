//! Wire-format integration tests for the Responses API backend.
//!
//! These tests stand up a wiremock server, point an `OpenAiModel` at
//! `base_url(server.uri())`, and assert on the SSE bytes the provider sees.
//!
//! The Responses API SSE event JSON must include all required fields
//! (no optional-only shapes). See `ResponseStreamEvent` in async-openai 0.40
//! for the exact schema.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_openai::OpenAiModel;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn user(text: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
    }
}

/// Minimal JSON for a `response.output_text.delta` event (all required fields).
fn text_delta_event(seq: u64, delta: &str) -> String {
    format!(
        r#"{{"type":"response.output_text.delta","sequence_number":{seq},"item_id":"msg_01","output_index":0,"content_index":0,"delta":"{delta}"}}"#
    )
}

/// Minimal JSON for a `response.completed` event (all required fields).
///
/// `ResponseCompletedEvent` → `response: Response`.
/// `Response` requires: `created_at`, `id`, `model`, `object`, `output[]`, `status`.
/// `ResponseUsage` requires: `input_tokens`, `input_tokens_details.cached_tokens`,
/// `output_tokens`, `output_tokens_details.reasoning_tokens`, `total_tokens`.
fn completed_event(seq: u64, input_tokens: u32, output_tokens: u32) -> String {
    let total = input_tokens + output_tokens;
    format!(
        r#"{{"type":"response.completed","sequence_number":{seq},"response":{{"id":"resp_01","object":"response","created_at":1,"model":"gpt-5","output":[],"status":"completed","usage":{{"input_tokens":{input_tokens},"input_tokens_details":{{"cached_tokens":0}},"output_tokens":{output_tokens},"output_tokens_details":{{"reasoning_tokens":0}},"total_tokens":{total}}}}}}}"#
    )
}

/// SSE wire representation of a single data event followed by `[DONE]`.
fn sse(events: &[String]) -> String {
    let mut out = String::new();
    for event in events {
        out.push_str("data: ");
        out.push_str(event);
        out.push_str("\n\n");
    }
    out.push_str("data: [DONE]\n\n");
    out
}

#[tokio::test]
async fn happy_path_text_completion() {
    let server = MockServer::start().await;

    let body = sse(&[text_delta_event(1, "hi"), completed_event(2, 5, 1)]);

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.as_bytes(), "text/event-stream"))
        .mount(&server)
        .await;

    let model = OpenAiModel::responses("gpt-5")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();

    let mut req = ModelRequest::new();
    req.messages = vec![user("hi")];

    let stream = model.invoke(req, CancellationToken::new()).await.unwrap();

    let events: Vec<ModelEvent> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|r| r.expect("event was Err"))
        .collect();

    // First event must be the text token delta.
    assert!(
        matches!(events[0], ModelEvent::TokenDelta { ref text } if text == "hi"),
        "expected TokenDelta(\"hi\") at events[0], got {:?}",
        events[0]
    );

    // A Usage event must be present.
    assert!(
        events.iter().any(|e| matches!(
            e,
            ModelEvent::Usage {
                input_tokens: 5,
                output_tokens: 1,
                ..
            }
        )),
        "expected a Usage {{ input_tokens: 5, output_tokens: 1 }} event, events = {events:?}"
    );

    // Finish(Stop) must be the last event (status=completed → Stop).
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
async fn previous_response_id_passes_through_to_request_body() {
    let server = MockServer::start().await;

    let body = sse(&[completed_event(1, 1, 1)]);

    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_string_contains(r#""previous_response_id":"resp_abc""#))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.as_bytes(), "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let model = OpenAiModel::responses("gpt-5")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();

    let mut req = ModelRequest::new();
    req.messages = vec![user("continue")];
    req.model_settings.previous_response_id = Some("resp_abc".to_owned());

    let stream = model.invoke(req, CancellationToken::new()).await.unwrap();

    // Drain the stream to ensure the request is sent and wiremock's `.expect(1)`
    // assertion fires on drop.
    let _: Vec<_> = stream.collect().await;
}
