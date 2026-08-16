//! Request translation: core types → LiteLLM (OpenAI-compatible) JSON.

pub(crate) mod extras;
pub(crate) mod request;
pub(crate) mod response_format;
pub(crate) mod tools;

use paigasus_helikon_core::{ModelRequest, ToolChoice};
use serde_json::{Map, Value};

use crate::builder::Config;

/// Assemble the full streaming Chat Completions request body.
///
/// Always streaming, always `include_usage` — the trailing usage chunk is how
/// token counts arrive (SMA-451 design §9.5).
///
/// Deliberately absent from the body: `parallel_tool_calls` (carries no caller
/// instruction, so sending it only adds downstream risk), `n` (only the first
/// choice is read), and `previous_response_id` (an OpenAI Responses concept
/// this provider has no backend for).
pub(crate) fn build_request(cfg: &Config, req: &ModelRequest) -> Value {
    let mut body = Map::new();
    body.insert("model".to_owned(), Value::from(cfg.model_id.clone()));
    body.insert(
        "messages".to_owned(),
        strip_null_assistant_content(request::to_chat_messages(&req.messages)),
    );
    body.insert("stream".to_owned(), Value::Bool(true));
    body.insert(
        "stream_options".to_owned(),
        serde_json::json!({"include_usage": true}),
    );

    if !req.tools.is_empty() {
        body.insert("tools".to_owned(), tools::to_tools(&req.tools));
        if let Some(choice) = &req.model_settings.tool_choice {
            body.insert("tool_choice".to_owned(), tools::to_tool_choice(choice));
        }
    } else if matches!(
        req.model_settings.tool_choice,
        Some(ToolChoice::Required) | Some(ToolChoice::Tool { .. })
    ) {
        tracing::warn!(
            target: "paigasus::litellm::translate",
            "tool_choice requires a tool call but the request carries no tools; dropping tool_choice"
        );
    }

    if let Some(fmt) = &req.model_settings.response_format {
        if let Some(v) = response_format::to_response_format(fmt) {
            body.insert("response_format".to_owned(), v);
        }
    }
    if let Some(t) = req.model_settings.temperature {
        body.insert("temperature".to_owned(), Value::from(t));
    }
    if let Some(p) = req.model_settings.top_p {
        body.insert("top_p".to_owned(), Value::from(p));
    }
    if let Some(m) = req.model_settings.max_output_tokens {
        body.insert("max_tokens".to_owned(), Value::from(m));
    }

    extras::apply(&mut body, &cfg.extras);
    Value::Object(body)
}

/// Drop a literal `content: null` from every assistant-role message.
///
/// This is a **post-processing step on the assembled body, not a change to
/// the shared translator**: `translate::request::to_chat_messages` stays
/// byte-identical to the OpenAI crate's copy of the same function (SMA-451
/// design D6) — that identity is what the cross-crate parity test and the
/// duplication decision both rest on, so it must not be touched here.
///
/// `to_chat_messages` sets `content: Value::Null` on an assistant message
/// synthesized purely from pending tool calls (a turn with tool_calls but no
/// text — see `flush_pending`/`assistant_message` in `request.rs`), and it
/// does this identically in both crates. The OpenAI provider's wire output
/// never actually shows that literal `null`: `providers-openai/src/backend/
/// chat.rs` round-trips the translated `Value` through `async-openai`'s
/// typed `ChatCompletionRequestAssistantMessage`, whose `content` field is
/// `Option<_>` with `#[serde(skip_serializing_if = "Option::is_none")]` —
/// deserializing `null` yields `None`, which is omitted on serialization.
/// This crate has no `async-openai` dependency by design (SMA-451 design
/// D2: "own reqwest + eventsource-stream client") and would otherwise send
/// that null literally onto the wire — an artifact never exercised
/// end-to-end against a real backend (the live-proxy capture in Appendix B
/// could not provoke a tool-call turn). Stripping it here makes this
/// provider's wire shape match the sibling provider's exactly, so nothing
/// about "the OpenAI Chat Completions API treats `content: null` and an
/// absent key the same" needs to be assumed by anyone sending these bytes.
///
/// Scoped to `role == "assistant"` — the only role either translator can
/// produce a literal `content: null` for.
fn strip_null_assistant_content(mut messages: Value) -> Value {
    for m in messages.as_array_mut().into_iter().flatten() {
        if m.get("role") == Some(&Value::from("assistant"))
            && m.get("content") == Some(&Value::Null)
        {
            m.as_object_mut()
                .expect("assistant message is an object")
                .remove("content");
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::RESERVED_BODY_KEYS;
    use paigasus_helikon_core::{
        ContentPart, Item, MediaSource, ModelRequest, ResponseFormat, ToolDef,
    };

    fn cfg() -> Config {
        crate::LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .api_key("sk-test")
            .build()
            .unwrap()
            .config_for_test()
    }

    fn user(text: &str) -> ModelRequest {
        let mut r = ModelRequest::new();
        r.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text { text: text.into() }],
        }];
        r
    }

    fn tool_def() -> ToolDef {
        ToolDef {
            name: "get_weather".into(),
            description: "Look up weather".into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {"city": {"type": "string"}}
            }),
        }
    }

    #[test]
    fn snap_plain_text_turn() {
        insta::assert_json_snapshot!(build_request(&cfg(), &user("hi")));
    }

    #[test]
    fn snap_system_prompt() {
        let mut r = user("hi");
        r.messages.insert(
            0,
            Item::System {
                content: vec![ContentPart::Text {
                    text: "You are terse.".into(),
                }],
            },
        );
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_tools_and_tool_choice_auto() {
        let mut r = user("weather?");
        r.tools = vec![tool_def()];
        r.model_settings.tool_choice = Some(ToolChoice::Auto);
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_tool_choice_named() {
        let mut r = user("weather?");
        r.tools = vec![tool_def()];
        r.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "get_weather".into(),
        });
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_tool_call_and_result() {
        let mut r = ModelRequest::new();
        r.messages = vec![
            Item::UserMessage {
                content: vec![ContentPart::Text {
                    text: "weather?".into(),
                }],
            },
            Item::ToolCall {
                call_id: "call_1".into(),
                name: "get_weather".into(),
                args: serde_json::json!({"city": "Berlin"}),
            },
            Item::ToolResult {
                call_id: "call_1".into(),
                content: vec![ContentPart::Text { text: "18C".into() }],
            },
        ];
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_structured_output_json_schema() {
        let mut r = user("give me json");
        r.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Answer".into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {"answer": {"type": "string"}}
            }),
            strict: true,
        });
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_inline_image_part() {
        let mut r = ModelRequest::new();
        r.messages = vec![Item::UserMessage {
            content: vec![
                ContentPart::Text {
                    text: "what is this?".into(),
                },
                ContentPart::Image {
                    source: MediaSource::Base64 {
                        mime_type: "image/png".into(),
                        data: "AAAA".into(),
                    },
                },
            ],
        }];
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_sampling_settings() {
        let mut r = user("hi");
        r.model_settings.temperature = Some(0.7);
        r.model_settings.top_p = Some(0.9);
        r.model_settings.max_output_tokens = Some(512);
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_litellm_extras() {
        let model = crate::LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .fallbacks(["backup-a", "backup-b"])
            .num_retries(2)
            .tags(["team:research"])
            .metadata("trace_id", "t-123")
            .extra_body(serde_json::json!({"guardrails": ["pii-check"]}))
            .build()
            .unwrap();
        insta::assert_json_snapshot!(build_request(&model.config_for_test(), &user("hi")));
    }

    /// An assistant turn synthesized purely from pending tool calls (no
    /// preceding text) must omit `content` entirely on the wire — not send
    /// it as `content: null`.
    ///
    /// `to_chat_messages` (duplicated byte-for-byte from
    /// `providers-openai`, per SMA-451 design D6 — see
    /// `translate/request.rs`) sets a literal `content: Value::Null` here;
    /// the OpenAI provider never shows that on the wire because
    /// `backend/chat.rs` round-trips the translated `Value` through
    /// `async-openai`'s typed `ChatCompletionRequestAssistantMessage`
    /// (`content: Option<_>` with `skip_serializing_if = "Option::is_none"`,
    /// which drops a deserialized `null`). This crate has no `async-openai`
    /// dependency (design D2) and would otherwise send the null literally —
    /// `build_request` strips it in a post-processing step so the wire
    /// shape matches the sibling provider's.
    #[test]
    fn assistant_tool_call_turn_emits_no_content_key() {
        let mut r = ModelRequest::new();
        r.messages = vec![Item::ToolCall {
            call_id: "call_1".into(),
            name: "get_weather".into(),
            args: serde_json::json!({"city": "Berlin"}),
        }];
        let v = build_request(&cfg(), &r);
        let messages = v["messages"].as_array().unwrap();
        let assistant_msg = messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("a synthesized assistant tool-call message");
        assert!(
            assistant_msg.get("content").is_none(),
            "assistant tool-call turn must omit `content` entirely (not \
             `null`, not `\"\"`), got: {assistant_msg}"
        );
    }

    // ── Invariants ──────────────────────────────────────────────────────

    #[test]
    fn parallel_tool_calls_is_never_sent() {
        let mut r = user("hi");
        r.tools = vec![tool_def()];
        let v = build_request(&cfg(), &r);
        assert!(v.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn previous_response_id_is_never_sent() {
        let mut r = user("hi");
        r.model_settings.previous_response_id = Some("resp_123".into());
        let v = build_request(&cfg(), &r);
        assert!(v.get("previous_response_id").is_none());
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains("resp_123"));
    }

    #[test]
    fn n_is_never_sent() {
        assert!(build_request(&cfg(), &user("hi")).get("n").is_none());
    }

    #[test]
    fn tool_choice_is_dropped_when_there_are_no_tools() {
        let mut r = user("hi");
        r.model_settings.tool_choice = Some(ToolChoice::Required);
        assert!(build_request(&cfg(), &r).get("tool_choice").is_none());
    }

    /// Catches "we added a new body field and forgot to reserve it".
    ///
    /// Every top-level key the translator can emit must be either reserved
    /// against `extra_body` or a known LiteLLM extra. A new field added to
    /// `build_request` without a matching `RESERVED_BODY_KEYS` entry would
    /// silently become forgeable by callers.
    #[test]
    fn every_emitted_top_level_key_is_reserved_or_a_known_extra() {
        const KNOWN_EXTRAS: &[&str] = &["fallbacks", "num_retries", "metadata", "tags"];

        let model = crate::LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .fallbacks(["b"])
            .num_retries(1)
            .tags(["t"])
            .metadata("trace_id", "x")
            .build()
            .unwrap();

        let mut r = user("hi");
        r.tools = vec![tool_def()];
        r.model_settings.tool_choice = Some(ToolChoice::Auto);
        r.model_settings.response_format = Some(ResponseFormat::JsonObject);
        r.model_settings.temperature = Some(0.5);
        r.model_settings.top_p = Some(0.9);
        r.model_settings.max_output_tokens = Some(64);

        let v = build_request(&model.config_for_test(), &r);
        for key in v.as_object().unwrap().keys() {
            assert!(
                RESERVED_BODY_KEYS.contains(&key.as_str()) || KNOWN_EXTRAS.contains(&key.as_str()),
                "body key `{key}` is neither reserved nor a known LiteLLM extra \
                 — add it to RESERVED_BODY_KEYS or to KNOWN_EXTRAS here"
            );
        }
    }
}
