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
    std::env::var("OPENAI_API_KEY").is_ok()
}

fn user(text: &str) -> Item {
    Item::UserMessage { content: vec![ContentPart::Text { text: text.to_owned() }] }
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
    assert!(events.iter().any(|r| matches!(r, Ok(ModelEvent::Finish { .. }))));
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
    assert!(events.iter().any(|r| matches!(r, Ok(ModelEvent::Finish { .. }))));
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
    let has_tool_call =
        events.iter().any(|r| matches!(r, Ok(ModelEvent::ToolCallDelta { .. })));
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
    let events: Vec<ModelEvent> =
        stream.collect::<Vec<_>>().await.into_iter().filter_map(|r| r.ok()).collect();

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TokenDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let v: serde_json::Value =
        serde_json::from_str(&text).expect("response was not valid JSON");
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
    assert!(deltas > 1, "expected multiple TokenDelta events, got {deltas}");
    assert_eq!(finishes, 1, "expected exactly one Finish event");
}
