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
const TRAILING_USAGE_FIXTURE: &str = include_str!("fixtures/chat_text_usage_trailing.txt");
const TRAILING_USAGE_EMPTY_CHOICES_FIXTURE: &str =
    include_str!("fixtures/chat_text_usage_trailing_empty_choices.txt");

fn user(text: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
    }
}

async fn run(fixture: &str) -> Vec<ModelEvent> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture.as_bytes(), "text/event-stream"),
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

    let stream = model.invoke(req, CancellationToken::new()).await.unwrap();

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
    assert!(
        tcs.len() >= 4,
        "expected at least 4 ToolCallDelta events, got {}",
        tcs.len()
    );

    let mut seen_c1_name = false;
    let mut seen_c2_name = false;
    let mut c1_args = String::new();
    let mut c2_args = String::new();
    for e in &events {
        if let ModelEvent::ToolCallDelta {
            call_id,
            name,
            args_delta,
        } = e
        {
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
    assert!(
        seen_c1_name,
        "name 'a' should be emitted on c1's first delta"
    );
    assert!(
        seen_c2_name,
        "name 'b' should be emitted on c2's first delta"
    );
    assert_eq!(c1_args, "{\"x\":1}");
    assert_eq!(c2_args, "{\"y\":2}");

    assert!(
        matches!(
            events.last().unwrap(),
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls
            }
        ),
        "expected Finish(ToolCalls) as last event, got {:?}",
        events.last()
    );
}

#[tokio::test]
async fn content_filter_finish_reason_maps_correctly() {
    let events = run(FILTER_FIXTURE).await;
    assert!(
        matches!(
            events.last().unwrap(),
            ModelEvent::Finish {
                reason: FinishReason::ContentFilter
            }
        ),
        "expected Finish(ContentFilter) as last event, got {:?}",
        events.last()
    );
}

/// SMA-522: `usage` arrives on a chunk AFTER the one carrying `finish_reason`.
/// `Finish` must still be the terminal event, with `Usage` before it.
#[tokio::test]
async fn trailing_usage_chunk_still_finishes_last() {
    let events = run(TRAILING_USAGE_FIXTURE).await;

    let finish_count = events
        .iter()
        .filter(|e| matches!(e, ModelEvent::Finish { .. }))
        .count();
    assert_eq!(
        finish_count, 1,
        "expected exactly one Finish, got {events:#?}"
    );

    assert!(
        matches!(
            events.last().unwrap(),
            ModelEvent::Finish {
                reason: FinishReason::Stop
            }
        ),
        "Finish(Stop) must be the last event, got {events:#?}"
    );

    let usage_pos = events
        .iter()
        .position(|e| matches!(e, ModelEvent::Usage { .. }))
        .expect("a Usage event must be present");
    let finish_pos = events
        .iter()
        .position(|e| matches!(e, ModelEvent::Finish { .. }))
        .expect("a Finish event must be present");
    assert!(
        usage_pos < finish_pos,
        "Usage must precede Finish; usage at {usage_pos}, finish at {finish_pos}"
    );

    // Assert the real captured counts. Without this a translator emitting a
    // zeroed Usage would satisfy the ordering assertions above.
    assert!(
        events.iter().any(|e| matches!(
            e,
            ModelEvent::Usage {
                input_tokens: 8,
                output_tokens: 6,
                ..
            }
        )),
        "expected Usage {{ input_tokens: 8, output_tokens: 6 }}, got {events:#?}"
    );
}

/// The same envelope with `"choices":[]` on the usage chunk — the shape
/// api.openai.com emits — must translate identically.
#[tokio::test]
async fn trailing_usage_with_empty_choices_finishes_last() {
    let events = run(TRAILING_USAGE_EMPTY_CHOICES_FIXTURE).await;

    assert!(
        matches!(
            events.last().unwrap(),
            ModelEvent::Finish {
                reason: FinishReason::Stop
            }
        ),
        "Finish(Stop) must be the last event, got {events:#?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ModelEvent::Usage {
                input_tokens: 8,
                output_tokens: 6,
                ..
            }
        )),
        "expected Usage {{ input_tokens: 8, output_tokens: 6 }}, got {events:#?}"
    );
}

/// SMA-522: when the stream errors AFTER the finish chunk, the buffered
/// Finish is discarded. Yielding `Finish` and then `Err` would place an item
/// after the terminal event — the exact thing this fix exists to prevent.
#[tokio::test]
async fn parse_error_after_finish_chunk_yields_err_and_no_finish() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        // Trailing usage chunk missing the required `object` field.
        "data: {\"id\":\"x\",\"created\":1,\"model\":\"gpt-4o\",",
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

    let items: Vec<_> = model
        .invoke(req, CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await;

    assert!(
        items.iter().any(|r| r.is_err()),
        "expected a transport/parse error, got {items:#?}"
    );
    assert!(
        !items
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .any(|e| matches!(e, ModelEvent::Finish { .. })),
        "buffered Finish must be discarded when the stream errors, got {items:#?}"
    );
}
