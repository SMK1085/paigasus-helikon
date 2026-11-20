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
    /// `true` when the last user message produced no message, no kept
    /// message or tool item follows it (dropped assistant items do not
    /// count), and the last sent message is an assistant one.
    pub(crate) trailing_message_dropped: bool,
}

/// Translate the conversation into Anthropic's request shape.
///
/// `system` is `None` when no `Item::System` has non-blank text. `messages`
/// is always an array. Text parts that are empty or whitespace only, and
/// messages left with no blocks, are dropped because the API rejects them.
pub(crate) fn translate_messages(items: &[Item]) -> TranslatedMessages {
    let mut system_text = String::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut pending_tool_use: Vec<Value> = Vec::new();
    let mut pending_tool_results: Vec<Value> = Vec::new();
    let mut last_item_dropped = false;

    fn flush_pending_assistant(messages: &mut Vec<Value>, pending: &mut Vec<Value>) {
        if !pending.is_empty() {
            messages.push(json!({
                "role": "assistant",
                "content": std::mem::take(pending),
            }));
        }
    }

    fn flush_pending_user(messages: &mut Vec<Value>, pending: &mut Vec<Value>) {
        if !pending.is_empty() {
            messages.push(json!({
                "role": "user",
                "content": std::mem::take(pending),
            }));
        }
    }

    for item in items {
        match item {
            Item::System { content } => {
                flush_pending_assistant(&mut messages, &mut pending_tool_use);
                flush_pending_user(&mut messages, &mut pending_tool_results);
                if !system_text.is_empty() {
                    system_text.push('\n');
                }
                system_text.push_str(&text_of(content));
            }
            Item::UserMessage { content } => {
                flush_pending_assistant(&mut messages, &mut pending_tool_use);
                // Pending tool_results go at the front of this user turn.
                let mut blocks: Vec<Value> = std::mem::take(&mut pending_tool_results);
                blocks.extend(user_blocks(content).as_array().unwrap().iter().cloned());
                last_item_dropped = blocks.is_empty();
                if !blocks.is_empty() {
                    messages.push(json!({"role": "user", "content": blocks}));
                }
            }
            Item::AssistantMessage { content, agent: _ } => {
                let blocks = assistant_blocks(content);
                // An assistant item with no blocks acts as if it were absent:
                // it flushes nothing (so an open tool round stays whole) and
                // leaves the flag as it is (a drop of the user turn before
                // it still counts). A kept assistant message ends the drop.
                if !blocks.as_array().unwrap().is_empty() {
                    flush_pending_assistant(&mut messages, &mut pending_tool_use);
                    flush_pending_user(&mut messages, &mut pending_tool_results);
                    last_item_dropped = false;
                    messages.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            Item::ToolCall {
                call_id,
                name,
                args,
            } => {
                last_item_dropped = false;
                // A new tool_use follows any results already seen. Close
                // that user turn first only when the last assistant round is
                // fully answered; otherwise the call joins that round.
                if round_is_answered(&messages, &pending_tool_results) {
                    flush_pending_user(&mut messages, &mut pending_tool_results);
                }
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
            Item::ToolResult { call_id, content } => {
                last_item_dropped = false;
                flush_pending_assistant(&mut messages, &mut pending_tool_use);
                pending_tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": text_of(content),
                }));
            }
            _ => {
                tracing::warn!(
                    target: "paigasus::anthropic::translate",
                    "unknown Item variant; skipping",
                );
            }
        }
    }

    flush_pending_assistant(&mut messages, &mut pending_tool_use);
    flush_pending_user(&mut messages, &mut pending_tool_results);

    let trailing_message_dropped =
        last_item_dropped && messages.last().is_some_and(|m| m["role"] == "assistant");

    let system = if system_text.trim().is_empty() {
        None
    } else {
        Some(Value::String(system_text))
    };
    TranslatedMessages {
        system,
        messages: Value::Array(messages),
        trailing_message_dropped,
    }
}

/// `true` when every `tool_use` id in the trailing run of assistant messages
/// has a matching `tool_result` in `results`. The API joins adjacent
/// assistant messages into one turn, so the whole run is one round. Also
/// `true` when there is nothing to compare (no assistant message last), so
/// the results then flush as before.
fn round_is_answered(messages: &[Value], results: &[Value]) -> bool {
    messages
        .iter()
        .rev()
        .take_while(|m| m["role"] == "assistant")
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .filter(|b| b["type"] == "tool_use")
        .all(|b| results.iter().any(|r| r["tool_use_id"] == b["id"]))
}

/// The API rejects empty and whitespace-only text blocks.
fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
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
    use paigasus_helikon_core::MediaSource;
    let blocks: Vec<Value> = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } if is_blank(text) => None,
            ContentPart::Text { text } => Some(json!({"type": "text", "text": text})),
            ContentPart::ToolResult { call_id, content } => Some(json!({
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": text_of(content),
            })),
            ContentPart::Image { source } => match source {
                MediaSource::Url { url } => Some(json!({
                    "type": "image",
                    "source": {"type": "url", "url": url},
                })),
                MediaSource::Base64 { mime_type, data } => Some(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": mime_type,
                        "data": data,
                    },
                })),
                _ => {
                    tracing::warn!(
                        target: "paigasus::anthropic::translate",
                        "unsupported MediaSource variant; skipping image",
                    );
                    None
                }
            },
            _ => None,
        })
        .collect();
    Value::Array(blocks)
}

fn assistant_blocks(content: &[ContentPart]) -> Value {
    let blocks: Vec<Value> = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } if is_blank(text) => None,
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
            Item::System {
                content: vec![text("first")],
            },
            Item::UserMessage {
                content: vec![text("hi")],
            },
            Item::System {
                content: vec![text("second")],
            },
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
                ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                },
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
    fn tool_only_turn_then_assistant_text_keeps_order() {
        let items = vec![
            Item::UserMessage {
                content: vec![text("q")],
            },
            Item::ToolCall {
                call_id: "tu_1".into(),
                name: "ping".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_1".into(),
                content: vec![text("pong")],
            },
            Item::AssistantMessage {
                content: vec![text("done")],
                agent: None,
            },
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                {"role": "user", "content": [{"type": "text", "text": "q"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "pong"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "done"}]},
            ])
        );
    }

    #[test]
    fn tool_call_after_pending_result_does_not_fold_into_earlier_assistant() {
        let items = vec![
            Item::UserMessage {
                content: vec![text("q")],
            },
            Item::AssistantMessage {
                content: vec![text("a")],
                agent: None,
            },
            Item::ToolCall {
                call_id: "tu_1".into(),
                name: "ping".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_1".into(),
                content: vec![text("r1")],
            },
            Item::ToolCall {
                call_id: "tu_2".into(),
                name: "ping".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_2".into(),
                content: vec![text("r2")],
            },
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                {"role": "user", "content": [{"type": "text", "text": "q"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "a"},
                    {"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}}
                ]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "r1"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_2", "name": "ping", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_2", "content": "r2"}]},
            ])
        );
    }

    #[test]
    fn multi_round_tool_loop_without_assistant_text_alternates() {
        let items = vec![
            Item::UserMessage {
                content: vec![text("q")],
            },
            Item::ToolCall {
                call_id: "tu_1".into(),
                name: "ping".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_1".into(),
                content: vec![text("r1")],
            },
            Item::ToolCall {
                call_id: "tu_2".into(),
                name: "ping".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_2".into(),
                content: vec![text("r2")],
            },
            Item::AssistantMessage {
                content: vec![text("done")],
                agent: None,
            },
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                {"role": "user", "content": [{"type": "text", "text": "q"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "r1"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_2", "name": "ping", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_2", "content": "r2"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "done"}]},
            ])
        );
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

    #[test]
    fn tool_result_coalesces_into_following_user_message() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "tu_1".to_owned(),
                    name: "ping".to_owned(),
                    args: json!({}),
                }],
                agent: None,
            },
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![text("pong")],
            },
            Item::UserMessage {
                content: vec![text("now what?")],
            },
        ];
        let out = translate_messages(&items);
        let arr = out.messages.as_array().unwrap();
        assert_eq!(arr.len(), 2, "must not produce consecutive user turns");
        assert_eq!(arr[0]["role"], "assistant");
        assert_eq!(arr[1]["role"], "user");
        let blocks = arr[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0]["type"], "tool_result",
            "tool_result precedes text"
        );
        assert_eq!(blocks[0]["tool_use_id"], "tu_1");
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[1]["text"], "now what?");
    }

    /// SMA-804: a checkpoint `Inject` leaves a tool result directly followed
    /// by a user message. The translator folds both into one user turn.
    #[test]
    fn tool_result_then_user_message_request_shape() {
        let items = vec![
            Item::UserMessage {
                content: vec![text("start")],
            },
            Item::AssistantMessage {
                content: vec![text("calling")],
                agent: None,
            },
            Item::ToolCall {
                call_id: "tu_1".to_owned(),
                name: "ping".to_owned(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![text("pong")],
            },
            Item::UserMessage {
                content: vec![text("news")],
            },
        ];
        let out = translate_messages(&items);
        assert_eq!(
            out.messages,
            json!([
                {"role": "user", "content": [{"type": "text", "text": "start"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "calling"},
                    {"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tu_1", "content": "pong"},
                    {"type": "text", "text": "news"},
                ]},
            ])
        );
    }

    fn user(t: &str) -> Item {
        Item::UserMessage {
            content: vec![text(t)],
        }
    }

    fn asst(t: &str) -> Item {
        Item::AssistantMessage {
            content: vec![text(t)],
            agent: None,
        }
    }

    fn call(id: &str) -> Item {
        Item::ToolCall {
            call_id: id.into(),
            name: "ping".into(),
            args: json!({}),
        }
    }

    fn result(id: &str) -> Item {
        Item::ToolResult {
            call_id: id.into(),
            content: vec![text("pong")],
        }
    }

    fn utext(t: &str) -> Value {
        json!({"role": "user", "content": [{"type": "text", "text": t}]})
    }

    fn atext(t: &str) -> Value {
        json!({"role": "assistant", "content": [{"type": "text", "text": t}]})
    }

    #[test]
    fn empty_text_part_is_dropped() {
        let out = translate_messages(&[user("")]);
        assert_eq!(out.messages, json!([]));
    }

    #[test]
    fn whitespace_only_text_part_is_dropped() {
        let out = translate_messages(&[user("  \n\t")]);
        assert_eq!(out.messages, json!([]));
    }

    #[test]
    fn blank_text_is_dropped_but_real_text_kept_verbatim() {
        let items = vec![Item::UserMessage {
            content: vec![text(""), text("  hi  "), text(" ")],
        }];
        assert_eq!(
            translate_messages(&items).messages,
            json!([utext("  hi  ")])
        );
    }

    #[test]
    fn blank_user_text_keeps_pending_tool_results() {
        let items = vec![user("q"), call("tu_1"), result("tu_1"), user("")];
        let out = translate_messages(&items);
        assert_eq!(
            out.messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "pong"}]},
            ])
        );
        assert!(!out.trailing_message_dropped);
    }

    #[test]
    fn blank_assistant_text_is_dropped() {
        let items = vec![Item::AssistantMessage {
            content: vec![text("  "), text("ok")],
            agent: None,
        }];
        assert_eq!(translate_messages(&items).messages, json!([atext("ok")]));
    }

    #[test]
    fn assistant_with_no_blocks_is_not_sent() {
        let items = vec![user("q"), asst(""), user("r")];
        assert_eq!(
            translate_messages(&items).messages,
            json!([utext("q"), utext("r")])
        );
    }

    #[test]
    fn reasoning_only_assistant_is_not_sent() {
        let items = vec![
            user("q"),
            Item::AssistantMessage {
                content: vec![ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                }],
                agent: None,
            },
            user("r"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([utext("q"), utext("r")])
        );
    }

    fn tool_turn_then_done() -> Value {
        json!([
            utext("q"),
            {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "pong"}]},
            atext("done"),
        ])
    }

    #[test]
    fn reasoning_only_assistant_before_tool_call_keeps_order() {
        let items = vec![
            user("q"),
            Item::AssistantMessage {
                content: vec![ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                }],
                agent: None,
            },
            call("tu_1"),
            result("tu_1"),
            asst("done"),
        ];
        assert_eq!(translate_messages(&items).messages, tool_turn_then_done());
    }

    #[test]
    fn whitespace_assistant_before_tool_call_keeps_order() {
        let items = vec![
            user("q"),
            asst("\n\n"),
            call("tu_1"),
            result("tu_1"),
            asst("done"),
        ];
        assert_eq!(translate_messages(&items).messages, tool_turn_then_done());
    }

    #[test]
    fn blank_assistant_after_kept_assistant_folds_tool_call_into_kept() {
        let items = vec![user("q"), asst("a"), asst(" "), call("tu_1")];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [
                    {"type": "text", "text": "a"},
                    {"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}},
                ]},
            ])
        );
    }

    #[test]
    fn replayed_blank_user_between_real_turns_is_dropped() {
        let items = vec![user("q"), asst("a"), user(""), user("r")];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a"), utext("r")]));
        assert!(!out.trailing_message_dropped);
    }

    #[test]
    fn trailing_blank_user_after_assistant_sets_flag() {
        let items = vec![user("q"), asst("a"), user("")];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a")]));
        assert!(out.trailing_message_dropped);
    }

    #[test]
    fn caller_prefill_does_not_set_flag() {
        let items = vec![user("q"), asst("a"), user(""), asst("b")];
        assert!(!translate_messages(&items).trailing_message_dropped);
    }

    fn tu(id: &str) -> Value {
        json!({"type": "tool_use", "id": id, "name": "ping", "input": {}})
    }

    fn tr(id: &str) -> Value {
        json!({"type": "tool_result", "tool_use_id": id, "content": "pong"})
    }

    /// Adjacent assistant messages are one API turn, so a round counts as
    /// answered only when every call in all of them has a result.
    #[test]
    fn round_spanning_adjacent_assistant_messages_is_not_split() {
        let nested = |id: &str| Item::AssistantMessage {
            content: vec![ContentPart::ToolUse {
                call_id: id.to_owned(),
                name: "ping".to_owned(),
                args: json!({}),
            }],
            agent: None,
        };
        let items = vec![
            user("q"),
            nested("t1"),
            nested("t2"),
            result("t2"),
            call("t3"),
            result("t1"),
            result("t3"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [tu("t1")]},
                {"role": "assistant", "content": [tu("t2"), tu("t3")]},
                {"role": "user", "content": [tr("t2"), tr("t1"), tr("t3")]},
            ])
        );
    }

    #[test]
    fn interleaved_parallel_round_is_not_split() {
        let items = vec![
            user("q"),
            asst("a"),
            call("t1"),
            call("t2"),
            result("t1"),
            call("t3"),
            result("t2"),
            result("t3"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [{"type": "text", "text": "a"}, tu("t1"), tu("t2"), tu("t3")]},
                {"role": "user", "content": [tr("t1"), tr("t2"), tr("t3")]},
            ])
        );
    }

    #[test]
    fn parallel_round_then_second_round_alternates() {
        let items = vec![
            user("q"),
            asst("a"),
            call("t1"),
            call("t2"),
            result("t1"),
            result("t2"),
            call("t3"),
            result("t3"),
            asst("done"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [{"type": "text", "text": "a"}, tu("t1"), tu("t2")]},
                {"role": "user", "content": [tr("t1"), tr("t2")]},
                {"role": "assistant", "content": [tu("t3")]},
                {"role": "user", "content": [tr("t3")]},
                atext("done"),
            ])
        );
    }

    #[test]
    fn resume_shape_call_result_call_result() {
        let items = vec![
            user("q"),
            asst("a"),
            call("t1"),
            result("t1"),
            call("t2"),
            result("t2"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [{"type": "text", "text": "a"}, tu("t1")]},
                {"role": "user", "content": [tr("t1")]},
                {"role": "assistant", "content": [tu("t2")]},
                {"role": "user", "content": [tr("t2")]},
            ])
        );
    }

    #[test]
    fn trailing_blank_user_then_blank_assistant_sets_flag() {
        let items = vec![user("q"), asst("a"), user(""), asst("")];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a")]));
        assert!(out.trailing_message_dropped);
    }

    /// A dropped assistant item inside an open parallel round does not
    /// split the round.
    #[test]
    fn dropped_assistant_inside_open_round_does_not_split_it() {
        let items = vec![
            user("q"),
            asst("a"),
            call("t1"),
            call("t2"),
            result("t1"),
            asst(""),
            call("t3"),
            result("t2"),
            result("t3"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [
                    {"type": "text", "text": "a"}, tu("t1"), tu("t2"), tu("t3"),
                ]},
                {"role": "user", "content": [tr("t1"), tr("t2"), tr("t3")]},
            ])
        );
    }

    /// A dropped assistant item between a tool result and a user message
    /// does not split the user turn.
    #[test]
    fn dropped_assistant_between_result_and_user_keeps_one_user_turn() {
        let items = vec![
            user("q"),
            asst("a"),
            call("t1"),
            result("t1"),
            asst(" "),
            user("r"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [{"type": "text", "text": "a"}, tu("t1")]},
                {"role": "user", "content": [tr("t1"), {"type": "text", "text": "r"}]},
            ])
        );
    }

    /// A caller prefill that ends with a reasoning-only assistant item is
    /// not a drop-induced prefill: no user turn was dropped.
    #[test]
    fn trailing_reasoning_assistant_without_dropped_user_does_not_set_flag() {
        let items = vec![
            user("q"),
            asst("a"),
            Item::AssistantMessage {
                content: vec![ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                }],
                agent: None,
            },
        ];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a")]));
        assert!(!out.trailing_message_dropped);
    }

    #[test]
    fn trailing_blank_assistant_without_dropped_user_does_not_set_flag() {
        let items = vec![user("q"), asst("a"), asst("")];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a")]));
        assert!(!out.trailing_message_dropped);
    }

    #[test]
    fn trailing_blank_user_then_reasoning_assistant_sets_flag() {
        let items = vec![
            user("q"),
            asst("a"),
            user(""),
            Item::AssistantMessage {
                content: vec![ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                }],
                agent: None,
            },
        ];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a")]));
        assert!(out.trailing_message_dropped);
    }

    #[test]
    fn whitespace_only_system_is_absent() {
        let items = vec![
            Item::System {
                content: vec![text("  \n")],
            },
            user("q"),
        ];
        assert_eq!(translate_messages(&items).system, None);
    }

    #[test]
    fn trailing_tool_result_synthesizes_user_turn() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "tu_1".to_owned(),
                    name: "ping".to_owned(),
                    args: json!({}),
                }],
                agent: None,
            },
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![text("pong")],
            },
        ];
        let out = translate_messages(&items);
        let arr = out.messages.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1]["role"], "user");
        let blocks = arr[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
    }

    #[test]
    fn adjacent_tool_results_coalesce_into_one_user_turn() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![
                    ContentPart::ToolUse {
                        call_id: "tu_a".to_owned(),
                        name: "a".to_owned(),
                        args: json!({}),
                    },
                    ContentPart::ToolUse {
                        call_id: "tu_b".to_owned(),
                        name: "b".to_owned(),
                        args: json!({}),
                    },
                ],
                agent: None,
            },
            Item::ToolResult {
                call_id: "tu_a".to_owned(),
                content: vec![text("A!")],
            },
            Item::ToolResult {
                call_id: "tu_b".to_owned(),
                content: vec![text("B!")],
            },
        ];
        let out = translate_messages(&items);
        let arr = out.messages.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let blocks = arr[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["tool_use_id"], "tu_a");
        assert_eq!(blocks[1]["tool_use_id"], "tu_b");
    }

    #[test]
    fn user_with_nested_tool_result_emits_native_block() {
        // The Anthropic-native shape: ContentPart::ToolResult inside a UserMessage.
        let items = vec![Item::UserMessage {
            content: vec![ContentPart::ToolResult {
                call_id: "tu_z".to_owned(),
                content: vec![text("native")],
            }],
        }];
        let out = translate_messages(&items);
        let blocks = out.messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "tu_z");
    }

    #[test]
    fn user_message_with_url_image_emits_url_source() {
        use paigasus_helikon_core::MediaSource;
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
        let out = translate_messages(&items);
        let blocks = out.messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], json!({"type": "text", "text": "look:"}));
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "url");
        assert_eq!(blocks[1]["source"]["url"], "https://example.com/cat.png");
    }

    #[test]
    fn user_message_with_base64_image_emits_base64_source() {
        use paigasus_helikon_core::MediaSource;
        let items = vec![Item::UserMessage {
            content: vec![ContentPart::Image {
                source: MediaSource::Base64 {
                    mime_type: "image/png".to_owned(),
                    data: "AAAA".to_owned(),
                },
            }],
        }];
        let out = translate_messages(&items);
        let block = &out.messages[0]["content"][0];
        assert_eq!(block["type"], "image");
        assert_eq!(block["source"]["type"], "base64");
        assert_eq!(block["source"]["media_type"], "image/png");
        assert_eq!(block["source"]["data"], "AAAA");
    }
}
