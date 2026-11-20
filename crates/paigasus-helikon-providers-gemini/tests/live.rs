//! Live smoke tests. Ignored by default; run with `-- --ignored`.
//!
//! Developer API: set `GEMINI_API_KEY` (+ optional `GEMINI_MODEL_ID`; the default is
//! `gemini-3.8-flash`).
//! Vertex (feature `vertex-adc`): set `GOOGLE_CLOUD_PROJECT` + `GOOGLE_CLOUD_LOCATION`
//! with working ADC.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest, ResponseFormat, ToolDef,
};
use paigasus_helikon_providers_gemini::GeminiModel;

fn dev_model() -> Option<GeminiModel> {
    let _ = std::env::var("GEMINI_API_KEY")
        .or_else(|_| std::env::var("GOOGLE_API_KEY"))
        .ok()?;
    let id = std::env::var("GEMINI_MODEL_ID").unwrap_or_else(|_| "gemini-3.8-flash".into());
    GeminiModel::from_env(id).ok()
}

fn user(s: &str) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text { text: s.into() }],
    }];
    r
}

#[tokio::test]
#[ignore]
async fn live_developer_text_turn() {
    let Some(model) = dev_model() else {
        return;
    };
    let mut s = model
        .invoke(user("Say hi in one word."), CancellationToken::new())
        .await
        .unwrap();
    let mut got_text = false;
    while let Some(ev) = s.next().await {
        if let Ok(ModelEvent::TokenDelta { .. }) = ev {
            got_text = true;
        }
    }
    assert!(got_text);
}

#[tokio::test]
#[ignore]
async fn live_developer_structured_output() {
    let Some(model) = dev_model() else {
        return;
    };
    let mut r = user("Return a person named Ada aged 36.");
    r.model_settings.response_format = Some(ResponseFormat::JsonSchema {
        name: "Person".into(),
        schema: serde_json::json!({ "type":"object","properties":{"name":{"type":"string"},"age":{"type":"integer"}} }),
        strict: true,
    });
    let mut s = model.invoke(r, CancellationToken::new()).await.unwrap();
    let mut json = String::new();
    while let Some(ev) = s.next().await {
        if let Ok(ModelEvent::TokenDelta { text }) = ev {
            json.push_str(&text);
        }
    }
    let v: serde_json::Value = serde_json::from_str(json.trim()).expect("valid JSON");
    assert!(v.get("name").is_some());
}

fn text(s: &str) -> Vec<ContentPart> {
    vec![ContentPart::Text { text: s.into() }]
}

fn user_item(s: &str) -> Item {
    Item::UserMessage { content: text(s) }
}

fn call(id: &str, name: &str, args: serde_json::Value) -> Item {
    Item::ToolCall {
        call_id: id.into(),
        name: name.into(),
        args,
    }
}

fn result(id: &str, v: serde_json::Value) -> Item {
    Item::ToolResult {
        call_id: id.into(),
        content: text(&v.to_string()),
    }
}

/// Send `messages` with `search` and `pay` tools; assert the stream has no error and finishes.
async fn assert_finishes(model: &GeminiModel, messages: Vec<Item>) {
    let mut r = ModelRequest::new();
    r.messages = messages;
    r.tools = vec![
        ToolDef {
            name: "search".into(),
            description: "Search for a query.".into(),
            schema: serde_json::json!({
                "type":"object","properties":{"q":{"type":"string"}},"required":["q"]
            }),
        },
        ToolDef {
            name: "pay".into(),
            description: "Pay an amount.".into(),
            schema: serde_json::json!({
                "type":"object","properties":{"amount":{"type":"number"}},"required":["amount"]
            }),
        },
    ];
    let mut s = model.invoke(r, CancellationToken::new()).await.unwrap();
    let mut finished = false;
    while let Some(ev) = s.next().await {
        match ev {
            Ok(ModelEvent::Finish { .. }) => finished = true,
            Ok(_) => {}
            Err(e) => panic!("stream error: {e}"),
        }
    }
    assert!(finished, "no Finish event");
}

#[tokio::test]
#[ignore]
async fn live_developer_tool_result_then_user_text() {
    let Some(model) = dev_model() else {
        return;
    };
    assert_finishes(
        &model,
        vec![
            user_item("search cats"),
            call("fc_0", "search", serde_json::json!({"q":"cats"})),
            result("fc_0", serde_json::json!({"hits":3})),
            user_item("Answer in French."),
        ],
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn live_developer_parallel_round() {
    let Some(model) = dev_model() else {
        return;
    };
    assert_finishes(
        &model,
        vec![
            user_item("search a and b"),
            call("fc_0", "search", serde_json::json!({"q":"a"})),
            call("fc_1", "search", serde_json::json!({"q":"b"})),
            result("fc_0", serde_json::json!({"n":1})),
            result("fc_1", serde_json::json!({"n":2})),
            user_item("Summarize."),
        ],
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn live_developer_cross_turn_repeated_call_id() {
    let Some(model) = dev_model() else {
        return;
    };
    assert_finishes(
        &model,
        vec![
            user_item("search cats"),
            call("fc_0", "search", serde_json::json!({"q":"cats"})),
            result("fc_0", serde_json::json!({"hits":3})),
            Item::AssistantMessage {
                content: text("3 hits."),
                agent: None,
            },
            user_item("now pay 5"),
            call("fc_0", "pay", serde_json::json!({"amount":5})),
            result("fc_0", serde_json::json!({"ok":true})),
            user_item("Confirm."),
        ],
    )
    .await;
}

#[cfg(feature = "vertex-adc")]
#[tokio::test]
#[ignore]
async fn live_vertex_text_turn() {
    if std::env::var("GOOGLE_CLOUD_PROJECT").is_err() {
        return;
    }
    let id = std::env::var("GEMINI_MODEL_ID").unwrap_or_else(|_| "gemini-2.5-flash".into());
    let model = GeminiModel::vertex_from_env(id).await.unwrap();
    let mut s = model
        .invoke(user("Say hi."), CancellationToken::new())
        .await
        .unwrap();
    let mut got_text = false;
    while let Some(ev) = s.next().await {
        // Unwrap each event so a streamed Err fails the test.
        if let ModelEvent::TokenDelta { .. } = ev.unwrap() {
            got_text = true;
        }
    }
    assert!(got_text);
}
