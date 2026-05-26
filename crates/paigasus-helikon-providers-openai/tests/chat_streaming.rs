//! SSE streaming edge cases for the Chat Completions backend.
//!
//! Wiremock serves the entire fixture as one buffer — these tests prove
//! byte-level correctness of the translator's state machine, not pacing.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_openai::OpenAiModel;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PARALLEL_FIXTURE: &str = include_str!("fixtures/chat_parallel_tool_calls.txt");
const FILTER_FIXTURE: &str = include_str!("fixtures/chat_content_filter.txt");

fn user(text: &str) -> Item {
    Item::UserMessage { content: vec![ContentPart::Text { text: text.to_owned() }] }
}

async fn run(fixture: &str) -> Vec<ModelEvent> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(fixture.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let model = OpenAiModel::chat("gpt-4o")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();

    let mut req = ModelRequest::new();
    req.messages = vec![user("hi")];

    let stream = model
        .invoke(req, CancellationToken::new())
        .await
        .unwrap();

    stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect()
}

#[tokio::test]
async fn parallel_tool_calls_interleave_by_index() {
    let events = run(PARALLEL_FIXTURE).await;

    let tcs: Vec<&ModelEvent> = events
        .iter()
        .filter(|e| matches!(e, ModelEvent::ToolCallDelta { .. }))
        .collect();
    assert!(tcs.len() >= 4, "expected at least 4 ToolCallDelta events, got {}", tcs.len());

    let mut seen_c1_name = false;
    let mut seen_c2_name = false;
    let mut c1_args = String::new();
    let mut c2_args = String::new();
    for e in &events {
        if let ModelEvent::ToolCallDelta { call_id, name, args_delta } = e {
            match call_id.as_str() {
                "c1" => {
                    if name.as_deref() == Some("a") {
                        seen_c1_name = true;
                    }
                    c1_args.push_str(args_delta);
                }
                "c2" => {
                    if name.as_deref() == Some("b") {
                        seen_c2_name = true;
                    }
                    c2_args.push_str(args_delta);
                }
                _ => panic!("unexpected call_id {call_id}"),
            }
        }
    }
    assert!(seen_c1_name, "name 'a' should be emitted on c1's first delta");
    assert!(seen_c2_name, "name 'b' should be emitted on c2's first delta");
    assert_eq!(c1_args, "{\"x\":1}");
    assert_eq!(c2_args, "{\"y\":2}");

    assert!(
        matches!(events.last().unwrap(), ModelEvent::Finish { reason: FinishReason::ToolCalls }),
        "expected Finish(ToolCalls) as last event, got {:?}",
        events.last()
    );
}

#[tokio::test]
async fn content_filter_finish_reason_maps_correctly() {
    let events = run(FILTER_FIXTURE).await;
    assert!(
        matches!(events.last().unwrap(), ModelEvent::Finish { reason: FinishReason::ContentFilter }),
        "expected Finish(ContentFilter) as last event, got {:?}",
        events.last()
    );
}
