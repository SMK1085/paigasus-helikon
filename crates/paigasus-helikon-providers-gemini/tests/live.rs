//! Live smoke tests. Ignored by default; run with `-- --ignored`.
//!
//! Developer API: set `GEMINI_API_KEY` (+ optional `GEMINI_MODEL_ID`).
//! Vertex (feature `vertex-adc`): set `GOOGLE_CLOUD_PROJECT` + `GOOGLE_CLOUD_LOCATION`
//! with working ADC.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest, ResponseFormat,
};
use paigasus_helikon_providers_gemini::GeminiModel;

fn dev_model() -> Option<GeminiModel> {
    let _ = std::env::var("GEMINI_API_KEY")
        .or_else(|_| std::env::var("GOOGLE_API_KEY"))
        .ok()?;
    let id = std::env::var("GEMINI_MODEL_ID").unwrap_or_else(|_| "gemini-2.5-flash".into());
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
    while s.next().await.is_some() {}
}
