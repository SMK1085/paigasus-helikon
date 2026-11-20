//! `Vec<Item>` → Bedrock Converse `Message` / `SystemContentBlock` translation.
//!
//! Rules per SMA-329 spec §6 "Wire translation":
//! - `Item::System` → `SystemContentBlock::Text`; system blocks are never mixed
//!   into the `messages` slice.
//! - Blank text (empty or whitespace-only) is never sent: Converse rejects it.
//!   A dropped block leaves no placeholder. An `Item::System` with only blank
//!   text adds no system block.
//! - Adjacent same-role turns are joined into one turn.
//! - `Item::ToolCall` / `ContentPart::ToolUse` → `ContentBlock::ToolUse` on
//!   the assistant turn.
//! - `Item::ToolResult` queued pending → flushed as `ContentBlock::ToolResult`
//!   blocks prepended to the next user turn.
//! - No message with content after blank text is dropped → error.

use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, Message, SystemContentBlock, ToolResultBlock,
    ToolResultContentBlock, ToolUseBlock,
};
use paigasus_helikon_core::{ContentPart, Item, ModelError};
#[cfg(test)]
use serde_json::{json, Value};

use crate::document::value_to_document;

/// Output of [`items_to_messages`].
#[derive(Debug)]
pub(crate) struct TranslatedMessages {
    /// System prompt blocks (`SystemContentBlock::Text`) extracted from
    /// `Item::System` entries.
    pub(crate) system: Vec<SystemContentBlock>,
    /// Conversation turns. Adjacent same-role turns are joined.
    pub(crate) messages: Vec<Message>,
}

/// Translate the conversation into Bedrock Converse `Message`s.
///
/// Returns an error when no message with content remains. Empty or
/// whitespace-only text and unsupported parts are not sent.
pub(crate) fn items_to_messages(items: &[Item]) -> Result<TranslatedMessages, ModelError> {
    let mut system: Vec<SystemContentBlock> = Vec::new();
    let mut messages: Vec<Message> = Vec::new();

    // Pending assistant-side content blocks (ToolUse / Text for an assistant turn
    // that hasn't been flushed yet).
    let mut pending_assistant: Vec<ContentBlock> = Vec::new();
    // Pending ToolResult blocks to prepend to the next user turn.
    let mut pending_tool_results: Vec<ContentBlock> = Vec::new();

    /// Flush `pending_assistant` into an assistant Message on `messages`.
    ///
    /// If the last message is already an assistant turn, the pending content
    /// blocks are appended to it (coalescing) rather than creating a new turn.
    fn flush_assistant(messages: &mut Vec<Message>, pending: &mut Vec<ContentBlock>) {
        if !pending.is_empty() {
            let content = std::mem::take(pending);
            if messages
                .last()
                .map(|m| m.role == ConversationRole::Assistant)
                .unwrap_or(false)
            {
                // Coalesce: append to the existing assistant turn.
                let last = messages.last_mut().unwrap();
                last.content.extend(content);
            } else {
                let msg = Message::builder()
                    .role(ConversationRole::Assistant)
                    .set_content(Some(content))
                    .build()
                    .expect("role + non-empty content always valid");
                messages.push(msg);
            }
        }
    }

    /// Flush `pending_tool_results` into a user Message on `messages`.
    ///
    /// If the last message is already a user turn, the results are appended to
    /// it (coalescing) rather than creating a new turn.
    fn flush_user_results(messages: &mut Vec<Message>, pending: &mut Vec<ContentBlock>) {
        if !pending.is_empty() {
            let content = std::mem::take(pending);
            if messages
                .last()
                .map(|m| m.role == ConversationRole::User)
                .unwrap_or(false)
            {
                let last = messages.last_mut().unwrap();
                last.content.extend(content);
            } else {
                let msg = Message::builder()
                    .role(ConversationRole::User)
                    .set_content(Some(content))
                    .build()
                    .expect("role + non-empty content always valid");
                messages.push(msg);
            }
        }
    }

    for item in items {
        match item {
            Item::System { content } => {
                // System items are pulled out to the `system` slot; flush any
                // in-progress turns first so ordering is preserved.
                flush_assistant(&mut messages, &mut pending_assistant);
                flush_user_results(&mut messages, &mut pending_tool_results);
                let text = text_of(content);
                if !is_blank(&text) {
                    system.push(SystemContentBlock::Text(text));
                }
            }

            Item::UserMessage { content } => {
                flush_assistant(&mut messages, &mut pending_assistant);
                // Pending tool results precede any user text in this turn.
                let mut blocks: Vec<ContentBlock> = std::mem::take(&mut pending_tool_results);
                for part in content {
                    match part {
                        ContentPart::Text { text } => {
                            if !is_blank(text) {
                                blocks.push(ContentBlock::Text(text.clone()));
                            }
                        }
                        ContentPart::ToolResult { call_id, content } => {
                            // Native Anthropic-shaped ToolResult inside a UserMessage.
                            let result_block = build_tool_result_block(call_id, content);
                            blocks.push(ContentBlock::ToolResult(result_block));
                        }
                        _ => {
                            tracing::warn!(
                                target: "paigasus::bedrock::translate",
                                "unsupported ContentPart variant in UserMessage; skipping",
                            );
                        }
                    }
                }
                if !blocks.is_empty() {
                    if messages
                        .last()
                        .map(|m| m.role == ConversationRole::User)
                        .unwrap_or(false)
                    {
                        // Coalesce: append to the existing user turn.
                        let last = messages.last_mut().unwrap();
                        last.content.extend(blocks);
                    } else {
                        let msg = Message::builder()
                            .role(ConversationRole::User)
                            .set_content(Some(blocks))
                            .build()
                            .expect("role + non-empty content always valid");
                        messages.push(msg);
                    }
                }
            }

            Item::AssistantMessage { content, .. } => {
                let mut blocks: Vec<ContentBlock> = Vec::new();
                for part in content {
                    match part {
                        ContentPart::Text { text } => {
                            if !is_blank(text) {
                                blocks.push(ContentBlock::Text(text.clone()));
                            }
                        }
                        ContentPart::ToolUse {
                            call_id,
                            name,
                            args,
                        } => {
                            let tool_use = ToolUseBlock::builder()
                                .tool_use_id(call_id)
                                .name(name)
                                .input(value_to_document(args))
                                .build()
                                .expect("tool_use_id + name + input always valid");
                            blocks.push(ContentBlock::ToolUse(tool_use));
                        }
                        ContentPart::Reasoning { .. } => {
                            tracing::warn!(
                                target: "paigasus::bedrock::translate",
                                "dropping ContentPart::Reasoning — round-trip not supported on Bedrock",
                            );
                        }
                        _ => {
                            tracing::warn!(
                                target: "paigasus::bedrock::translate",
                                "unsupported ContentPart variant in AssistantMessage; skipping",
                            );
                        }
                    }
                }
                // An assistant item with no blocks acts as if it were absent:
                // it flushes nothing, so an open tool round stays whole.
                if !blocks.is_empty() {
                    // Queued calls go first, then the results that answer them.
                    flush_assistant(&mut messages, &mut pending_assistant);
                    flush_user_results(&mut messages, &mut pending_tool_results);
                    if messages
                        .last()
                        .map(|m| m.role == ConversationRole::Assistant)
                        .unwrap_or(false)
                    {
                        // Coalesce: append to the existing assistant turn.
                        let last = messages.last_mut().unwrap();
                        last.content.extend(blocks);
                    } else {
                        let msg = Message::builder()
                            .role(ConversationRole::Assistant)
                            .set_content(Some(blocks))
                            .build()
                            .expect("role + non-empty content always valid");
                        messages.push(msg);
                    }
                }
            }

            Item::ToolCall {
                call_id,
                name,
                args,
            } => {
                // A new call follows results already seen. Close that user
                // turn only when the last assistant round is fully answered;
                // otherwise the call joins that round.
                if !pending_tool_results.is_empty()
                    && round_is_answered(&messages, &pending_tool_results)
                {
                    flush_user_results(&mut messages, &mut pending_tool_results);
                }
                // Queue onto the pending assistant turn (flushed when a user
                // turn / system / end-of-input is encountered).
                let tool_use = ToolUseBlock::builder()
                    .tool_use_id(call_id)
                    .name(name)
                    .input(value_to_document(args))
                    .build()
                    .expect("tool_use_id + name + input always valid");
                pending_assistant.push(ContentBlock::ToolUse(tool_use));
            }

            Item::ToolResult { call_id, content } => {
                // Flush the preceding assistant turn that requested this result.
                flush_assistant(&mut messages, &mut pending_assistant);
                // Queue onto pending user / tool-result turn.
                let result_block = build_tool_result_block(call_id, content);
                pending_tool_results.push(ContentBlock::ToolResult(result_block));
            }

            _ => {
                tracing::warn!(
                    target: "paigasus::bedrock::translate",
                    "unknown Item variant; skipping",
                );
            }
        }
    }

    // Flush any remaining pending content.
    flush_assistant(&mut messages, &mut pending_assistant);
    flush_user_results(&mut messages, &mut pending_tool_results);

    // Guard: nothing to send.
    if messages.is_empty() {
        return Err(ModelError::Other(anyhow::anyhow!(
            "no message with content to send (empty or whitespace-only text and unsupported parts are not sent)"
        )));
    }

    Ok(TranslatedMessages { system, messages })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// `true` when `text` is empty or only whitespace. Converse rejects such blocks.
fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// `true` when every `ToolUse` id in the trailing run of assistant messages
/// has a matching `ToolResult` in `results`. Converse joins adjacent assistant
/// messages into one turn, so the whole run is one round. Also `true` when the
/// last message is not an assistant message, so the results then flush as
/// before.
fn round_is_answered(messages: &[Message], results: &[ContentBlock]) -> bool {
    messages
        .iter()
        .rev()
        .take_while(|m| m.role == ConversationRole::Assistant)
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolUse(tu) => Some(tu.tool_use_id.as_str()),
            _ => None,
        })
        .all(|id| {
            results.iter().any(|r| match r {
                ContentBlock::ToolResult(tr) => tr.tool_use_id == id,
                _ => false,
            })
        })
}

/// Extract and concatenate all text from a `Vec<ContentPart>`.
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

/// Build a `ToolResultBlock` from a call_id + content.
fn build_tool_result_block(call_id: &str, content: &[ContentPart]) -> ToolResultBlock {
    let text = text_of(content);
    ToolResultBlock::builder()
        .tool_use_id(call_id)
        .content(ToolResultContentBlock::Text(text))
        .build()
        .expect("tool_use_id + content always valid")
}

// ── Wire-projection helper (for snapshot tests) ───────────────────────────────

/// Project [`TranslatedMessages`] into a stable [`serde_json::Value`] for
/// snapshot tests. The projection is independent of SDK `Debug` output.
#[cfg(test)]
pub(crate) fn messages_to_wire_json(tm: &TranslatedMessages) -> Value {
    let system: Vec<Value> = tm
        .system
        .iter()
        .map(|s| match s {
            SystemContentBlock::Text(t) => json!({"type": "text", "text": t}),
            _ => json!({"type": "unknown"}),
        })
        .collect();

    let messages: Vec<Value> = tm
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                ConversationRole::User => "user",
                ConversationRole::Assistant => "assistant",
                _ => "unknown",
            };
            let content: Vec<Value> = m
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text(t) => json!({"type": "text", "text": t}),
                    ContentBlock::ToolUse(tu) => json!({
                        "type": "tool_use",
                        "toolUseId": tu.tool_use_id,
                        "name": tu.name,
                    }),
                    ContentBlock::ToolResult(tr) => {
                        let result_text = tr
                            .content
                            .iter()
                            .find_map(|c| {
                                if let ToolResultContentBlock::Text(t) = c {
                                    Some(t.as_str())
                                } else {
                                    None
                                }
                            })
                            .unwrap_or("");
                        json!({
                            "type": "tool_result",
                            "toolUseId": tr.tool_use_id,
                            "content": result_text,
                        })
                    }
                    _ => json!({"type": "unknown"}),
                })
                .collect();
            json!({"role": role, "content": content})
        })
        .collect();

    json!({"system": system, "messages": messages})
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::ContentPart;
    use serde_json::json;

    fn text(t: &str) -> ContentPart {
        ContentPart::Text { text: t.to_owned() }
    }

    fn user(content: Vec<ContentPart>) -> Item {
        Item::UserMessage { content }
    }

    fn assistant(content: Vec<ContentPart>) -> Item {
        Item::AssistantMessage {
            content,
            agent: None,
        }
    }

    fn system(t: &str) -> Item {
        Item::System {
            content: vec![text(t)],
        }
    }

    // Helper: parse the wire projection for assertions.
    fn wire(items: &[Item]) -> Result<Value, ModelError> {
        let tm = items_to_messages(items)?;
        Ok(messages_to_wire_json(&tm))
    }

    #[test]
    fn empty_conversation_returns_error() {
        let err = items_to_messages(&[]).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn system_only_returns_error() {
        // System items don't count as messages.
        let items = vec![system("be helpful")];
        let err = items_to_messages(&items).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn system_item_goes_to_system_slot() {
        let items = vec![system("be helpful"), user(vec![text("hi")])];
        let tm = items_to_messages(&items).unwrap();
        assert_eq!(tm.system.len(), 1);
        assert!(matches!(&tm.system[0], SystemContentBlock::Text(t) if t == "be helpful"));
        assert_eq!(tm.messages.len(), 1);
    }

    #[test]
    fn single_user_message_emits_one_user_turn() {
        let items = vec![user(vec![text("hello")])];
        let w = wire(&items).unwrap();
        assert_eq!(w["messages"].as_array().unwrap().len(), 1);
        assert_eq!(w["messages"][0]["role"], "user");
        assert_eq!(w["messages"][0]["content"][0]["type"], "text");
        assert_eq!(w["messages"][0]["content"][0]["text"], "hello");
    }

    #[test]
    fn assistant_first_is_sent_without_placeholder() {
        let items = vec![assistant(vec![text("done")])];
        let w = wire(&items).unwrap();
        assert_eq!(
            w["messages"],
            json!([{"role": "assistant", "content": [{"type": "text", "text": "done"}]}])
        );
    }

    #[test]
    fn tool_call_item_queues_onto_assistant_turn() {
        let items = vec![Item::ToolCall {
            call_id: "tu_1".to_owned(),
            name: "ping".to_owned(),
            args: json!({"host": "example.com"}),
        }];
        let w = wire(&items).unwrap();
        let msgs = w["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[0]["content"][0]["type"], "tool_use");
        assert_eq!(msgs[0]["content"][0]["toolUseId"], "tu_1");
    }

    #[test]
    fn tool_result_item_queues_onto_user_turn() {
        let items = vec![
            user(vec![text("call ping")]),
            assistant(vec![ContentPart::ToolUse {
                call_id: "tu_1".to_owned(),
                name: "ping".to_owned(),
                args: json!({}),
            }]),
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![text("pong")],
            },
        ];
        let tm = items_to_messages(&items).unwrap();
        assert_eq!(tm.messages.len(), 3);
        assert_eq!(tm.messages[0].role, ConversationRole::User);
        assert_eq!(tm.messages[1].role, ConversationRole::Assistant);
        assert_eq!(tm.messages[2].role, ConversationRole::User);
        let user_content = &tm.messages[2].content;
        assert_eq!(user_content.len(), 1);
        assert!(matches!(&user_content[0], ContentBlock::ToolResult(_)));
    }

    #[test]
    fn tool_result_followed_by_user_message_merges_into_one_user_turn() {
        let items = vec![
            user(vec![text("call ping")]),
            assistant(vec![ContentPart::ToolUse {
                call_id: "tu_1".to_owned(),
                name: "ping".to_owned(),
                args: json!({}),
            }]),
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![text("pong")],
            },
            user(vec![text("now what?")]),
        ];
        let w = wire(&items).unwrap();
        let msgs = w["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "must not produce consecutive user turns");
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        let user_msg = &msgs[2];
        assert_eq!(user_msg["role"], "user");
        let content = user_msg["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[1]["type"], "text");
    }

    /// SMA-804: a checkpoint `Inject` leaves a tool result directly followed
    /// by a user message. Bedrock merges both into one user turn.
    #[test]
    fn tool_result_then_user_message_request_shape() {
        let items = vec![
            user(vec![text("start")]),
            assistant(vec![text("calling")]),
            Item::ToolCall {
                call_id: "tu_1".to_owned(),
                name: "ping".to_owned(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![text("pong")],
            },
            user(vec![text("news")]),
        ];
        let w = wire(&items).unwrap();
        assert_eq!(
            w["messages"],
            json!([
                {"role": "user", "content": [{"type": "text", "text": "start"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "calling"},
                    {"type": "tool_use", "toolUseId": "tu_1", "name": "ping"},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "toolUseId": "tu_1", "content": "pong"},
                    {"type": "text", "text": "news"},
                ]},
            ])
        );
    }

    #[test]
    fn multiple_system_items_all_go_to_system_slot() {
        let items = vec![system("rule 1"), system("rule 2"), user(vec![text("hi")])];
        let tm = items_to_messages(&items).unwrap();
        assert_eq!(tm.system.len(), 2);
        assert_eq!(tm.messages.len(), 1);
    }

    #[test]
    fn assistant_message_with_tool_use_part_emits_tool_use_block() {
        let items = vec![
            user(vec![text("do it")]),
            assistant(vec![
                text("ok"),
                ContentPart::ToolUse {
                    call_id: "tu_x".to_owned(),
                    name: "search".to_owned(),
                    args: json!({"q": "rust"}),
                },
            ]),
        ];
        let w = wire(&items).unwrap();
        let content = &w["messages"][1]["content"];
        assert_eq!(content.as_array().unwrap().len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["name"], "search");
    }

    // ── Fix C tests: coalesce adjacent same-role turns ────────────────────────

    #[test]
    fn adjacent_user_messages_coalesce_into_one_turn() {
        let items = vec![user(vec![text("first")]), user(vec![text("second")])];
        let w = wire(&items).unwrap();
        let msgs = w["messages"].as_array().unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "two adjacent user messages must merge into one turn"
        );
        assert_eq!(msgs[0]["role"], "user");
        let content = msgs[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["text"], "first");
        assert_eq!(content[1]["text"], "second");
    }

    #[test]
    fn assistant_message_followed_by_tool_call_stays_on_same_assistant_turn() {
        // Item::AssistantMessage + Item::ToolCall → single assistant message with both blocks.
        // AssistantMessage is pushed as an assistant turn, then ToolCall queues to
        // pending_assistant, which at end-of-input is flushed and coalesced into the
        // existing assistant turn.
        let items = vec![
            user(vec![text("do it")]),
            assistant(vec![text("ok, calling tool")]),
            Item::ToolCall {
                call_id: "tu_1".to_owned(),
                name: "ping".to_owned(),
                args: json!({}),
            },
        ];
        let w = wire(&items).unwrap();
        let msgs = w["messages"].as_array().unwrap();
        // user + one assistant turn (text + tool_use coalesced)
        assert_eq!(
            msgs.len(),
            2,
            "assistant text + tool_call must be one assistant turn"
        );
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        let content = msgs[1]["content"].as_array().unwrap();
        assert_eq!(
            content.len(),
            2,
            "assistant turn has text block + tool_use block"
        );
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
    }

    #[test]
    fn adjacent_tool_results_coalesce_into_one_user_turn() {
        let items = vec![
            user(vec![text("do both")]),
            assistant(vec![
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
            ]),
            Item::ToolResult {
                call_id: "tu_a".to_owned(),
                content: vec![text("A!")],
            },
            Item::ToolResult {
                call_id: "tu_b".to_owned(),
                content: vec![text("B!")],
            },
        ];
        let tm = items_to_messages(&items).unwrap();
        assert_eq!(tm.messages.len(), 3);
        assert_eq!(tm.messages[2].role, ConversationRole::User);
        assert_eq!(tm.messages[2].content.len(), 2);
    }

    // ── SMA-824: blank text is never sent ─────────────────────────────────────

    const NO_CONTENT: &str = "no message with content to send";

    fn t(role: &str, s: &str) -> Value {
        json!({"role": role, "content": [{"type": "text", "text": s}]})
    }

    fn tool_use_item(id: &str) -> ContentPart {
        ContentPart::ToolUse {
            call_id: id.to_owned(),
            name: "ping".to_owned(),
            args: json!({}),
        }
    }

    fn result_item(id: &str, s: &str) -> Item {
        Item::ToolResult {
            call_id: id.to_owned(),
            content: vec![text(s)],
        }
    }

    fn blank_between(blank: &str) -> Vec<Item> {
        vec![
            user(vec![text("q")]),
            assistant(vec![text("a")]),
            user(vec![text(blank)]),
            user(vec![text("r")]),
        ]
    }

    #[test]
    fn empty_text_part_is_dropped() {
        let w = wire(&blank_between("")).unwrap();
        assert_eq!(
            w["messages"],
            json!([t("user", "q"), t("assistant", "a"), t("user", "r")])
        );
    }

    #[test]
    fn whitespace_only_text_part_is_dropped() {
        let w = wire(&blank_between("  \n\t")).unwrap();
        assert_eq!(
            w["messages"],
            json!([t("user", "q"), t("assistant", "a"), t("user", "r")])
        );
    }

    #[test]
    fn unicode_whitespace_text_part_is_dropped() {
        let w = wire(&blank_between("\u{00A0}\u{2003}")).unwrap();
        assert_eq!(
            w["messages"],
            json!([t("user", "q"), t("assistant", "a"), t("user", "r")])
        );
    }

    #[test]
    fn blank_text_is_dropped_but_real_text_kept_verbatim() {
        let w = wire(&[user(vec![text(""), text("  hi  ")])]).unwrap();
        assert_eq!(w["messages"], json!([t("user", "  hi  ")]));
    }

    #[test]
    fn blank_user_text_keeps_pending_tool_results() {
        let items = vec![
            user(vec![text("q")]),
            assistant(vec![tool_use_item("tu_1")]),
            result_item("tu_1", "pong"),
            user(vec![text("")]),
        ];
        let w = wire(&items).unwrap();
        assert_eq!(
            w["messages"],
            json!([
                t("user", "q"),
                {"role": "assistant", "content": [{"type": "tool_use", "toolUseId": "tu_1", "name": "ping"}]},
                {"role": "user", "content": [{"type": "tool_result", "toolUseId": "tu_1", "content": "pong"}]},
            ])
        );
    }

    #[test]
    fn blank_assistant_text_is_dropped() {
        let items = vec![
            user(vec![text("q")]),
            assistant(vec![text("  "), text("a")]),
        ];
        let w = wire(&items).unwrap();
        assert_eq!(w["messages"], json!([t("user", "q"), t("assistant", "a")]));
    }

    #[test]
    fn assistant_with_no_blocks_is_not_sent() {
        let items = vec![
            user(vec![text("q")]),
            assistant(vec![text("")]),
            user(vec![text("r")]),
        ];
        let w = wire(&items).unwrap();
        assert_eq!(
            w["messages"],
            json!([{"role": "user", "content": [
                {"type": "text", "text": "q"},
                {"type": "text", "text": "r"},
            ]}])
        );
    }

    #[test]
    fn replayed_blank_user_between_real_turns_is_dropped() {
        let w = wire(&blank_between("")).unwrap();
        assert_eq!(
            w["messages"],
            json!([t("user", "q"), t("assistant", "a"), t("user", "r")])
        );
    }

    #[test]
    fn blank_first_user_gives_assistant_first() {
        let items = vec![
            user(vec![text("")]),
            assistant(vec![text("a")]),
            user(vec![text("b")]),
        ];
        let w = wire(&items).unwrap();
        assert_eq!(w["messages"], json!([t("assistant", "a"), t("user", "b")]));
    }

    #[test]
    fn whitespace_only_system_is_absent() {
        let tm = items_to_messages(&[system("  \n"), user(vec![text("hi")])]).unwrap();
        assert!(tm.system.is_empty());
        for items in [vec![system("")], vec![system("  "), user(vec![text("")])]] {
            let err = items_to_messages(&items).unwrap_err();
            assert!(matches!(err, ModelError::Other(_)));
            assert!(err.to_string().contains(NO_CONTENT), "{err}");
        }
    }

    #[test]
    fn blank_system_inside_open_round_still_flushes() {
        let build = |sys: &str| {
            vec![
                user(vec![text("q")]),
                assistant(vec![tool_use_item("tu_1")]),
                result_item("tu_1", "r"),
                system(sys),
                user(vec![text("next")]),
            ]
        };
        let blank = items_to_messages(&build(" ")).unwrap();
        let real = items_to_messages(&build("s")).unwrap();
        assert!(blank.system.is_empty());
        assert_eq!(
            messages_to_wire_json(&blank)["messages"],
            messages_to_wire_json(&real)["messages"]
        );
    }

    #[test]
    fn only_blank_user_returns_error() {
        let err = items_to_messages(&[user(vec![text("")])]).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
        assert!(err.to_string().contains(NO_CONTENT), "{err}");
    }

    // ── Tool rounds keep item order (SMA-824) ─────────────────────────────

    fn call_item(id: &str) -> Item {
        Item::ToolCall {
            call_id: id.to_owned(),
            name: "ping".to_owned(),
            args: json!({}),
        }
    }

    fn tu(id: &str) -> Value {
        json!({"type": "tool_use", "toolUseId": id, "name": "ping"})
    }

    fn tr(id: &str) -> Value {
        json!({"type": "tool_result", "toolUseId": id, "content": id.replace('t', "r")})
    }

    fn tx(s: &str) -> Value {
        json!({"type": "text", "text": s})
    }

    fn msg(role: &str, content: Vec<Value>) -> Value {
        json!({"role": role, "content": content})
    }

    /// `r1` answers `t1`, and so on.
    fn res(id: &str) -> Item {
        result_item(id, &id.replace('t', "r"))
    }

    fn two_rounds() -> Vec<Item> {
        vec![
            user(vec![text("q")]),
            assistant(vec![text("a")]),
            call_item("t1"),
            res("t1"),
            call_item("t2"),
        ]
    }

    fn two_rounds_expected() -> Vec<Value> {
        vec![
            msg("user", vec![tx("q")]),
            msg("assistant", vec![tx("a"), tu("t1")]),
            msg("user", vec![tr("t1")]),
            msg("assistant", vec![tu("t2")]),
        ]
    }

    #[test]
    fn tool_call_after_pending_result_does_not_fold_into_earlier_assistant() {
        let mut items = two_rounds();
        items.push(res("t2"));
        let mut exp = two_rounds_expected();
        exp.push(msg("user", vec![tr("t2")]));
        assert_eq!(wire(&items).unwrap()["messages"], json!(exp));
    }

    #[test]
    fn tool_call_after_pending_result_at_end_of_input() {
        assert_eq!(
            wire(&two_rounds()).unwrap()["messages"],
            json!(two_rounds_expected())
        );
    }

    #[test]
    fn tool_call_after_pending_result_at_system() {
        let mut items = two_rounds();
        items.push(system("s"));
        let tm = items_to_messages(&items).unwrap();
        assert_eq!(
            messages_to_wire_json(&tm)["messages"],
            json!(two_rounds_expected())
        );
        assert_eq!(tm.system.len(), 1);
        assert!(matches!(&tm.system[0], SystemContentBlock::Text(t) if t == "s"));
    }

    #[test]
    fn tool_call_after_pending_result_at_user_message() {
        let mut items = two_rounds();
        items.push(user(vec![text("x")]));
        let mut exp = two_rounds_expected();
        exp.push(msg("user", vec![tx("x")]));
        assert_eq!(wire(&items).unwrap()["messages"], json!(exp));
    }

    fn alternating_expected() -> Value {
        json!([
            msg("user", vec![tx("q")]),
            msg("assistant", vec![tu("t1")]),
            msg("user", vec![tr("t1")]),
            msg("assistant", vec![tu("t2")]),
            msg("user", vec![tr("t2")]),
            msg("assistant", vec![tx("done")]),
        ])
    }

    #[test]
    fn multi_round_tool_loop_without_assistant_text_alternates() {
        let items = vec![
            user(vec![text("q")]),
            call_item("t1"),
            res("t1"),
            call_item("t2"),
            res("t2"),
            assistant(vec![text("done")]),
        ];
        assert_eq!(wire(&items).unwrap()["messages"], alternating_expected());
    }

    #[test]
    fn reasoning_only_multi_round_loop_alternates() {
        let reasoning = || {
            assistant(vec![ContentPart::Reasoning {
                text: "thinking".to_owned(),
            }])
        };
        let items = vec![
            user(vec![text("q")]),
            reasoning(),
            call_item("t1"),
            res("t1"),
            reasoning(),
            call_item("t2"),
            res("t2"),
            assistant(vec![text("done")]),
        ];
        assert_eq!(wire(&items).unwrap()["messages"], alternating_expected());
    }

    #[test]
    fn whitespace_assistant_before_tool_call_keeps_order() {
        let items = vec![
            user(vec![text("q")]),
            assistant(vec![text("  ")]),
            call_item("t1"),
            res("t1"),
            assistant(vec![text("done")]),
        ];
        assert_eq!(
            wire(&items).unwrap()["messages"],
            json!([
                msg("user", vec![tx("q")]),
                msg("assistant", vec![tu("t1")]),
                msg("user", vec![tr("t1")]),
                msg("assistant", vec![tx("done")]),
            ])
        );
    }

    #[test]
    fn interleaved_parallel_round_is_not_split() {
        let items = vec![
            user(vec![text("q")]),
            assistant(vec![tool_use_item("t1"), tool_use_item("t2")]),
            res("t1"),
            call_item("t3"),
            res("t2"),
            res("t3"),
        ];
        assert_eq!(
            wire(&items).unwrap()["messages"],
            json!([
                msg("user", vec![tx("q")]),
                msg("assistant", vec![tu("t1"), tu("t2"), tu("t3")]),
                msg("user", vec![tr("t1"), tr("t2"), tr("t3")]),
            ])
        );
    }

    #[test]
    fn out_of_order_results_keep_one_round() {
        let items = vec![
            user(vec![text("q")]),
            assistant(vec![tool_use_item("t1"), tool_use_item("t2")]),
            res("t2"),
            res("t1"),
            assistant(vec![text("done")]),
        ];
        assert_eq!(
            wire(&items).unwrap()["messages"],
            json!([
                msg("user", vec![tx("q")]),
                msg("assistant", vec![tu("t1"), tu("t2")]),
                msg("user", vec![tr("t2"), tr("t1")]),
                msg("assistant", vec![tx("done")]),
            ])
        );
    }

    #[test]
    fn dropped_assistant_inside_open_round_does_not_split_it() {
        let items = vec![
            user(vec![text("q")]),
            assistant(vec![tool_use_item("t1"), tool_use_item("t2")]),
            res("t1"),
            assistant(vec![text("")]),
            res("t2"),
            assistant(vec![text("done")]),
        ];
        assert_eq!(
            wire(&items).unwrap()["messages"],
            json!([
                msg("user", vec![tx("q")]),
                msg("assistant", vec![tu("t1"), tu("t2")]),
                msg("user", vec![tr("t1"), tr("t2")]),
                msg("assistant", vec![tx("done")]),
            ])
        );
    }

    #[test]
    fn dropped_assistant_between_result_and_user_keeps_one_user_turn() {
        let items = vec![
            user(vec![text("q")]),
            assistant(vec![tool_use_item("t1")]),
            res("t1"),
            assistant(vec![ContentPart::Reasoning {
                text: "thinking".to_owned(),
            }]),
            user(vec![text("next")]),
        ];
        assert_eq!(
            wire(&items).unwrap()["messages"],
            json!([
                msg("user", vec![tx("q")]),
                msg("assistant", vec![tu("t1")]),
                msg("user", vec![tr("t1"), tx("next")]),
            ])
        );
    }

    #[test]
    fn assistant_message_flushes_calls_before_results() {
        let items = vec![
            user(vec![text("q")]),
            assistant(vec![text("a")]),
            call_item("t1"),
            call_item("t2"),
            res("t1"),
            call_item("t3"),
            assistant(vec![text("b")]),
            res("t2"),
            res("t3"),
        ];
        assert_eq!(
            wire(&items).unwrap()["messages"],
            json!([
                msg("user", vec![tx("q")]),
                msg("assistant", vec![tx("a"), tu("t1"), tu("t2"), tu("t3")]),
                msg("user", vec![tr("t1")]),
                msg("assistant", vec![tx("b")]),
                msg("user", vec![tr("t2"), tr("t3")]),
            ])
        );
    }
}
