//! [`Vec<Item>`] → OpenAI Chat / Responses request messages.
//!
//! See SMA-316 spec § "Wire translation" for the rule table. Both backends
//! share many translation rules but the output JSON shape differs; we
//! build serde_json `Value`s directly rather than typed structs to keep
//! the test-fixture surface readable.

use paigasus_helikon_core::{ContentPart, Item, MediaSource};
use serde_json::{json, Value};

/// Translate a conversation `Vec<Item>` into OpenAI Chat Completions
/// `messages: [...]` form.
///
/// Rules per the SMA-316 spec's Wire translation § Chat Completions
/// table. Notably:
/// - Standalone `Item::ToolCall`s (no preceding `AssistantMessage` in
///   the same turn) are gathered into a synthesized
///   `{role: "assistant", content: null, tool_calls: [...]}`.
/// - `UserMessage` containing `ContentPart::ToolResult` (Anthropic
///   nested shape) hoists those parts into top-level `tool` messages.
/// - `ContentPart::Image`/`Audio` inside an `AssistantMessage` are
///   dropped with `tracing::warn!` (Chat assistant role accepts
///   string-or-null only).
/// - `ContentPart::Reasoning` is dropped (OpenAI Chat does not accept
///   reasoning input).
pub(crate) fn to_chat_messages(items: &[Item]) -> Value {
    let mut out: Vec<Value> = Vec::new();
    let mut pending_tool_calls: Vec<Value> = Vec::new();

    fn flush_pending(out: &mut Vec<Value>, pending: &mut Vec<Value>) {
        if !pending.is_empty() {
            out.push(json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": std::mem::take(pending),
            }));
        }
    }

    for item in items {
        match item {
            Item::System { content } => {
                flush_pending(&mut out, &mut pending_tool_calls);
                out.push(json!({"role": "system", "content": text_of(content)}));
            }
            Item::UserMessage { content } => {
                flush_pending(&mut out, &mut pending_tool_calls);
                emit_user_or_hoist(content, &mut out);
            }
            Item::AssistantMessage { content, agent: _ } => {
                flush_pending(&mut out, &mut pending_tool_calls);
                out.push(assistant_message(content));
            }
            Item::ToolCall { call_id, name, args } => {
                if let Some(last) = out.last_mut().filter(|m| m["role"] == "assistant") {
                    if last["tool_calls"].is_array() {
                        last["tool_calls"]
                            .as_array_mut()
                            .unwrap()
                            .push(openai_tool_call(call_id, name, args));
                    } else {
                        last["tool_calls"] = json!([openai_tool_call(call_id, name, args)]);
                    }
                } else {
                    pending_tool_calls.push(openai_tool_call(call_id, name, args));
                }
            }
            Item::ToolResult { call_id, content } => {
                flush_pending(&mut out, &mut pending_tool_calls);
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": text_of(content),
                }));
            }
            _ => {
                // Future Item variants (Item is #[non_exhaustive]) — skip with warn.
                tracing::warn!(
                    target = "paigasus::openai::translate",
                    "unknown Item variant; skipping"
                );
            }
        }
    }
    flush_pending(&mut out, &mut pending_tool_calls);
    Value::Array(out)
}

fn text_of(parts: &[ContentPart]) -> String {
    let mut s = String::new();
    for p in parts {
        if let ContentPart::Text { text } = p {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(text);
        }
    }
    s
}

fn emit_user_or_hoist(content: &[ContentPart], out: &mut Vec<Value>) {
    let mut user_parts: Vec<&ContentPart> = Vec::new();
    let mut hoisted: Vec<(String, String)> = Vec::new(); // (call_id, text)

    for p in content {
        match p {
            ContentPart::ToolResult { call_id, content } => {
                hoisted.push((call_id.clone(), text_of(content)));
            }
            other => user_parts.push(other),
        }
    }

    if !user_parts.is_empty() {
        out.push(user_message(&user_parts));
    }
    for (call_id, body) in hoisted {
        out.push(json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": body,
        }));
    }
}

fn user_message(parts: &[&ContentPart]) -> Value {
    // If everything is plain text, emit `content: "..."` (string).
    // Otherwise, emit the multimodal parts array.
    if parts.iter().all(|p| matches!(p, ContentPart::Text { .. })) {
        let mut text = String::new();
        for p in parts {
            if let ContentPart::Text { text: t } = p {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
        }
        return json!({"role": "user", "content": text});
    }
    let arr: Vec<Value> = parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(json!({"type": "text", "text": text})),
            ContentPart::Image { source } => Some(json!({"type": "image_url", "image_url": {"url": media_url(source)}})),
            ContentPart::Audio { source } => Some(json!({"type": "input_audio", "input_audio": {"data": media_url(source)}})),
            _ => None,
        })
        .collect();
    json!({"role": "user", "content": arr})
}

fn media_url(src: &MediaSource) -> String {
    match src {
        MediaSource::Url { url } => url.clone(),
        MediaSource::Base64 { mime_type, data } => format!("data:{mime_type};base64,{data}"),
        _ => String::new(),
    }
}

fn assistant_message(content: &[ContentPart]) -> Value {
    // Assistant role accepts string-or-null content + sibling tool_calls.
    // Hoist nested ToolUse blocks into tool_calls; warn on Image/Audio
    // parts (not representable); drop Reasoning.
    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    for p in content {
        match p {
            ContentPart::Text { text: t } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
            ContentPart::ToolUse { call_id, name, args } => {
                tool_calls.push(openai_tool_call(call_id, name, args));
            }
            ContentPart::Reasoning { .. } => { /* drop */ }
            ContentPart::Image { .. } | ContentPart::Audio { .. } => {
                tracing::warn!(
                    target = "paigasus::openai::translate",
                    "dropping multimodal ContentPart from AssistantMessage (Chat assistant role accepts only string content)"
                );
            }
            ContentPart::ToolResult { .. } => {
                tracing::warn!(
                    target = "paigasus::openai::translate",
                    "dropping ContentPart::ToolResult nested in AssistantMessage (only valid on UserMessage in Anthropic shape)"
                );
            }
            _ => { /* future variants */ }
        }
    }

    let content_value = if text.is_empty() && !tool_calls.is_empty() {
        Value::Null
    } else {
        Value::String(text)
    };

    let mut obj = serde_json::Map::new();
    obj.insert("role".to_owned(), Value::String("assistant".to_owned()));
    obj.insert("content".to_owned(), content_value);
    if !tool_calls.is_empty() {
        obj.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    Value::Object(obj)
}

fn openai_tool_call(call_id: &str, name: &str, args: &Value) -> Value {
    json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": args.to_string(),
        }
    })
}

// Suppress dead-code warnings until backends consume to_chat_messages.
#[allow(dead_code)]
const _SILENCE_DEAD_CODE: fn(&[Item]) -> Value = to_chat_messages;

#[cfg(test)]
mod chat_tests {
    use super::*;

    fn text(t: &str) -> ContentPart {
        ContentPart::Text { text: t.to_owned() }
    }

    #[test]
    fn system_message_is_text_only() {
        let items = vec![Item::System { content: vec![text("be helpful")] }];
        let out = to_chat_messages(&items);
        assert_eq!(out, json!([{"role": "system", "content": "be helpful"}]));
    }

    #[test]
    fn user_message_text_only() {
        let items = vec![Item::UserMessage { content: vec![text("hi")] }];
        let out = to_chat_messages(&items);
        assert_eq!(out, json!([{"role": "user", "content": "hi"}]));
    }

    #[test]
    fn user_message_with_image_url_emits_multimodal_parts() {
        let items = vec![Item::UserMessage {
            content: vec![
                text("look:"),
                ContentPart::Image {
                    source: MediaSource::Url {
                        url: "https://example.com/cat.png".to_owned(),
                    },
                },
            ],
        }];
        let out = to_chat_messages(&items);
        let parts = out[0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], json!({"type": "text", "text": "look:"}));
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "https://example.com/cat.png");
    }

    #[test]
    fn user_message_with_base64_image_renders_data_uri() {
        let items = vec![Item::UserMessage {
            content: vec![ContentPart::Image {
                source: MediaSource::Base64 {
                    mime_type: "image/png".to_owned(),
                    data: "AAAA".to_owned(),
                },
            }],
        }];
        let out = to_chat_messages(&items);
        assert_eq!(
            out[0]["content"][0]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
    }

    #[test]
    fn assistant_with_text_emits_assistant_role() {
        let items = vec![Item::AssistantMessage {
            content: vec![text("done")],
            agent: Some("planner".to_owned()),
        }];
        let out = to_chat_messages(&items);
        assert_eq!(out[0]["role"], "assistant");
        assert_eq!(out[0]["content"], "done");
        // `agent` attribution is intentionally dropped (no OpenAI slot).
        assert!(out[0].get("agent").is_none());
    }

    #[test]
    fn assistant_with_nested_tool_use_hoists_to_sibling_tool_calls() {
        let items = vec![Item::AssistantMessage {
            content: vec![
                text("calling..."),
                ContentPart::ToolUse {
                    call_id: "c1".to_owned(),
                    name: "search".to_owned(),
                    args: json!({"q": "rust"}),
                },
            ],
            agent: None,
        }];
        let out = to_chat_messages(&items);
        assert_eq!(out[0]["role"], "assistant");
        assert_eq!(out[0]["content"], "calling...");
        let tcs = out[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0]["id"], "c1");
        assert_eq!(tcs[0]["function"]["name"], "search");
        // arguments serialized as a JSON string per OpenAI's shape.
        let args_str = tcs[0]["function"]["arguments"].as_str().unwrap();
        let args: Value = serde_json::from_str(args_str).unwrap();
        assert_eq!(args, json!({"q": "rust"}));
    }

    #[test]
    fn assistant_image_content_part_is_dropped_with_warning() {
        let items = vec![Item::AssistantMessage {
            content: vec![
                text("here:"),
                ContentPart::Image {
                    source: MediaSource::Url {
                        url: "x".to_owned(),
                    },
                },
            ],
            agent: None,
        }];
        let out = to_chat_messages(&items);
        assert!(out[0]["content"].is_string() || out[0]["content"].is_null());
        assert_eq!(out[0]["content"], "here:");
    }

    #[test]
    fn standalone_tool_calls_synthesize_assistant_carrier() {
        let items = vec![
            Item::ToolCall {
                call_id: "c1".to_owned(),
                name: "a".to_owned(),
                args: json!({}),
            },
            Item::ToolCall {
                call_id: "c2".to_owned(),
                name: "b".to_owned(),
                args: json!({"x": 1}),
            },
        ];
        let out = to_chat_messages(&items);
        assert_eq!(out.as_array().unwrap().len(), 1);
        assert_eq!(out[0]["role"], "assistant");
        assert!(out[0]["content"].is_null());
        assert_eq!(out[0]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(out[0]["tool_calls"][0]["id"], "c1");
        assert_eq!(out[0]["tool_calls"][1]["id"], "c2");
    }

    #[test]
    fn tool_call_folds_into_preceding_assistant() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![text("calling")],
                agent: None,
            },
            Item::ToolCall {
                call_id: "c1".to_owned(),
                name: "ping".to_owned(),
                args: json!({}),
            },
        ];
        let out = to_chat_messages(&items);
        assert_eq!(out.as_array().unwrap().len(), 1);
        assert_eq!(out[0]["content"], "calling");
        assert_eq!(out[0]["tool_calls"][0]["id"], "c1");
    }

    #[test]
    fn tool_result_emits_tool_role() {
        let items = vec![Item::ToolResult {
            call_id: "c1".to_owned(),
            content: vec![text("ok")],
        }];
        let out = to_chat_messages(&items);
        assert_eq!(
            out,
            json!([{
                "role": "tool",
                "tool_call_id": "c1",
                "content": "ok",
            }])
        );
    }

    #[test]
    fn user_message_with_nested_tool_result_hoists_to_tool_role() {
        let items = vec![Item::UserMessage {
            content: vec![ContentPart::ToolResult {
                call_id: "c1".to_owned(),
                content: vec![text("nested ok")],
            }],
        }];
        let out = to_chat_messages(&items);
        assert_eq!(out.as_array().unwrap().len(), 1);
        assert_eq!(out[0]["role"], "tool");
        assert_eq!(out[0]["tool_call_id"], "c1");
        assert_eq!(out[0]["content"], "nested ok");
    }

    #[test]
    fn reasoning_content_part_dropped_on_chat() {
        let items = vec![Item::AssistantMessage {
            content: vec![
                ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                },
                text("answer"),
            ],
            agent: None,
        }];
        let out = to_chat_messages(&items);
        assert_eq!(out[0]["content"], "answer");
    }
}
