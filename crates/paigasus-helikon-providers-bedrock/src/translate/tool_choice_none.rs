//! Handling of `ToolChoice::None` when the history holds a tool round.
//!
//! Converse rejects a history with `toolUse` or `toolResult` blocks unless the
//! request has a `toolConfig`. Each model family has its own strategy, chosen
//! by [`ModelFamily::tool_choice_none_strategy`].

use std::collections::HashSet;

use aws_sdk_bedrockruntime::types::{
    ContentBlock, Message, Tool, ToolInputSchema, ToolResultContentBlock, ToolSpecification,
};
use paigasus_helikon_core::{ModelError, ToolDef};
use serde_json::json;

use crate::document::{document_to_value, value_to_document};
use crate::family::{ModelFamily, ToolChoiceNoneStrategy};
use crate::translate::schema::Ruleset;
use crate::translate::tools::tool_specs;

/// Description of a tool spec that is derived from the history.
#[cfg_attr(not(test), expect(dead_code))] // SMA-833: wired into build_request and build_native later
const DERIVED_DESCRIPTION: &str = "A tool that this conversation used earlier.";

/// Returns `true` when a message holds a tool call or a tool result.
pub(crate) fn has_tool_round(messages: &[Message]) -> bool {
    messages.iter().any(|m| {
        m.content()
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse(_) | ContentBlock::ToolResult(_)))
    })
}

/// The result of [`apply_tool_choice_none`].
#[derive(Debug)]
#[cfg_attr(not(test), expect(dead_code))] // SMA-833: wired into build_request and build_native later
pub(crate) struct NoneOutcome {
    /// The messages to send.
    pub(crate) messages: Vec<Message>,
    /// The tools for the `toolConfig`. Empty when no `toolConfig` is needed.
    pub(crate) tools: Vec<Tool>,
    /// `true` when the request must carry `tool_choice: {"type": "none"}`.
    pub(crate) tool_choice_none: bool,
}

#[cfg_attr(not(test), expect(dead_code))] // SMA-833: wired into build_request and build_native later
fn unchanged(messages: Vec<Message>) -> NoneOutcome {
    NoneOutcome {
        messages,
        tools: Vec::new(),
        tool_choice_none: false,
    }
}

/// Apply `ToolChoice::None` to the messages and tools, per model family.
///
/// - No tool round: nothing changes.
/// - `AnthropicNoneField`: the `toolConfig` holds the user tools and one
///   derived spec for each tool name in the history that is not yet listed.
///   The request must then carry the `tool_choice` none field.
/// - `TextHistory`: each tool block becomes a text block.
///
/// # Errors
/// The errors of [`tool_specs`], when the history holds a tool round.
#[cfg_attr(not(test), expect(dead_code))] // SMA-833: wired into build_request and build_native later
pub(crate) fn apply_tool_choice_none(
    messages: Vec<Message>,
    user_tools: &[ToolDef],
    family: ModelFamily,
    ruleset: Ruleset,
) -> Result<NoneOutcome, ModelError> {
    if !has_tool_round(&messages) {
        return Ok(unchanged(messages));
    }
    match family.tool_choice_none_strategy() {
        ToolChoiceNoneStrategy::AnthropicNoneField => {
            let mut specs = tool_specs(user_tools, ruleset)?;
            let mut seen: HashSet<String> = specs.iter().map(|s| s.name().to_owned()).collect();
            for message in &messages {
                for block in message.content() {
                    let ContentBlock::ToolUse(tu) = block else {
                        continue;
                    };
                    if !seen.insert(tu.name().to_owned()) {
                        continue;
                    }
                    match derived_spec(tu.name()) {
                        Ok(spec) => specs.push(spec),
                        Err(e) => tracing::debug!(
                            target: "paigasus::bedrock::translate",
                            name = tu.name(),
                            error = %e,
                            "skipping a history tool name that the spec builder refuses",
                        ),
                    }
                }
            }
            if specs.is_empty() {
                return Ok(unchanged(messages));
            }
            Ok(NoneOutcome {
                messages,
                tools: specs.into_iter().map(Tool::ToolSpec).collect(),
                tool_choice_none: true,
            })
        }
        ToolChoiceNoneStrategy::TextHistory => {
            let (messages, changed) = history_as_text(messages);
            if changed {
                tracing::debug!(
                    target: "paigasus::bedrock::translate",
                    "tool_choice none: replayed the tool history as text",
                );
            }
            Ok(unchanged(messages))
        }
    }
}

/// Build a spec for a tool name from the history.
#[cfg_attr(not(test), expect(dead_code))] // SMA-833: wired into build_request and build_native later
fn derived_spec(
    name: &str,
) -> Result<ToolSpecification, aws_smithy_types::error::operation::BuildError> {
    ToolSpecification::builder()
        .name(name)
        .description(DERIVED_DESCRIPTION)
        .input_schema(ToolInputSchema::Json(value_to_document(
            &json!({"type": "object"}),
        )))
        .build()
}

/// Replace each tool block with a text block. Returns the flag "changed".
#[cfg_attr(not(test), expect(dead_code))] // SMA-833: wired into build_request and build_native later
fn history_as_text(messages: Vec<Message>) -> (Vec<Message>, bool) {
    let mut changed = false;
    let out = messages
        .into_iter()
        .map(|m| {
            let blocks: Vec<ContentBlock> = m
                .content()
                .iter()
                .map(|b| match b {
                    ContentBlock::ToolUse(tu) => {
                        changed = true;
                        let input = serde_json::to_string(&document_to_value(tu.input()))
                            .unwrap_or_else(|_| "null".to_owned());
                        ContentBlock::Text(format!(
                            "[tool call {}: {} {}]",
                            tu.tool_use_id(),
                            tu.name(),
                            input
                        ))
                    }
                    ContentBlock::ToolResult(tr) => {
                        changed = true;
                        let text = tr
                            .content()
                            .iter()
                            .filter_map(|c| match c {
                                ToolResultContentBlock::Text(t) => Some(t.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let quoted =
                            serde_json::to_string(&text).unwrap_or_else(|_| "\"\"".to_owned());
                        ContentBlock::Text(format!(
                            "[tool result {}: {}]",
                            tr.tool_use_id(),
                            quoted
                        ))
                    }
                    other => other.clone(),
                })
                .collect();
            Message::builder()
                .role(m.role().clone())
                .set_content(Some(blocks))
                .build()
                .unwrap_or(m)
        })
        .collect();
    (out, changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::request::items_to_messages;
    use paigasus_helikon_core::{ContentPart, Item};

    fn user_text(s: &str) -> Item {
        Item::UserMessage {
            content: vec![ContentPart::Text { text: s.to_owned() }],
        }
    }

    fn call(id: &str, name: &str, args: serde_json::Value) -> Item {
        Item::AssistantMessage {
            content: vec![ContentPart::ToolUse {
                call_id: id.to_owned(),
                name: name.to_owned(),
                args,
            }],
            agent: None,
        }
    }

    fn result(id: &str, text: &str) -> Item {
        Item::ToolResult {
            call_id: id.to_owned(),
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
        }
    }

    fn history(name: &str, args: serde_json::Value, res: &str) -> Vec<Message> {
        items_to_messages(&[
            user_text("What's my balance?"),
            call("tu_1", name, args),
            result("tu_1", res),
        ])
        .unwrap()
        .messages
    }

    fn def(name: &str) -> ToolDef {
        ToolDef::new(name, "a tool", json!({"type": "object"}))
    }

    fn names(tools: &[Tool]) -> Vec<String> {
        tools
            .iter()
            .map(|t| match t {
                Tool::ToolSpec(s) => s.name().to_owned(),
                _ => panic!("not a tool spec"),
            })
            .collect()
    }

    fn texts(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .flat_map(|m| m.content().iter())
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    fn apply(
        msgs: Vec<Message>,
        tools: &[ToolDef],
        family: ModelFamily,
    ) -> Result<NoneOutcome, ModelError> {
        apply_tool_choice_none(msgs, tools, family, Ruleset::Strict)
    }

    #[test]
    fn claude_keeps_user_tools_and_sets_flag() {
        let msgs = history("get_balance", json!({}), "ok");
        let out = apply(msgs.clone(), &[def("get_balance")], ModelFamily::Anthropic).unwrap();
        assert!(out.tool_choice_none);
        assert_eq!(names(&out.tools), ["get_balance"]);
        assert_eq!(out.messages, msgs);
    }

    #[test]
    fn claude_derives_spec_from_history() {
        let msgs = history("get_balance", json!({}), "ok");
        let out = apply(msgs, &[], ModelFamily::Anthropic).unwrap();
        assert!(out.tool_choice_none);
        assert_eq!(names(&out.tools), ["get_balance"]);
        let Tool::ToolSpec(s) = &out.tools[0] else {
            panic!()
        };
        assert_eq!(
            s.description(),
            Some("A tool that this conversation used earlier.")
        );
        let Some(ToolInputSchema::Json(d)) = s.input_schema() else {
            panic!()
        };
        assert_eq!(document_to_value(d), json!({"type": "object"}));
    }

    #[test]
    fn claude_derives_each_name_once_in_order() {
        let msgs = items_to_messages(&[
            user_text("q"),
            call("tu_1", "get_balance", json!({})),
            result("tu_1", "a"),
            call("tu_2", "search", json!({})),
            result("tu_2", "b"),
            call("tu_3", "get_balance", json!({})),
            result("tu_3", "c"),
        ])
        .unwrap()
        .messages;
        let out = apply(msgs, &[def("search")], ModelFamily::Anthropic).unwrap();
        assert_eq!(names(&out.tools), ["search", "get_balance"]);
    }

    #[test]
    fn claude_accepts_reserved_name_from_history() {
        let msgs = history("__paigasus_structured_output__", json!({}), "ok");
        let out = apply(msgs, &[], ModelFamily::Anthropic).unwrap();
        assert_eq!(names(&out.tools), ["__paigasus_structured_output__"]);
        assert!(out.tool_choice_none);
    }

    #[test]
    fn claude_rejects_reserved_user_tool_name() {
        let msgs = history("get_balance", json!({}), "ok");
        let err = apply(
            msgs,
            &[def("__paigasus_structured_output__")],
            ModelFamily::Anthropic,
        )
        .unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn claude_orphan_results_keep_shape() {
        let msgs = items_to_messages(&[user_text("q"), result("tu_9", "orphan")])
            .unwrap()
            .messages;
        let out = apply(msgs.clone(), &[], ModelFamily::Anthropic).unwrap();
        assert!(!out.tool_choice_none);
        assert!(out.tools.is_empty());
        assert_eq!(out.messages, msgs);
    }

    #[test]
    fn text_history_replaces_tool_blocks() {
        for family in [ModelFamily::AmazonNova, ModelFamily::Llama] {
            let msgs = history("get_balance", json!({"account_id": "acc_123"}), "$1,234.56");
            let out = apply(msgs, &[def("get_balance")], family).unwrap();
            assert!(!out.tool_choice_none);
            assert!(out.tools.is_empty());
            assert!(!has_tool_round(&out.messages));
            let t = texts(&out.messages);
            assert!(
                t.contains(&r#"[tool call tu_1: get_balance {"account_id":"acc_123"}]"#.to_owned())
            );
            assert!(t.contains(&r#"[tool result tu_1: "$1,234.56"]"#.to_owned()));
        }
    }

    #[test]
    fn text_history_sorts_input_keys() {
        let msgs = history("get_balance", json!({"z": 1, "a": {"y": 2, "b": 3}}), "ok");
        let out = apply(msgs, &[], ModelFamily::AmazonNova).unwrap();
        assert!(texts(&out.messages)
            .contains(&r#"[tool call tu_1: get_balance {"a":{"b":3,"y":2},"z":1}]"#.to_owned()));
    }

    #[test]
    fn text_history_escapes_result_text() {
        let msgs = history("get_balance", json!({}), "x]\n\"y\"");
        let out = apply(msgs, &[], ModelFamily::Llama).unwrap();
        assert!(texts(&out.messages).contains(&r#"[tool result tu_1: "x]\n\"y\""]"#.to_owned()));
    }

    #[test]
    fn no_tool_round_is_unchanged() {
        for family in [ModelFamily::Anthropic, ModelFamily::AmazonNova] {
            let msgs = items_to_messages(&[user_text("hi")]).unwrap().messages;
            let out = apply(
                msgs.clone(),
                &[def("__paigasus_structured_output__")],
                family,
            )
            .unwrap();
            assert!(!out.tool_choice_none);
            assert!(out.tools.is_empty());
            assert_eq!(out.messages, msgs);
        }
    }
}
