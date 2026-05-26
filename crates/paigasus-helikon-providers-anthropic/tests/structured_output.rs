//! End-to-end test of `ResponseFormat::JsonSchema` via forced-tool synthesis.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelError, ModelEvent,
    ModelRequest, ResponseFormat, ToolChoice, ToolDef,
};
use paigasus_helikon_providers_anthropic::AnthropicModel;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

const SYNTH_NAME: &str = "__paigasus_structured_output__";

struct CapturingResponder {
    body: String,
}
impl Respond for CapturingResponder {
    fn respond(&self, _req: &wiremock::Request) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(self.body.clone(), "text/event-stream")
    }
}

fn synth_tool_use_stream() -> String {
    format!(
        "event: message_start\n\
         data: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":10}}}}}}\n\n\
         event: content_block_start\n\
         data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"tu_s\",\"name\":\"{name}\",\"input\":{{}}}}}}\n\n\
         event: content_block_delta\n\
         data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":\"{{\\\"name\\\":\\\"Ada\\\"}}\"}}}}\n\n\
         event: content_block_stop\n\
         data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
         event: message_delta\n\
         data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"tool_use\"}},\"usage\":{{\"output_tokens\":8}}}}\n\n\
         event: message_stop\n\
         data: {{\"type\":\"message_stop\"}}\n\n",
        name = SYNTH_NAME
    )
}

fn user(s: &str) -> Item {
    Item::UserMessage { content: vec![ContentPart::Text { text: s.to_owned() }] }
}

fn req_with(messages: Vec<Item>, tools: Vec<ToolDef>, mutate: impl FnOnce(&mut ModelRequest)) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = messages;
    r.tools = tools;
    mutate(&mut r);
    r
}

#[tokio::test]
async fn json_schema_synthesizes_forced_tool_and_remaps_to_text() {
    let responder = CapturingResponder { body: synth_tool_use_stream() };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(responder)
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = req_with(vec![user("Give me a Person.")], vec![], |r| {
        r.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Person".to_owned(),
            schema: serde_json::json!({"type": "object", "properties": {"name": {"type": "string"}}}),
            strict: true,
        });
    });
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let mut events = Vec::new();
    while let Some(ev) = s.next().await {
        events.push(ev.unwrap());
    }

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TokenDelta { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "{\"name\":\"Ada\"}");

    assert!(!events.iter().any(|e| matches!(e, ModelEvent::ToolCallDelta { .. })),
        "synthesized tool must NOT surface as ToolCallDelta");

    assert!(matches!(
        events.last().unwrap(),
        ModelEvent::Finish { reason: FinishReason::Stop },
    ), "tool_use stop_reason rewrites to Stop for synthesized-only path");

    // The mock captured the request body — verify synthesized tool + tool_choice.
    let received = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["tool_choice"]["type"], "tool");
    assert_eq!(body["tool_choice"]["name"], SYNTH_NAME);
    let tools = body["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == SYNTH_NAME));
}

#[tokio::test]
async fn json_schema_plus_caller_tool_choice_tool_returns_synchronous_other() {
    let server = MockServer::start().await;
    // No mount needed — the guard fires before the HTTP call.

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = req_with(
        vec![user("hi")],
        vec![ToolDef {
            name: "search".to_owned(),
            description: "".to_owned(),
            schema: serde_json::json!({}),
        }],
        |r| {
            r.model_settings.response_format = Some(ResponseFormat::JsonObject);
            r.model_settings.tool_choice = Some(ToolChoice::Tool { name: "search".to_owned() });
        },
    );
    let result = model.invoke(req, CancellationToken::new()).await;
    assert!(matches!(result, Err(ModelError::Other(_))), "expected ModelError::Other, got Ok");
}

#[tokio::test]
async fn reserved_tool_name_returns_synchronous_other() {
    let server = MockServer::start().await;
    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = req_with(
        vec![user("hi")],
        vec![ToolDef {
            name: SYNTH_NAME.to_owned(),
            description: "".to_owned(),
            schema: serde_json::json!({}),
        }],
        |_| {},
    );
    let result = model.invoke(req, CancellationToken::new()).await;
    assert!(matches!(result, Err(ModelError::Other(_))), "expected ModelError::Other, got Ok");
}
