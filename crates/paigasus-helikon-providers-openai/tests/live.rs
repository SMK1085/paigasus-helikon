//! Live integration tests hit the real OpenAI API.
//!
//! Skipped silently if `OPENAI_API_KEY` is unset. Annotated `#[ignore]`
//! so `cargo test` doesn't run them by default; opt-in via
//! `cargo test -p paigasus-helikon-providers-openai -- --ignored`.
//!
//! Cost: ~$0.001 per `cargo test --ignored` run.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest, ModelSettings,
    ResponseFormat, ToolDef,
};
use paigasus_helikon_providers_openai::OpenAiModel;

fn key_set() -> bool {
    std::env::var("OPENAI_API_KEY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn user(text: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
    }
}

#[tokio::test]
#[ignore]
async fn chat_smoke() {
    if !key_set() {
        return;
    }
    let model = OpenAiModel::chat("gpt-4o-mini").build().unwrap();
    let mut req = ModelRequest::new();
    req.messages = vec![user("Reply with the single word HELLO.")];
    let stream = model.invoke(req, CancellationToken::new()).await.unwrap();
    let events: Vec<_> = stream.collect().await;
    assert!(!events.is_empty(), "live API returned empty stream");
    assert!(events
        .iter()
        .any(|r| matches!(r, Ok(ModelEvent::Finish { .. }))));
}

#[tokio::test]
#[ignore]
async fn responses_smoke() {
    if !key_set() {
        return;
    }
    let model = OpenAiModel::responses("gpt-4o-mini").build().unwrap();
    let mut req = ModelRequest::new();
    req.messages = vec![user("Reply with the single word HELLO.")];
    let stream = model.invoke(req, CancellationToken::new()).await.unwrap();
    let events: Vec<_> = stream.collect().await;
    assert!(events
        .iter()
        .any(|r| matches!(r, Ok(ModelEvent::Finish { .. }))));
}

/// An end-to-end pin against the live API for a zero-argument tool call.
///
/// `responses_tool_call_zero_args.txt` pins the 2026-08-22 capture as a frozen
/// fixture; this test re-drives the same shape against the real endpoint.
///
/// **What this test can no longer notice (final-review correction):** it was
/// originally framed as "if OpenAI ever elides the `{}` delta, this is where
/// it surfaces." That framing predates SMA-562's `response.completed`
/// reconciliation sweep. The sweep synthesises a `ToolCallDelta` from
/// `response.output` for any call that streamed no argument deltas at all, so
/// a live response *with* the `{}` delta and one *without* it now produce
/// byte-identical `ModelEvent` sequences through the public API — the two
/// shapes are indistinguishable downstream of the translator, which is the
/// fix working as intended, not a gap. This test therefore cannot detect
/// OpenAI eliding the delta, and is also blind to a regression that deleted
/// the argument-delta emission path entirely (the reconciliation sweep would
/// silently cover for it). Its real value is pinning the end-to-end shape —
/// name, args and finish reason — against the live API, not distinguishing
/// which wire path produced it.
#[tokio::test]
#[ignore]
async fn responses_zero_arg_tool_streams_a_delta() {
    if !key_set() {
        return;
    }
    let model = OpenAiModel::responses("gpt-4o-mini").build().unwrap();
    let mut req = ModelRequest::new();
    req.messages = vec![user("What time is it right now? Use the tool.")];
    req.tools = vec![ToolDef {
        name: "get_current_time".to_owned(),
        description: "Return the current server time. Takes no arguments.".to_owned(),
        schema: serde_json::json!({"type": "object", "properties": {}}),
    }];
    let stream = model.invoke(req, CancellationToken::new()).await.unwrap();
    let events: Vec<ModelEvent> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|r| r.expect("live stream emitted Err"))
        .collect();

    let tool_call_deltas: Vec<&ModelEvent> = events
        .iter()
        .filter(|e| matches!(e, ModelEvent::ToolCallDelta { .. }))
        .collect();
    assert_eq!(
        tool_call_deltas.len(),
        1,
        "expected exactly one ToolCallDelta for a single zero-argument tool call; got {events:#?}"
    );
    match tool_call_deltas[0] {
        ModelEvent::ToolCallDelta {
            name, args_delta, ..
        } => {
            assert_eq!(name.as_deref(), Some("get_current_time"));
            assert_eq!(args_delta, "{}");
        }
        other => panic!("expected ToolCallDelta, got {other:?}"),
    }
    assert!(
        matches!(
            events.last(),
            Some(ModelEvent::Finish {
                reason: paigasus_helikon_core::FinishReason::ToolCalls
            })
        ),
        "expected a terminal Finish {{ ToolCalls }}; got {events:#?}"
    );
}

#[tokio::test]
#[ignore]
async fn chat_tool_call_round_trip() {
    if !key_set() {
        return;
    }
    let model = OpenAiModel::chat("gpt-4o-mini").build().unwrap();
    let mut req = ModelRequest::new();
    req.messages = vec![user("Call the `ping` tool with no arguments.")];
    req.tools = vec![ToolDef {
        name: "ping".to_owned(),
        description: "Returns pong.".to_owned(),
        schema: serde_json::json!({"type": "object", "properties": {}}),
    }];
    let stream = model.invoke(req, CancellationToken::new()).await.unwrap();
    let events: Vec<_> = stream.collect().await;
    let has_tool_call = events
        .iter()
        .any(|r| matches!(r, Ok(ModelEvent::ToolCallDelta { .. })));
    assert!(has_tool_call, "expected a tool-call delta, got {events:#?}");
}

#[tokio::test]
#[ignore]
async fn chat_structured_output_round_trip() {
    if !key_set() {
        return;
    }
    let model = OpenAiModel::chat("gpt-4o-mini").build().unwrap();
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
    });
    let mut req = ModelRequest::new();
    req.messages = vec![user("What's the capital of France? Answer as JSON.")];
    let mut settings = ModelSettings::new();
    settings.response_format = Some(ResponseFormat::JsonSchema {
        name: "Answer".to_owned(),
        schema,
        strict: true,
    });
    req.model_settings = settings;
    let stream = model.invoke(req, CancellationToken::new()).await.unwrap();
    let events: Vec<ModelEvent> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|r| r.expect("live stream emitted Err"))
        .collect();

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TokenDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let v: serde_json::Value = serde_json::from_str(&text).expect("response was not valid JSON");
    assert!(v.get("answer").is_some(), "missing `answer` key in: {v}");
}

#[tokio::test]
#[ignore]
async fn streaming_round_trip() {
    if !key_set() {
        return;
    }
    let model = OpenAiModel::chat("gpt-4o-mini").build().unwrap();
    let mut req = ModelRequest::new();
    req.messages = vec![user("Count to 5.")];
    let stream = model.invoke(req, CancellationToken::new()).await.unwrap();
    let mut deltas = 0;
    let mut finishes = 0;
    let mut s = stream;
    while let Some(item) = s.next().await {
        match item {
            Ok(ModelEvent::TokenDelta { .. }) => deltas += 1,
            Ok(ModelEvent::Finish { .. }) => finishes += 1,
            _ => {}
        }
    }
    assert!(
        deltas > 1,
        "expected multiple TokenDelta events, got {deltas}"
    );
    assert_eq!(finishes, 1, "expected exactly one Finish event");
}
