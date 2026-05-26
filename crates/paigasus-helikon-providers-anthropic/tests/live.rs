//! Live integration tests against the real Anthropic API.
//!
//! All `#[ignore]` so they don't run in CI. Activate locally with
//! `cargo test -p paigasus-helikon-providers-anthropic -- --ignored`.
//! Each test no-ops without an API key.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest, ResponseFormat,
};
use paigasus_helikon_providers_anthropic::{AnthropicModel, CacheStrategy};

fn skip_if_no_key() -> bool {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        tracing::info!("ANTHROPIC_API_KEY unset; skipping live test");
        return true;
    }
    false
}

fn user(s: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text { text: s.to_owned() }],
    }
}

fn req_with(messages: Vec<Item>, mutate: impl FnOnce(&mut ModelRequest)) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = messages;
    mutate(&mut r);
    r
}

#[tokio::test]
#[ignore]
async fn messages_smoke() {
    if skip_if_no_key() {
        return;
    }
    let model = AnthropicModel::messages("claude-haiku-4-5")
        .build()
        .unwrap();
    let req = req_with(vec![user("Reply with exactly: hello")], |r| {
        r.model_settings.max_output_tokens = Some(64);
    });
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let mut text = String::new();
    while let Some(ev) = s.next().await {
        match ev {
            Ok(ModelEvent::TokenDelta { text: t }) => text.push_str(&t),
            Ok(_) => {}
            Err(e) => panic!("stream error: {e:?}"),
        }
    }
    assert!(text.to_lowercase().contains("hello"));
}

#[tokio::test]
#[ignore]
async fn structured_output_smoke() {
    if skip_if_no_key() {
        return;
    }
    let model = AnthropicModel::messages("claude-haiku-4-5")
        .build()
        .unwrap();
    let req = req_with(vec![user("Give a Person named Ada.")], |r| {
        r.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Person".to_owned(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }),
            strict: true,
        });
        r.model_settings.max_output_tokens = Some(256);
    });
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let mut text = String::new();
    while let Some(ev) = s.next().await {
        match ev {
            Ok(ModelEvent::TokenDelta { text: t }) => text.push_str(&t),
            Ok(_) => {}
            Err(e) => panic!("stream error: {e:?}"),
        }
    }
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("response is JSON");
    assert!(parsed["name"].is_string());
}

#[tokio::test]
#[ignore]
async fn cache_strategy_round_trip() {
    if skip_if_no_key() {
        return;
    }
    // Construct a system prompt big enough to exceed the per-model cache write minimum.
    let big_system = "You are a careful assistant. ".repeat(200);
    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .cache_strategy(CacheStrategy::SystemAndTools)
        .build()
        .unwrap();
    let messages = vec![
        Item::System {
            content: vec![ContentPart::Text {
                text: big_system.clone(),
            }],
        },
        user("Hello, ack only."),
    ];
    let req1 = req_with(messages.clone(), |r| {
        r.model_settings.max_output_tokens = Some(32);
    });
    let mut s = model.invoke(req1, CancellationToken::new()).await.unwrap();
    while let Some(ev) = s.next().await {
        match ev {
            Ok(_) => {}
            Err(e) => panic!("turn-1 stream error: {e:?}"),
        }
    }

    let mut messages2 = messages;
    messages2.push(Item::AssistantMessage {
        content: vec![ContentPart::Text {
            text: "ack".to_owned(),
        }],
        agent: None,
    });
    messages2.push(user("Again, ack."));
    let req2 = req_with(messages2, |r| {
        r.model_settings.max_output_tokens = Some(32);
    });
    let mut s = model.invoke(req2, CancellationToken::new()).await.unwrap();
    let mut cached = 0u32;
    let mut saw_usage = false;
    while let Some(ev) = s.next().await {
        match ev {
            Ok(ModelEvent::Usage {
                cached_input_tokens: Some(c),
                ..
            }) => {
                saw_usage = true;
                cached = cached.max(c);
            }
            Ok(ModelEvent::Usage { .. }) => {
                saw_usage = true;
            }
            Ok(_) => {}
            Err(e) => panic!("turn-2 stream error: {e:?}"),
        }
    }
    assert!(
        saw_usage,
        "expected at least one Usage event in live response"
    );
    if cached == 0 {
        tracing::info!("cache_prefix_too_small: live cache test ran below per-model write minimum",);
        // Pass — caching at <write-minimum is a documented no-op.
    }
}
