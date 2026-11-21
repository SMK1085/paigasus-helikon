//! Live round trip of a thought signature. Ignored by default.
//!
//! Run: `GEMINI_API_KEY=... cargo test -p paigasus-helikon-providers-gemini -- --ignored live`
//! Optional: `GEMINI_MODEL_ID` (default `gemini-3.8-flash`).

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelEvent, ModelRequest,
    ModelTurnAccumulator, ToolChoice, ToolDef,
};

use crate::translate::build_request;
use crate::{GeminiModel, PROVIDER_DATA_KEY};

fn search_tool() -> ToolDef {
    ToolDef::new(
        "search",
        "Search for a query.",
        serde_json::json!({
            "type":"object","properties":{"q":{"type":"string"}},"required":["q"]
        }),
    )
}

#[tokio::test]
#[ignore]
async fn live_signature_round_trip() {
    if std::env::var("GEMINI_API_KEY").is_err() && std::env::var("GOOGLE_API_KEY").is_err() {
        eprintln!("skipped: set GEMINI_API_KEY or GOOGLE_API_KEY");
        return;
    }
    let id = std::env::var("GEMINI_MODEL_ID").unwrap_or_else(|_| "gemini-3.8-flash".into());
    let model = GeminiModel::from_env(id).unwrap();

    let mut request = ModelRequest::new();
    request.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text {
            text: "Use the search tool to look up cats. Do not answer before you search.".into(),
        }],
    }];
    request.tools = vec![search_tool()];
    // Turn 1 must call the tool. Turn 2 must not be forced.
    request.model_settings.tool_choice = Some(ToolChoice::Tool {
        name: "search".into(),
    });

    // Turn 1: collect the turn and look for a signed call.
    let mut stream = model
        .invoke(request.clone(), CancellationToken::new())
        .await
        .unwrap();
    let mut acc = ModelTurnAccumulator::new("root");
    while let Some(ev) = stream.next().await {
        acc.observe(&ev.expect("turn 1 stream error"));
    }
    let items = acc.finish().unwrap().items;

    let (signed_call, signature) = items
        .iter()
        .find_map(|item| {
            let Item::AssistantMessage { content, .. } = item else {
                return None;
            };
            content.iter().find_map(|part| match part {
                ContentPart::ProviderData {
                    provider,
                    call_id: Some(call_id),
                    data,
                } if provider == PROVIDER_DATA_KEY => data
                    .get("thoughtSignature")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| (call_id.clone(), s.to_owned())),
                _ => None,
            })
        })
        .expect("turn 1 has a gemini ProviderData with a thoughtSignature and a call id");

    // Turn 2 request: the turn, then one result per call, and no user text.
    let mut messages = request.messages.clone();
    messages.extend(items.iter().cloned());
    for item in &items {
        if let Item::ToolCall { call_id, .. } = item {
            messages.push(Item::ToolResult {
                call_id: call_id.clone(),
                content: vec![ContentPart::Text {
                    text: r#"{"hits":3}"#.into(),
                }],
            });
        }
    }
    request.messages = messages;
    request.model_settings.tool_choice = None;

    let body = build_request(&model.0, &request).unwrap().body;
    // The call with the captured id must carry exactly that signature.
    let part = body["contents"]
        .as_array()
        .expect("contents array")
        .iter()
        .flat_map(|c| c["parts"].as_array().into_iter().flatten())
        .find(|p| p["functionCall"]["id"].as_str() == Some(signed_call.as_str()))
        .unwrap_or_else(|| panic!("no functionCall part has id {signed_call}: {body}"));
    assert_eq!(
        part["thoughtSignature"].as_str(),
        Some(signature.as_str()),
        "the functionCall part {signed_call} does not carry its signature"
    );
    assert!(
        !body
            .to_string()
            .contains(crate::translate::request::DUMMY_THOUGHT_SIGNATURE),
        "a dummy signature is in the request"
    );

    let mut stream = model
        .invoke(request, CancellationToken::new())
        .await
        .unwrap();
    let mut reason = None;
    while let Some(ev) = stream.next().await {
        if let ModelEvent::Finish { reason: r } = ev.expect("turn 2 stream error") {
            reason = Some(r);
        }
    }
    // `Other` (for example MALFORMED_FUNCTION_CALL), `ContentFilter` and
    // `Length` are failures.
    match reason {
        Some(FinishReason::Stop | FinishReason::ToolCalls) => {}
        other => panic!("turn 2 finish is not Stop or ToolCalls: {other:?}"),
    }
}
