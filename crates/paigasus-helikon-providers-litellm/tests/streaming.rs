//! SSE → `ModelEvent` translation, driven through the real HTTP path.
//!
//! Combines two things: a regression test for the mid-stream error-frame
//! heuristic (`null_error_field_alongside_choices_is_not_fatal`, added in
//! Task 9), and the Task 10 fixture-driven streaming suite below, whose
//! fixtures are transcribed from traffic captured against LiteLLM 1.97.0.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelError, ModelEvent, ModelRequest,
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

// ── Task 10: fixture-driven streaming suite ────────────────────────────────
//
// Fixtures are transcribed from traffic captured against LiteLLM 1.97.0 —
// not hand-invented shapes. See `tests/fixtures/*.txt`.

fn user(s: &str) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text { text: s.into() }],
    }];
    r
}

async fn events_for(fixture: &str) -> Vec<ModelEvent> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(fixture.to_owned(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();

    let mut out = Vec::new();
    while let Some(ev) = s.next().await {
        out.push(ev.expect("no error expected"));
    }
    out
}

/// The regression test for the core ordering contract.
#[tokio::test]
async fn usage_arrives_before_finish_even_though_it_is_a_later_chunk() {
    let evs = events_for(include_str!("fixtures/text_then_trailing_usage.txt")).await;

    let last = evs.last().expect("at least one event");
    assert!(
        matches!(last, ModelEvent::Finish { .. }),
        "Finish must be terminal, got {last:?}"
    );
    let usage_pos = evs
        .iter()
        .position(|e| matches!(e, ModelEvent::Usage { .. }))
        .expect("Usage must be emitted");
    assert_eq!(
        usage_pos,
        evs.len() - 2,
        "Usage must immediately precede Finish"
    );

    let text: String = evs
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TokenDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");
}

#[tokio::test]
async fn truncated_stream_emits_no_finish() {
    let evs = events_for(include_str!("fixtures/truncated_no_finish.txt")).await;
    assert!(
        !evs.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
        "a truncated stream must not be reported as a clean completion"
    );
}

#[tokio::test]
async fn unknown_finish_reason_lands_in_other() {
    let evs = events_for(include_str!("fixtures/unknown_finish_reason.txt")).await;
    match evs.last().unwrap() {
        ModelEvent::Finish { reason } => {
            assert_eq!(*reason, FinishReason::Other("guardrail_intervened".into()));
        }
        other => panic!("expected Finish, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unparseable_frame_is_skipped_without_killing_the_stream() {
    let evs = events_for(include_str!("fixtures/unparseable_frame.txt")).await;
    let text: String = evs
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TokenDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, "beforeafter",
        "text on both sides of the bad frame survives"
    );
    assert!(matches!(evs.last().unwrap(), ModelEvent::Finish { .. }));
}

/// Helper: collect the (call_id, name, args) of every ToolCallDelta.
fn tool_calls(evs: &[ModelEvent]) -> Vec<(String, Option<String>, String)> {
    evs.iter()
        .filter_map(|e| match e {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => Some((call_id.clone(), name.clone(), args_delta.clone())),
            _ => None,
        })
        .collect()
}

/// The normal captured shape: one name-carrying delta, args concatenating to
/// the whole JSON object, Usage before a terminal Finish.
#[tokio::test]
async fn captured_tool_call_stream_assembles_one_named_call() {
    let evs = events_for(include_str!("fixtures/tool_call_stream.txt")).await;
    let calls = tool_calls(&evs);

    let named: Vec<_> = calls.iter().filter(|c| c.1.is_some()).collect();
    assert_eq!(
        named.len(),
        1,
        "exactly one delta carries the name, got {calls:?}"
    );
    assert_eq!(named[0].1.as_deref(), Some("get_weather"));
    assert!(
        calls.iter().all(|c| c.0 == "call_abc"),
        "one call_id throughout"
    );

    let args: String = calls.iter().map(|c| c.2.clone()).collect();
    assert_eq!(args, "{\"city\":\"Berlin\"}");

    // Usage must precede Finish, but need NOT be adjacent to it: the
    // end-of-stream name flush can sit between them (SMA-547 §2).
    let usage_pos = evs
        .iter()
        .position(|e| matches!(e, ModelEvent::Usage { .. }))
        .expect("Usage must be emitted");
    let finish_pos = evs
        .iter()
        .position(|e| matches!(e, ModelEvent::Finish { .. }))
        .expect("Finish must be emitted");
    assert!(
        usage_pos < finish_pos,
        "Usage must precede Finish, got {evs:?}"
    );
    assert_eq!(finish_pos, evs.len() - 1, "Finish is terminal");
}

/// SMA-547 regression, end to end over a real captured LiteLLM stream: the
/// name is split across two post-id deltas and must assemble to `get_weather`,
/// not truncate to `get_`.
#[tokio::test]
async fn captured_fragmented_name_stream_assembles_the_whole_name() {
    let evs = events_for(include_str!(
        "fixtures/tool_call_stream_fragmented_name.txt"
    ))
    .await;
    let calls = tool_calls(&evs);

    let named: Vec<_> = calls.iter().filter_map(|c| c.1.as_deref()).collect();
    assert_eq!(
        named,
        vec!["get_weather"],
        "the name must assemble from both fragments, and be emitted once"
    );

    let args: String = calls.iter().map(|c| c.2.clone()).collect();
    assert_eq!(args, "{\"city\":\"Berlin\"}");
}
