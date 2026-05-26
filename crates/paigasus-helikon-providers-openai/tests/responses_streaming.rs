//! SSE streaming edge cases for the Responses API backend.
//!
//! Wiremock serves the entire fixture as one buffer — these tests prove
//! byte-level correctness of the translator's state machine for the full
//! F2 event surface.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_openai::OpenAiModel;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REASONING: &str = include_str!("fixtures/responses_reasoning_then_text.txt");
const LENGTH: &str = include_str!("fixtures/responses_incomplete_length.txt");
const FILTER: &str = include_str!("fixtures/responses_incomplete_filter.txt");
const FAILED: &str = include_str!("fixtures/responses_failed.txt");

fn user(text: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
    }
}

async fn run(fixture: &str) -> Vec<Result<ModelEvent, paigasus_helikon_core::ModelError>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture.as_bytes(), "text/event-stream"),
        )
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
    stream.collect::<Vec<_>>().await
}

#[tokio::test]
async fn reasoning_summary_emits_reasoning_delta_then_text() {
    let events = run(REASONING).await;
    let unwrapped: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    assert!(
        matches!(&unwrapped[0], ModelEvent::ReasoningDelta { text } if text == "thinking..."),
        "expected ReasoningDelta(\"thinking...\") at [0], got {:?}",
        unwrapped[0]
    );
    assert!(
        matches!(&unwrapped[1], ModelEvent::TokenDelta { text } if text == "answer"),
        "expected TokenDelta(\"answer\") at [1], got {:?}",
        unwrapped[1]
    );

    let usage = unwrapped
        .iter()
        .find(|e| matches!(e, ModelEvent::Usage { .. }))
        .expect("expected a Usage event");
    if let ModelEvent::Usage {
        reasoning_tokens, ..
    } = usage
    {
        assert_eq!(
            *reasoning_tokens,
            Some(2),
            "expected reasoning_tokens=Some(2), got {:?}",
            reasoning_tokens
        );
    } else {
        panic!("expected Usage event, got {:?}", usage);
    }

    assert!(
        matches!(
            unwrapped.last().unwrap(),
            ModelEvent::Finish {
                reason: FinishReason::Stop
            }
        ),
        "expected Finish(Stop) as last event, got {:?}",
        unwrapped.last()
    );
}

#[tokio::test]
async fn incomplete_max_output_tokens_maps_to_finish_length() {
    let events = run(LENGTH).await;
    let unwrapped: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    assert!(
        matches!(
            unwrapped.last().unwrap(),
            ModelEvent::Finish {
                reason: FinishReason::Length
            }
        ),
        "expected Finish(Length) as last event, got {:?}",
        unwrapped.last()
    );
}

#[tokio::test]
async fn incomplete_content_filter_maps_to_finish_content_filter() {
    let events = run(FILTER).await;
    let unwrapped: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    assert!(
        unwrapped
            .iter()
            .any(|e| matches!(e, ModelEvent::TokenDelta { text } if text.starts_with("sorry"))),
        "expected a TokenDelta starting with \"sorry\", events = {unwrapped:?}"
    );
    assert!(
        matches!(
            unwrapped.last().unwrap(),
            ModelEvent::Finish {
                reason: FinishReason::ContentFilter
            }
        ),
        "expected Finish(ContentFilter) as last event, got {:?}",
        unwrapped.last()
    );
}

#[tokio::test]
async fn failed_event_terminates_stream_with_error() {
    let events = run(FAILED).await;

    let has_failure_signal = events.iter().any(|r| {
        matches!(
            r,
            Ok(ModelEvent::Finish { reason: FinishReason::Other(s) }) if s.contains("unavailable")
        ) || r.is_err()
    });
    assert!(
        has_failure_signal,
        "expected a failure signal in: {events:#?}"
    );
}
