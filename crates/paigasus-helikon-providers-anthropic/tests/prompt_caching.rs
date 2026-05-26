//! Acceptance-criterion test: prompt caching reduces second-turn input tokens.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest, ToolDef,
};
use paigasus_helikon_providers_anthropic::{AnthropicModel, CacheStrategy};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

const SYSTEM: &str = "You are a helpful assistant in a verbose tone. \
                     Always answer concisely with units. \
                     Use the available tools when relevant.";

fn turn(input: u32, cache_creation: u32, cache_read: u32, output: u32) -> String {
    format!(
        "event: message_start\n\
         data: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":{input},\"cache_read_input_tokens\":{cache_read},\"cache_creation_input_tokens\":{cache_creation},\"output_tokens\":0}}}}}}\n\n\
         event: content_block_start\n\
         data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
         event: content_block_delta\n\
         data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"ok\"}}}}\n\n\
         event: content_block_stop\n\
         data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
         event: message_delta\n\
         data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":{output}}}}}\n\n\
         event: message_stop\n\
         data: {{\"type\":\"message_stop\"}}\n\n"
    )
}

struct SwitchingResponder {
    counter: std::sync::Mutex<usize>,
    bodies: Vec<String>,
}
impl Respond for SwitchingResponder {
    fn respond(&self, _req: &wiremock::Request) -> ResponseTemplate {
        let mut c = self.counter.lock().unwrap();
        let b = self.bodies.get(*c).cloned().unwrap_or_default();
        *c += 1;
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(b, "text/event-stream")
    }
}

fn user(s: &str) -> Item {
    Item::UserMessage { content: vec![ContentPart::Text { text: s.to_owned() }] }
}

fn req_with(messages: Vec<Item>, tools: Vec<ToolDef>) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = messages;
    r.tools = tools;
    r
}

#[tokio::test]
async fn second_turn_cached_input_tokens_reflects_prefix_reuse() {
    let server = MockServer::start().await;
    let bodies = vec![turn(2200, 2048, 0, 5), turn(150, 0, 2048, 5)];
    let responder = SwitchingResponder {
        counter: Default::default(),
        bodies,
    };

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(responder)
        .mount(&server)
        .await;

    let tool = ToolDef {
        name: "search".to_owned(),
        description: "Search the web.".to_owned(),
        schema: serde_json::json!({"type": "object", "properties": {"q": {"type": "string"}}}),
    };

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .cache_strategy(CacheStrategy::SystemAndTools)
        .build()
        .unwrap();

    let base_messages = vec![
        Item::System { content: vec![ContentPart::Text { text: SYSTEM.to_owned() }] },
        user("Tell me about Athens."),
    ];
    let req1 = req_with(base_messages.clone(), vec![tool.clone()]);
    let mut s = model.invoke(req1, CancellationToken::new()).await.unwrap();
    let mut events1 = Vec::new();
    while let Some(ev) = s.next().await {
        events1.push(ev.unwrap());
    }
    // First Usage on turn 1: cached should be 0.
    let usage1 = events1
        .iter()
        .find_map(|e| match e {
            ModelEvent::Usage { cached_input_tokens, .. } => Some(*cached_input_tokens),
            _ => None,
        })
        .unwrap();
    assert_eq!(usage1, Some(0), "turn 1 has no cache reads");

    // Turn 2: identical prefix + new question.
    let mut messages2 = base_messages.clone();
    messages2.push(Item::AssistantMessage {
        content: vec![ContentPart::Text { text: "ok".to_owned() }],
        agent: None,
    });
    messages2.push(user("And Sparta?"));
    let req2 = req_with(messages2, vec![tool]);
    let mut s = model.invoke(req2, CancellationToken::new()).await.unwrap();
    let mut events2 = Vec::new();
    while let Some(ev) = s.next().await {
        events2.push(ev.unwrap());
    }
    let usage2 = events2
        .iter()
        .find_map(|e| match e {
            ModelEvent::Usage { cached_input_tokens, .. } => Some(*cached_input_tokens),
            _ => None,
        })
        .unwrap();
    assert_eq!(usage2, Some(2048), "turn 2 reads the cached prefix");

    // Inspect the request bodies the mock saw to confirm cache markers were sent.
    let received = server.received_requests().await.expect("requests recorded");
    assert_eq!(received.len(), 2);
    for r in &received {
        let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        let system_marker = body["system"][0]["cache_control"]["type"].as_str();
        assert_eq!(system_marker, Some("ephemeral"), "system block carries cache marker");
        let tools_arr = body["tools"].as_array().unwrap();
        assert_eq!(
            tools_arr.last().unwrap()["cache_control"]["type"].as_str(),
            Some("ephemeral"),
            "last tool carries cache marker",
        );
    }

    // Capability flag reflects cache support.
    assert!(model.capabilities().prompt_caching);
}
