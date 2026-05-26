//! `Vec<Item>` + `ModelRequest` → Anthropic Messages request body.
//!
//! Rules per the SMA-317 spec § "Wire translation". The translator
//! produces a `serde_json::Value` rather than typed structs to keep the
//! wire-snapshot tests readable.

use paigasus_helikon_core::{ContentPart, Item};
use serde_json::{json, Value};

/// Output of [`translate_messages`]: the top-level `system` field (string
/// or block-array form) and the `messages` array.
pub(crate) struct TranslatedMessages {
    pub(crate) system: Option<Value>,
    pub(crate) messages: Value,
}

/// Translate the conversation into Anthropic's request shape.
///
/// `system` is `None` when no `Item::System` is present. `messages` is
/// always an array.
pub(crate) fn translate_messages(items: &[Item]) -> TranslatedMessages {
    let mut system_text = String::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut pending_tool_use: Vec<Value> = Vec::new();

    fn flush_pending(messages: &mut Vec<Value>, pending: &mut Vec<Value>) {
        if !pending.is_empty() {
            messages.push(json!({
                "role": "assistant",
                "content": std::mem::take(pending),
            }));
        }
    }

    for item in items {
        match item {
            Item::System { content } => {
                flush_pending(&mut messages, &mut pending_tool_use);
                if !system_text.is_empty() {
                    system_text.push('\n');
                }
                system_text.push_str(&text_of(content));
            }
            Item::UserMessage { content } => {
                flush_pending(&mut messages, &mut pending_tool_use);
                messages.push(json!({
                    "role": "user",
                    "content": user_blocks(content),
                }));
            }
            Item::AssistantMessage { content, agent: _ } => {
                flush_pending(&mut messages, &mut pending_tool_use);
                messages.push(json!({
                    "role": "assistant",
                    "content": assistant_blocks(content),
                }));
            }
            Item::ToolCall { call_id, name, args } => {
                let block = json!({
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": args,
                });
                if let Some(last) = messages.last_mut().filter(|m| m["role"] == "assistant") {
                    last["content"].as_array_mut().unwrap().push(block);
                } else {
                    pending_tool_use.push(block);
                }
            }
            _ => {
                // Task 11 fills in ToolResult.
                tracing::warn!(
                    target: "paigasus::anthropic::translate",
                    "Item::ToolResult not yet implemented",
                );
            }
        }
    }

    flush_pending(&mut messages, &mut pending_tool_use);

    let system = if system_text.is_empty() {
        None
    } else {
        Some(Value::String(system_text))
    };
    TranslatedMessages { system, messages: Value::Array(messages) }
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

fn user_blocks(content: &[ContentPart]) -> Value {
    let blocks: Vec<Value> = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(json!({"type": "text", "text": text})),
            _ => None, // media + tool_result handled in later tasks
        })
        .collect();
    Value::Array(blocks)
}

fn assistant_blocks(content: &[ContentPart]) -> Value {
    let blocks: Vec<Value> = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(json!({"type": "text", "text": text})),
            ContentPart::ToolUse { call_id, name, args } => Some(json!({
                "type": "tool_use",
                "id": call_id,
                "name": name,
                "input": args,
            })),
            ContentPart::Reasoning { .. } => {
                tracing::warn!(
                    target: "paigasus::anthropic::translate",
                    "dropping ContentPart::Reasoning on input — signature round-trip not yet supported",
                );
                None
            }
            _ => None,
        })
        .collect();
    Value::Array(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> ContentPart {
        ContentPart::Text { text: t.to_owned() }
    }

    #[test]
    fn system_collapses_into_top_level_string() {
        let items = vec![Item::System {
            content: vec![text("be helpful")],
        }];
        let out = translate_messages(&items);
        assert_eq!(out.system, Some(Value::String("be helpful".to_owned())));
        assert_eq!(out.messages, json!([]));
    }

    #[test]
    fn multiple_system_items_concatenate_in_order() {
        let items = vec![
            Item::System { content: vec![text("first")] },
            Item::UserMessage { content: vec![text("hi")] },
            Item::System { content: vec![text("second")] },
        ];
        let out = translate_messages(&items);
        assert_eq!(
            out.system,
            Some(Value::String("first\nsecond".to_owned())),
            "all system items collapse into one top-level slot (order-loss vs surrounding turns)",
        );
    }

    #[test]
    fn user_text_emits_text_block() {
        let items = vec![Item::UserMessage {
            content: vec![text("hello")],
        }];
        let out = translate_messages(&items);
        assert_eq!(
            out.messages,
            json!([{"role": "user", "content": [{"type": "text", "text": "hello"}]}]),
        );
    }

    #[test]
    fn assistant_text_emits_text_block() {
        let items = vec![Item::AssistantMessage {
            content: vec![text("done")],
            agent: Some("planner".to_owned()),
        }];
        let out = translate_messages(&items);
        assert_eq!(
            out.messages,
            json!([{"role": "assistant", "content": [{"type": "text", "text": "done"}]}]),
        );
        // `agent` attribution is dropped (no Anthropic slot).
    }

    #[test]
    fn assistant_reasoning_is_always_dropped() {
        let items = vec![Item::AssistantMessage {
            content: vec![
                ContentPart::Reasoning { text: "scratch".to_owned() },
                text("answer"),
            ],
            agent: None,
        }];
        let out = translate_messages(&items);
        let content = &out.messages[0]["content"];
        assert_eq!(content.as_array().unwrap().len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "answer");
    }

    #[test]
    fn tool_call_folds_into_preceding_assistant() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![text("calling")],
                agent: None,
            },
            Item::ToolCall {
                call_id: "tu_1".to_owned(),
                name: "ping".to_owned(),
                args: json!({"host": "ex.com"}),
            },
        ];
        let out = translate_messages(&items);
        let msg = &out.messages[0];
        assert_eq!(msg["role"], "assistant");
        let blocks = msg["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], json!({"type": "text", "text": "calling"}));
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "tu_1");
        assert_eq!(blocks[1]["name"], "ping");
        assert_eq!(blocks[1]["input"], json!({"host": "ex.com"}));
    }

    #[test]
    fn standalone_tool_calls_synthesize_assistant_carrier() {
        let items = vec![
            Item::ToolCall {
                call_id: "tu_a".to_owned(),
                name: "a".to_owned(),
                args: json!({}),
            },
            Item::ToolCall {
                call_id: "tu_b".to_owned(),
                name: "b".to_owned(),
                args: json!({"x": 1}),
            },
        ];
        let out = translate_messages(&items);
        assert_eq!(out.messages.as_array().unwrap().len(), 1);
        let msg = &out.messages[0];
        assert_eq!(msg["role"], "assistant");
        let blocks = msg["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["id"], "tu_a");
        assert_eq!(blocks[1]["id"], "tu_b");
    }

    #[test]
    fn assistant_with_nested_tool_use_emits_tool_use_block() {
        let items = vec![Item::AssistantMessage {
            content: vec![
                text("ok"),
                ContentPart::ToolUse {
                    call_id: "tu_x".to_owned(),
                    name: "search".to_owned(),
                    args: json!({"q": "rust"}),
                },
            ],
            agent: None,
        }];
        let out = translate_messages(&items);
        let blocks = out.messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "tu_x");
        assert_eq!(blocks[1]["input"], json!({"q": "rust"}));
    }
}
