//! Streaming SSE fixture tests for the Anthropic provider.
//!
//! Note: wiremock serves the full fixture body in a single chunk; these
//! tests prove byte-level correctness, not resilience to slow chunk delivery.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelError, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_anthropic::AnthropicModel;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

fn user(s: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text { text: s.to_owned() }],
    }
}

fn req_with(messages: Vec<Item>) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = messages;
    r
}

async fn run_stream(
    server: &MockServer,
    fixture: &'static str,
) -> Vec<Result<ModelEvent, ModelError>> {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(fixture, "text/event-stream"),
        )
        .mount(server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(req_with(vec![user("hi")]), CancellationToken::new())
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(ev) = s.next().await {
        out.push(ev);
    }
    out
}

/// Assert the sequence contains exactly one `Finish` and that it is the final
/// event — the core contract at `paigasus_helikon_core::Model::invoke`
/// ("`Finish` is the terminal event; nothing follows it").
///
/// Applied to every well-formed fixture so a regression that drops, doubles,
/// or misplaces the terminal event cannot pass silently.
fn assert_exactly_one_terminal_finish(oks: &[ModelEvent]) {
    let finishes = oks
        .iter()
        .filter(|e| matches!(e, ModelEvent::Finish { .. }))
        .count();
    assert_eq!(finishes, 1, "expected exactly one Finish, got {oks:#?}");
    assert!(
        matches!(oks.last(), Some(ModelEvent::Finish { .. })),
        "Finish must be the last event, got {oks:#?}"
    );
}

#[tokio::test]
async fn text_only_stream_emits_usage_token_deltas_usage_finish() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/text_only.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    // First: Usage from message_start.
    assert!(matches!(
        oks[0],
        ModelEvent::Usage {
            input_tokens: 12,
            output_tokens: 0,
            ..
        }
    ));
    // Then two TokenDelta events.
    assert!(matches!(&oks[1], ModelEvent::TokenDelta { text } if text == "Hello"));
    assert!(matches!(&oks[2], ModelEvent::TokenDelta { text } if text == " world"));
    // Final Usage from message_delta then Finish::Stop.
    assert!(matches!(
        oks[3],
        ModelEvent::Usage {
            output_tokens: 5,
            ..
        }
    ));
    assert!(matches!(&oks[4], ModelEvent::Finish { reason } if *reason == FinishReason::Stop));
    assert_eq!(
        oks.len(),
        5,
        "well-formed stream must emit exactly these five events, got {oks:#?}"
    );
    assert_exactly_one_terminal_finish(&oks);
}

#[tokio::test]
async fn parallel_tool_use_stream_emits_two_tool_call_deltas() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/parallel_tool_use.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    let tc: Vec<&ModelEvent> = oks
        .iter()
        .filter(|e| matches!(e, ModelEvent::ToolCallDelta { .. }))
        .collect();
    assert_eq!(tc.len(), 2, "two tool calls");
    match tc[0] {
        ModelEvent::ToolCallDelta { call_id, name, .. } => {
            assert_eq!(call_id, "tu_a");
            assert_eq!(name.as_deref(), Some("a"));
        }
        _ => unreachable!(),
    }
    match tc[1] {
        ModelEvent::ToolCallDelta { call_id, name, .. } => {
            assert_eq!(call_id, "tu_b");
            assert_eq!(name.as_deref(), Some("b"));
        }
        _ => unreachable!(),
    }

    assert!(matches!(
        oks.last().unwrap(),
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls
        },
    ));
    assert_exactly_one_terminal_finish(&oks);
}

#[tokio::test]
async fn thinking_stream_emits_reasoning_delta_before_text_delta() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/thinking_then_text.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    let first_reasoning = oks
        .iter()
        .position(|e| matches!(e, ModelEvent::ReasoningDelta { .. }))
        .expect("reasoning delta present");
    let first_text = oks
        .iter()
        .position(|e| matches!(e, ModelEvent::TokenDelta { .. }))
        .expect("text delta present");
    assert!(
        first_reasoning < first_text,
        "reasoning must precede text in this fixture"
    );
    assert_exactly_one_terminal_finish(&oks);
}

#[tokio::test]
async fn stream_error_overloaded_terminates_with_unavailable() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/stream_error.txt");
    let events = run_stream(&server, fixture).await;
    assert!(
        !events
            .iter()
            .any(|r| matches!(r, Ok(ModelEvent::Finish { .. }))),
        "an error stream must emit no Finish, got {events:#?}"
    );
    let last = events.into_iter().last().unwrap();
    assert!(matches!(last, Err(ModelError::Unavailable)));
}

/// Two-turn fixture: serve stream 1 on the first POST, stream 2 on the second.
struct SwitchingResponder {
    counter: std::sync::Mutex<usize>,
    bodies: Vec<String>,
}
impl Respond for SwitchingResponder {
    fn respond(&self, _req: &wiremock::Request) -> ResponseTemplate {
        let mut c = self.counter.lock().unwrap();
        let body = self.bodies.get(*c).cloned().unwrap_or_default();
        *c += 1;
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(body, "text/event-stream")
    }
}

#[tokio::test]
async fn multi_turn_tool_use_continuation() {
    let raw = include_str!("fixtures/tool_use_then_continuation.txt");
    let parts: Vec<&str> = raw.split("# --- turn 2 ---\n").collect();
    assert_eq!(parts.len(), 2, "fixture must contain the turn-2 delimiter");
    let bodies = vec![parts[0].to_owned(), parts[1].to_owned()];

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(SwitchingResponder {
            counter: Default::default(),
            bodies,
        })
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();

    // Turn 1: prompt → expect text + tool_use + Finish::ToolCalls.
    let mut s = model
        .invoke(
            req_with(vec![user("weather in athens?")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut events1: Vec<_> = Vec::new();
    while let Some(ev) = s.next().await {
        events1.push(ev.unwrap());
    }
    assert!(events1.iter().any(
        |e| matches!(e, ModelEvent::ToolCallDelta { call_id, .. } if call_id == "tu_weather")
    ));
    assert!(matches!(
        events1.last().unwrap(),
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls
        },
    ));
    assert_exactly_one_terminal_finish(&events1);

    // Turn 2: append tool_result and re-invoke.
    let turn2_messages = vec![
        user("weather in athens?"),
        Item::AssistantMessage {
            content: vec![ContentPart::ToolUse {
                call_id: "tu_weather".to_owned(),
                name: "get_weather".to_owned(),
                args: serde_json::json!({"city": "Athens"}),
            }],
            agent: None,
        },
        Item::ToolResult {
            call_id: "tu_weather".to_owned(),
            content: vec![ContentPart::Text {
                text: "28C, sunny".to_owned(),
            }],
        },
    ];
    let mut s = model
        .invoke(req_with(turn2_messages), CancellationToken::new())
        .await
        .unwrap();
    let mut events2: Vec<_> = Vec::new();
    while let Some(ev) = s.next().await {
        events2.push(ev.unwrap());
    }
    assert!(events2
        .iter()
        .any(|e| matches!(e, ModelEvent::TokenDelta { text } if text.contains("28C"))));
    assert!(matches!(
        events2.last().unwrap(),
        ModelEvent::Finish {
            reason: FinishReason::Stop
        },
    ));
    assert_exactly_one_terminal_finish(&events2);
}

/// SMA-531: a response body that ends cleanly after `message_delta` — before
/// `message_stop` — must still deliver the terminal `Finish`. Before the fix
/// this stream emitted `[Usage, TokenDelta, TokenDelta, Usage]` and stopped.
#[tokio::test]
async fn clean_eof_after_message_delta_emits_finish() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/eof_after_message_delta.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    assert_eq!(
        oks.len(),
        5,
        "expected Usage, TokenDelta, TokenDelta, Usage, Finish -- got {oks:#?}"
    );
    assert_exactly_one_terminal_finish(&oks);
    assert!(
        matches!(oks.last().unwrap(), ModelEvent::Finish { reason } if *reason == FinishReason::Stop),
        "the buffered end_turn must flush as Finish::Stop, got {oks:#?}"
    );
}

/// SMA-531: the same guarantee for a byte-level cut. The partial `message_stop`
/// event has no blank-line terminator, so the SSE parser discards it at EOF and
/// the translator sees the same prefix as `eof_after_message_delta.txt`.
#[tokio::test]
async fn body_cut_inside_message_stop_emits_finish() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/body_cut_inside_message_stop.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    assert_eq!(
        oks.len(),
        5,
        "the partial message_stop must be discarded, leaving four events plus \
         the flushed Finish -- got {oks:#?}"
    );
    assert_exactly_one_terminal_finish(&oks);
}

/// SMA-531: no stop reason was ever observed, so a body that ends early must
/// NOT be reported as a clean `Stop`. Guards the flush against over-firing.
#[tokio::test]
async fn eof_mid_content_block_emits_no_finish() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/eof_mid_content_block.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    assert!(
        !oks.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
        "a stream with no observed stop reason must emit no Finish, got {oks:#?}"
    );
    assert_eq!(
        oks.len(),
        2,
        "expected Usage + one TokenDelta -- got {oks:#?}"
    );
}

/// SMA-531: a stop reason buffered by `message_delta` must be DISCARDED when a
/// mid-stream error follows, never flushed after the `Err`. The existing
/// `stream_error.txt` cannot catch this — that fixture has no `message_delta`,
/// so nothing is ever buffered there.
#[tokio::test]
async fn error_after_buffered_stop_reason_emits_no_finish() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/error_after_message_delta.txt");
    let events = run_stream(&server, fixture).await;

    assert!(
        !events
            .iter()
            .any(|r| matches!(r, Ok(ModelEvent::Finish { .. }))),
        "a buffered stop reason must be discarded on the error path, got {events:#?}"
    );
    assert!(
        matches!(events.last().unwrap(), Err(ModelError::Unavailable)),
        "the in-band error must be terminal, got {events:#?}"
    );
}
