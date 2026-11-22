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
use crate::translate::additional::{AdditionalFields, AdditionalToolChoice};
use crate::translate::schema::Ruleset;
use crate::translate::tools::{partition_tools, placeholder_spec};

/// Description of a tool spec that is derived from the history.
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
pub(crate) struct NoneOutcome {
    /// The messages to send.
    pub(crate) messages: Vec<Message>,
    /// The tools for the `toolConfig`. Empty when no `toolConfig` is needed.
    pub(crate) tools: Vec<Tool>,
    /// The typed tool entries and the tool choice for
    /// `additionalModelRequestFields`.
    pub(crate) additional: AdditionalFields,
}

fn unchanged(messages: Vec<Message>) -> NoneOutcome {
    NoneOutcome {
        messages,
        tools: Vec::new(),
        additional: AdditionalFields::default(),
    }
}

/// Apply `ToolChoice::None` to the messages and tools, per model family.
///
/// - No tool round: nothing changes.
/// - `AnthropicNoneField`: the `toolConfig` holds the generic user tools and
///   one derived spec for each tool name in the history that is not yet
///   listed. A typed tool name gets no derived spec. When the request has
///   typed tools, `additional` holds their entries. A placeholder spec fills
///   an empty `toolConfig`. The request then carries the `tool_choice` none
///   field.
/// - `TextHistory`: each tool block becomes a text block.
///
/// # Errors
/// The tools are split only when the strategy sends them. A request without a
/// tool round, or a text-history family, never validates the tool names.
///
/// # Errors
/// The errors of [`partition_tools`] and [`placeholder_spec`], when the
/// strategy sends the tools.
pub(crate) fn apply_tool_choice_none(
    messages: Vec<Message>,
    defs: &[ToolDef],
    ruleset: Ruleset,
    typed_enabled: bool,
    family: ModelFamily,
) -> Result<NoneOutcome, ModelError> {
    if !has_tool_round(&messages) {
        return Ok(unchanged(messages));
    }
    match family.tool_choice_none_strategy() {
        ToolChoiceNoneStrategy::AnthropicNoneField => {
            let tools = partition_tools(defs, ruleset, typed_enabled)?;
            let mut specs = tools.generic.clone();
            let mut seen: HashSet<String> = specs.iter().map(|s| s.name().to_owned()).collect();
            seen.extend(tools.typed.iter().map(|t| t.name.clone()));
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
            let mut additional = AdditionalFields::default();
            if tools.has_typed() {
                if specs.is_empty() {
                    specs.push(placeholder_spec()?);
                }
                additional.tools = tools.typed_entries();
            } else if specs.is_empty() {
                return Ok(unchanged(messages));
            }
            additional.tool_choice = Some(AdditionalToolChoice::None);
            Ok(NoneOutcome {
                messages,
                tools: specs.into_iter().map(Tool::ToolSpec).collect(),
                additional,
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
    use crate::translate::schema::Ruleset;
    use paigasus_helikon_core::{ContentPart, Item, ProviderToolType, ToolDef};

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
        apply_tool_choice_none(msgs, tools, Ruleset::Strict, false, family)
    }

    fn none_set(out: &NoneOutcome) -> bool {
        out.additional.tool_choice == Some(AdditionalToolChoice::None)
    }

    fn memory_def() -> ToolDef {
        ToolDef::new("memory", "d", json!({"type": "object"}))
            .with_provider_types(vec![ProviderToolType::new("anthropic", "memory_20250818")])
    }

    fn apply_typed(msgs: Vec<Message>, tools: &[ToolDef]) -> NoneOutcome {
        apply_tool_choice_none(msgs, tools, Ruleset::Strict, true, ModelFamily::Anthropic).unwrap()
    }

    fn memory_entry() -> serde_json::Value {
        json!({"type": "memory_20250818", "name": "memory"})
    }

    #[test]
    fn claude_keeps_user_tools_and_sets_flag() {
        let msgs = history("get_balance", json!({}), "ok");
        let out = apply(msgs.clone(), &[def("get_balance")], ModelFamily::Anthropic).unwrap();
        assert!(none_set(&out));
        assert_eq!(names(&out.tools), ["get_balance"]);
        assert_eq!(out.messages, msgs);
    }

    #[test]
    fn claude_derives_spec_from_history() {
        let msgs = history("get_balance", json!({}), "ok");
        let out = apply(msgs, &[], ModelFamily::Anthropic).unwrap();
        assert!(none_set(&out));
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
        assert!(none_set(&out));
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
        assert!(!none_set(&out));
        assert!(out.tools.is_empty());
        assert_eq!(out.messages, msgs);
    }

    #[test]
    fn typed_only_memory_history_gives_placeholder_and_none() {
        let msgs = history("memory", json!({"command": "view"}), "ok");
        let out = apply_typed(msgs, &[memory_def()]);
        assert_eq!(names(&out.tools), ["__paigasus_structured_output__"]);
        assert_eq!(out.additional.tools, vec![memory_entry()]);
        assert_eq!(out.additional.tool_choice, Some(AdditionalToolChoice::None));
    }

    #[test]
    fn typed_name_gets_no_derived_spec() {
        let msgs = history("memory", json!({"command": "view"}), "ok");
        let out = apply_typed(msgs, &[memory_def(), def("get_balance")]);
        assert_eq!(names(&out.tools), ["get_balance"]);
        assert_eq!(out.additional.tools, vec![memory_entry()]);
        assert!(none_set(&out));
    }

    #[test]
    fn typed_tools_without_tool_round_send_nothing() {
        let msgs = items_to_messages(&[user_text("hi")]).unwrap().messages;
        let out = apply_typed(msgs, &[memory_def()]);
        assert!(out.tools.is_empty());
        assert_eq!(out.additional, AdditionalFields::default());
    }

    #[test]
    fn text_history_family_ignores_typed_set() {
        let msgs = history("get_balance", json!({}), "ok");
        let out = apply_tool_choice_none(
            msgs,
            &[memory_def()],
            Ruleset::Strict,
            false,
            ModelFamily::AmazonNova,
        )
        .unwrap();
        assert!(!none_set(&out));
        assert!(out.tools.is_empty());
        assert_eq!(out.additional, AdditionalFields::default());
        assert!(!has_tool_round(&out.messages));
    }

    #[test]
    fn text_history_replaces_tool_blocks() {
        for family in [ModelFamily::AmazonNova, ModelFamily::Llama] {
            let msgs = history("get_balance", json!({"account_id": "acc_123"}), "$1,234.56");
            let out = apply(msgs, &[def("get_balance")], family).unwrap();
            assert!(!none_set(&out));
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
            assert!(!none_set(&out));
            assert!(out.tools.is_empty());
            assert_eq!(out.messages, msgs);
        }
    }
}
