//! Translate core `Item`s into Gemini `contents` + `systemInstruction`.

use paigasus_helikon_core::{ContentPart, Item, MediaSource, ModelError};
use serde_json::{json, Value};
use std::collections::HashSet;

/// `contents` + optional `systemInstruction`.
pub(crate) struct TranslatedContents {
    pub(crate) system: Option<Value>,
    pub(crate) contents: Vec<Value>,
}

/// Tracks tool calls that have no result yet, so each result takes its name
/// from the earliest open call with the same id (FIFO). Gemini ids are only
/// unique within one response, so ids can repeat across turns.
struct OpenCalls<'a> {
    /// `(call_id, name)` in open order.
    open: Vec<(&'a str, &'a str)>,
    /// Ids opened so far.
    seen: HashSet<&'a str>,
    /// Ids of every call in the conversation (error-message choice only).
    all: HashSet<&'a str>,
}

impl<'a> OpenCalls<'a> {
    fn new(items: &'a [Item]) -> Self {
        let mut all = HashSet::new();
        for it in items {
            match it {
                Item::ToolCall { call_id, .. } => {
                    all.insert(call_id.as_str());
                }
                Item::AssistantMessage { content, .. } => {
                    for p in content {
                        if let ContentPart::ToolUse { call_id, .. } = p {
                            all.insert(call_id.as_str());
                        }
                    }
                }
                _ => {}
            }
        }
        Self {
            open: Vec::new(),
            seen: HashSet::new(),
            all,
        }
    }

    fn open(&mut self, call_id: &'a str, name: &'a str) {
        self.seen.insert(call_id);
        self.open.push((call_id, name));
    }

    /// `Ok(Some(name))` closes the earliest open call with this id. `Ok(None)`
    /// means a duplicate result: the caller drops it.
    fn close(&mut self, call_id: &str) -> Result<Option<&'a str>, ModelError> {
        if let Some(pos) = self.open.iter().position(|(id, _)| *id == call_id) {
            return Ok(Some(self.open.remove(pos).1));
        }
        if self.seen.contains(call_id) {
            tracing::warn!(
                target: "paigasus::gemini::translate",
                call_id,
                "tool result closes no open call; dropping"
            );
            return Ok(None);
        }
        if self.all.contains(call_id) {
            Err(ModelError::Other(anyhow::anyhow!(
                "tool result for call_id {call_id} comes before its call"
            )))
        } else {
            Err(ModelError::Other(anyhow::anyhow!(
                "tool result references unknown call_id {call_id}"
            )))
        }
    }
}

/// Translate core items into Gemini `contents`. Returns an error on an empty
/// or system-only conversation (Gemini 400s on empty contents).
pub(crate) fn items_to_contents(items: &[Item]) -> Result<TranslatedContents, ModelError> {
    let mut calls = OpenCalls::new(items);
    let mut system_parts: Vec<Value> = Vec::new();
    let mut entries: Vec<(&'static str, Vec<Value>)> = Vec::new();

    for it in items {
        match it {
            Item::System { content } => {
                system_parts.extend(text_parts(content));
            }
            Item::UserMessage { content } => {
                entries.push(("user", user_parts(content, &mut calls)?));
            }
            Item::AssistantMessage { content, .. } => {
                for p in content {
                    if let ContentPart::ToolUse { call_id, name, .. } = p {
                        calls.open(call_id, name);
                    }
                }
                entries.push(("model", assistant_parts(content)));
            }
            Item::ToolCall {
                call_id,
                name,
                args,
            } => {
                calls.open(call_id, name);
                entries.push((
                    "model",
                    vec![json!({ "functionCall": { "id": call_id, "name": name, "args": args } })],
                ));
            }
            Item::ToolResult { call_id, content } => {
                if let Some(name) = calls.close(call_id)? {
                    entries.push((
                        "user",
                        vec![json!({
                            "functionResponse": {
                                "id": call_id,
                                "name": name,
                                "response": tool_response_object(content),
                            }
                        })],
                    ));
                }
            }
            _ => {}
        }
    }

    let contents = merge_same_role(entries);
    if contents.is_empty() {
        return Err(ModelError::Other(anyhow::anyhow!(
            "gemini request has no user/model turns (empty or system-only conversation)"
        )));
    }

    let system = (!system_parts.is_empty()).then(|| json!({ "parts": system_parts }));
    Ok(TranslatedContents { system, contents })
}

/// Merge adjacent entries of the same role into one content, in order.
///
/// Two `user` entries do not merge when exactly one side of the joint is a
/// `functionResponse`. Gemini 3 starts the current turn at the last `user`
/// content that is not a tool result, and it checks thought signatures only in
/// that turn. The SDK sends no signatures yet (SMA-827). A text and a
/// `functionResponse` in one content would keep the unsigned `functionCall` in
/// the current turn, and Gemini would return HTTP 400.
/// An entry with no parts is dropped first, so it never emits `"parts": []`.
fn merge_same_role(entries: Vec<(&'static str, Vec<Value>)>) -> Vec<Value> {
    let is_response = |p: Option<&Value>| p.is_some_and(|p| p.get("functionResponse").is_some());
    let mut merged: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for (role, parts) in entries {
        if parts.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some((last_role, last_parts))
                if *last_role == role
                    && !(role == "user"
                        && is_response(last_parts.last()) != is_response(parts.first())) =>
            {
                last_parts.extend(parts)
            }
            _ => merged.push((role, parts)),
        }
    }
    merged
        .into_iter()
        .map(|(role, parts)| json!({ "role": role, "parts": parts }))
        .collect()
}

fn text_parts(content: &[ContentPart]) -> Vec<Value> {
    content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(json!({ "text": text })),
            _ => None,
        })
        .collect()
}

/// Translate the parts of an `Item::UserMessage`.
///
/// Handles the Anthropic-native [`ContentPart::ToolResult`] shape (a tool
/// result carried inside a user turn) by emitting a `functionResponse` part,
/// recovering the function name from the earliest open call with that id
/// exactly like the top-level [`Item::ToolResult`] branch. A duplicate result
/// (no open call left) is dropped with a warning. Text and base64 images translate as usual;
/// URL images and other parts are dropped with a discriminant warning.
fn user_parts<'a>(
    content: &'a [ContentPart],
    calls: &mut OpenCalls<'a>,
) -> Result<Vec<Value>, ModelError> {
    let mut out = Vec::new();
    for p in content {
        match p {
            // The translator drops empty text, as SMA-804 spec section 5.3
            // asks every translator to do.
            ContentPart::Text { text } if text.is_empty() => {}
            ContentPart::Text { text } => out.push(json!({ "text": text })),
            ContentPart::Image {
                source: MediaSource::Base64 { mime_type, data },
            } => {
                out.push(json!({ "inlineData": { "mimeType": mime_type, "data": data } }));
            }
            ContentPart::ToolResult { call_id, content } => {
                if let Some(name) = calls.close(call_id)? {
                    out.push(json!({
                        "functionResponse": {
                            "id": call_id,
                            "name": name,
                            "response": tool_response_object(content),
                        }
                    }));
                }
            }
            other => {
                tracing::warn!(
                    target: "paigasus::gemini::translate",
                    part = ?std::mem::discriminant(other),
                    "unsupported content part; skipping"
                );
            }
        }
    }
    Ok(out)
}

fn assistant_parts(content: &[ContentPart]) -> Vec<Value> {
    let mut out = Vec::new();
    for p in content {
        match p {
            ContentPart::Text { text } if text.is_empty() => {}
            ContentPart::Text { text } => out.push(json!({ "text": text })),
            ContentPart::ToolUse {
                call_id,
                name,
                args,
            } => {
                out.push(json!({ "functionCall": { "id": call_id, "name": name, "args": args } }));
            }
            ContentPart::Reasoning { .. } => { /* deferred (D3) */ }
            other => {
                tracing::warn!(
                    target: "paigasus::gemini::translate",
                    part = ?std::mem::discriminant(other),
                    "unsupported assistant part; skipping"
                );
            }
        }
    }
    out
}

/// Reduce a tool result's content parts to a JSON object for `functionResponse.response`.
fn tool_response_object(content: &[ContentPart]) -> Value {
    let text: String = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => Value::Object(m),
        Ok(other) => json!({ "result": other }),
        Err(_) => json!({ "result": text }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(s: &str) -> Item {
        Item::UserMessage {
            content: vec![ContentPart::Text { text: s.into() }],
        }
    }

    #[test]
    fn system_goes_to_system_instruction_not_contents() {
        let items = vec![
            Item::System {
                content: vec![ContentPart::Text {
                    text: "be terse".into(),
                }],
            },
            user("hi"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(t.system.unwrap()["parts"][0]["text"], "be terse");
        assert_eq!(t.contents.len(), 1);
        assert_eq!(t.contents[0]["role"], "user");
        assert_eq!(t.contents[0]["parts"][0]["text"], "hi");
    }

    #[test]
    fn empty_and_system_only_error() {
        assert!(items_to_contents(&[]).is_err());
        let sys = vec![Item::System {
            content: vec![ContentPart::Text { text: "x".into() }],
        }];
        assert!(items_to_contents(&sys).is_err());
    }

    #[test]
    fn assistant_becomes_model_role() {
        let items = vec![
            user("hi"),
            Item::AssistantMessage {
                content: vec![ContentPart::Text { text: "yo".into() }],
                agent: None,
            },
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(t.contents[1]["role"], "model");
    }

    #[test]
    fn tool_call_and_result_roundtrip_id_and_name() {
        let items = vec![
            user("search cats"),
            Item::ToolCall {
                call_id: "fc_0".into(),
                name: "search".into(),
                args: json!({"q":"cats"}),
            },
            Item::ToolResult {
                call_id: "fc_0".into(),
                content: vec![ContentPart::Text {
                    text: "{\"hits\":3}".into(),
                }],
            },
        ];
        let t = items_to_contents(&items).unwrap();
        let call = &t.contents[1];
        assert_eq!(call["role"], "model");
        assert_eq!(call["parts"][0]["functionCall"]["name"], "search");
        assert_eq!(call["parts"][0]["functionCall"]["id"], "fc_0");
        let result = &t.contents[2];
        assert_eq!(result["role"], "user");
        let fr = &result["parts"][0]["functionResponse"];
        assert_eq!(fr["name"], "search"); // recovered from call_id->name map
        assert_eq!(fr["id"], "fc_0");
        assert_eq!(fr["response"]["hits"], 3); // parsed JSON object
    }

    /// SMA-804: a checkpoint `Inject` leaves a tool result directly followed
    /// by a user message. SMA-820: the tool-result boundary keeps the text in its
    /// own `user` content, so the `functionResponse` content stays apart.
    #[test]
    fn tool_result_then_user_message_request_shape() {
        let items = vec![
            user("start"),
            Item::AssistantMessage {
                content: vec![ContentPart::Text {
                    text: "calling".into(),
                }],
                agent: None,
            },
            Item::ToolCall {
                call_id: "fc_0".into(),
                name: "search".into(),
                args: json!({"q":"cats"}),
            },
            Item::ToolResult {
                call_id: "fc_0".into(),
                content: vec![ContentPart::Text {
                    text: "{\"hits\":3}".into(),
                }],
            },
            user("news"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "start"}]},
                {"role": "model", "parts": [{"text": "calling"}, {"functionCall": {"id": "fc_0", "name": "search", "args": {"q": "cats"}}}]},
                {"role": "user", "parts": [{"functionResponse": {"id": "fc_0", "name": "search", "response": {"hits": 3}}}]},
                {"role": "user", "parts": [{"text": "news"}]},
            ])
        );
    }

    #[test]
    fn non_object_tool_result_wrapped_in_result_key() {
        let items = vec![
            user("x"),
            Item::ToolCall {
                call_id: "fc_0".into(),
                name: "echo".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "fc_0".into(),
                content: vec![ContentPart::Text {
                    text: "plain text".into(),
                }],
            },
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            t.contents[2]["parts"][0]["functionResponse"]["response"]["result"],
            "plain text"
        );
    }

    #[test]
    fn tool_result_without_matching_call_errors() {
        let items = vec![
            user("x"),
            Item::ToolResult {
                call_id: "ghost".into(),
                content: vec![ContentPart::Text { text: "{}".into() }],
            },
        ];
        let err = items_to_contents(&items).err().expect("must error");
        assert!(err.to_string().contains("unknown call_id ghost"), "{err}");
    }

    #[test]
    fn inline_base64_image_becomes_inline_data() {
        let items = vec![Item::UserMessage {
            content: vec![ContentPart::Image {
                source: MediaSource::Base64 {
                    mime_type: "image/png".into(),
                    data: "AAAA".into(),
                },
            }],
        }];
        let t = items_to_contents(&items).unwrap();
        let part = &t.contents[0]["parts"][0]["inlineData"];
        assert_eq!(part["mimeType"], "image/png");
        assert_eq!(part["data"], "AAAA");
    }

    #[test]
    fn nested_tool_result_in_user_message_becomes_function_response() {
        // Anthropic-native shape: a ToolResult carried inside a UserMessage,
        // with a prior ToolCall so the name resolves.
        let items = vec![
            user("search cats"),
            Item::ToolCall {
                call_id: "fc_0".into(),
                name: "search".into(),
                args: json!({"q":"cats"}),
            },
            Item::UserMessage {
                content: vec![ContentPart::ToolResult {
                    call_id: "fc_0".into(),
                    content: vec![ContentPart::Text {
                        text: "{\"hits\":3}".into(),
                    }],
                }],
            },
        ];
        let t = items_to_contents(&items).unwrap();
        let fr = &t.contents[2]["parts"][0]["functionResponse"];
        assert_eq!(fr["id"], "fc_0");
        assert_eq!(fr["name"], "search"); // recovered from call_id->name map
        assert_eq!(fr["response"]["hits"], 3);
    }

    #[test]
    fn user_message_of_only_tool_result_is_non_empty_user_turn() {
        let items = vec![
            Item::ToolCall {
                call_id: "fc_0".into(),
                name: "search".into(),
                args: json!({}),
            },
            Item::UserMessage {
                content: vec![ContentPart::ToolResult {
                    call_id: "fc_0".into(),
                    content: vec![ContentPart::Text { text: "{}".into() }],
                }],
            },
        ];
        let t = items_to_contents(&items).unwrap();
        // The user turn must NOT degrade to "parts": []; it has one functionResponse.
        let user_turn = &t.contents[1];
        assert_eq!(user_turn["role"], "user");
        let parts = user_turn["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert!(parts[0].get("functionResponse").is_some());
    }

    #[test]
    fn url_image_skipped() {
        let items = vec![Item::UserMessage {
            content: vec![
                ContentPart::Text { text: "see".into() },
                ContentPart::Image {
                    source: MediaSource::Url {
                        url: "http://x/y.png".into(),
                    },
                },
            ],
        }];
        let t = items_to_contents(&items).unwrap();
        // Only the text part survives.
        assert_eq!(t.contents[0]["parts"].as_array().unwrap().len(), 1);
        assert_eq!(t.contents[0]["parts"][0]["text"], "see");
    }

    fn call(id: &str, name: &str) -> Item {
        Item::ToolCall {
            call_id: id.into(),
            name: name.into(),
            args: json!({}),
        }
    }

    fn result(id: &str, json: &str) -> Item {
        Item::ToolResult {
            call_id: id.into(),
            content: vec![ContentPart::Text { text: json.into() }],
        }
    }

    fn assistant_text(s: &str) -> Item {
        Item::AssistantMessage {
            content: vec![ContentPart::Text { text: s.into() }],
            agent: None,
        }
    }

    /// `(id, name)` of every `functionResponse` part, in order.
    fn responses(t: &TranslatedContents) -> Vec<(String, String)> {
        t.contents
            .iter()
            .flat_map(|c| c["parts"].as_array().unwrap().iter())
            .filter_map(|p| p.get("functionResponse"))
            .map(|fr| {
                (
                    fr["id"].as_str().unwrap().to_string(),
                    fr["name"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn cross_turn_repeated_id_names_each_result_from_its_own_call() {
        let items = vec![
            user("a"),
            call("fc_0", "search"),
            result("fc_0", "{}"),
            assistant_text("ok"),
            user("b"),
            call("fc_0", "pay"),
            result("fc_0", "{}"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(responses(&t), pairs(&[("fc_0", "search"), ("fc_0", "pay")]));
    }

    #[test]
    fn same_round_repeated_id_tool_call_items_pair_fifo() {
        let items = vec![
            user("a"),
            call("fc_0", "search"),
            call("fc_0", "pay"),
            result("fc_0", "{}"),
            result("fc_0", "{}"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(responses(&t), pairs(&[("fc_0", "search"), ("fc_0", "pay")]));
    }

    #[test]
    fn same_round_repeated_id_tool_use_parts_pair_fifo() {
        let tool_use = |name: &str| ContentPart::ToolUse {
            call_id: "fc_0".into(),
            name: name.into(),
            args: json!({}),
        };
        let tool_result = || ContentPart::ToolResult {
            call_id: "fc_0".into(),
            content: vec![ContentPart::Text { text: "{}".into() }],
        };
        let items = vec![
            user("a"),
            Item::AssistantMessage {
                content: vec![tool_use("search"), tool_use("pay")],
                agent: None,
            },
            Item::UserMessage {
                content: vec![tool_result(), tool_result()],
            },
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(responses(&t), pairs(&[("fc_0", "search"), ("fc_0", "pay")]));
    }

    #[test]
    fn results_in_different_order_than_calls_keep_their_names() {
        let items = vec![
            user("a"),
            call("fc_0", "search"),
            call("fc_1", "pay"),
            result("fc_1", "{}"),
            result("fc_0", "{}"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(responses(&t), pairs(&[("fc_1", "pay"), ("fc_0", "search")]));
    }

    #[test]
    fn duplicate_result_for_one_call_is_dropped() {
        let items = vec![
            user("a"),
            call("fc_0", "search"),
            result("fc_0", "{\"n\":1}"),
            result("fc_0", "{\"n\":2}"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(responses(&t), pairs(&[("fc_0", "search")]));
        let kept = t
            .contents
            .iter()
            .flat_map(|c| c["parts"].as_array().unwrap().iter())
            .find_map(|p| p.get("functionResponse"))
            .unwrap();
        assert_eq!(kept["response"]["n"], 1);
    }

    #[test]
    fn duplicate_result_as_only_part_leaves_no_empty_content() {
        let items = vec![
            user("a"),
            call("fc_0", "search"),
            result("fc_0", "{}"),
            Item::UserMessage {
                content: vec![ContentPart::ToolResult {
                    call_id: "fc_0".into(),
                    content: vec![ContentPart::Text { text: "{}".into() }],
                }],
            },
            user("b"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(responses(&t), pairs(&[("fc_0", "search")]));
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "a"}]},
                {"role": "model", "parts": [{"functionCall": {"id": "fc_0", "name": "search", "args": {}}}]},
                {"role": "user", "parts": [{"functionResponse": {"id": "fc_0", "name": "search", "response": {}}}]},
                {"role": "user", "parts": [{"text": "b"}]},
            ])
        );
    }

    fn fr(id: &str, name: &str, response: Value) -> Value {
        json!({"functionResponse": {"id": id, "name": name, "response": response}})
    }

    fn fc(id: &str, name: &str) -> Value {
        json!({"functionCall": {"id": id, "name": name, "args": {}}})
    }

    #[test]
    fn adjacent_user_texts_merge_but_not_into_a_tool_result() {
        let items = vec![
            user("start"),
            call("fc_0", "search"),
            result("fc_0", "{}"),
            user("one"),
            user("two"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "start"}]},
                {"role": "model", "parts": [fc("fc_0", "search")]},
                {"role": "user", "parts": [fr("fc_0", "search", json!({}))]},
                {"role": "user", "parts": [{"text": "one"}, {"text": "two"}]},
            ])
        );
    }

    #[test]
    fn user_text_before_a_tool_result_does_not_merge_into_it() {
        let items = vec![
            user("q"),
            call("fc_0", "search"),
            user("note"),
            result("fc_0", "{}"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "q"}]},
                {"role": "model", "parts": [fc("fc_0", "search")]},
                {"role": "user", "parts": [{"text": "note"}]},
                {"role": "user", "parts": [fr("fc_0", "search", json!({}))]},
            ])
        );
    }

    #[test]
    fn three_parallel_results_merge_into_one_user_content() {
        let items = vec![
            user("x"),
            call("fc_0", "search"),
            call("fc_1", "search"),
            call("fc_2", "search"),
            result("fc_0", "{}"),
            result("fc_1", "{}"),
            result("fc_2", "{}"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "x"}]},
                {"role": "model", "parts": [fc("fc_0", "search"), fc("fc_1", "search"), fc("fc_2", "search")]},
                {"role": "user", "parts": [
                    fr("fc_0", "search", json!({})),
                    fr("fc_1", "search", json!({})),
                    fr("fc_2", "search", json!({})),
                ]},
            ])
        );
    }

    #[test]
    fn tool_result_boundary_keeps_text_after_results_separate() {
        let items = vec![
            user("x"),
            call("fc_0", "search"),
            call("fc_1", "search"),
            result("fc_0", "{}"),
            result("fc_1", "{}"),
            user("a"),
            user("b"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "x"}]},
                {"role": "model", "parts": [fc("fc_0", "search"), fc("fc_1", "search")]},
                {"role": "user", "parts": [fr("fc_0", "search", json!({})), fr("fc_1", "search", json!({}))]},
                {"role": "user", "parts": [{"text": "a"}, {"text": "b"}]},
            ])
        );
    }

    #[test]
    fn empty_text_in_assistant_message_is_dropped() {
        let items = vec![
            user("a"),
            Item::AssistantMessage {
                content: vec![
                    ContentPart::Text {
                        text: String::new(),
                    },
                    ContentPart::Text { text: "t".into() },
                ],
                agent: None,
            },
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            t.contents[1],
            json!({"role": "model", "parts": [{"text": "t"}]})
        );
    }

    #[test]
    fn parallel_calls_merge_into_one_model_and_one_user_content() {
        let items = vec![
            user("x"),
            call("fc_0", "search"),
            call("fc_1", "search"),
            result("fc_0", "{\"n\":1}"),
            result("fc_1", "{\"n\":2}"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "x"}]},
                {"role": "model", "parts": [fc("fc_0", "search"), fc("fc_1", "search")]},
                {"role": "user", "parts": [
                    fr("fc_0", "search", json!({"n": 1})),
                    fr("fc_1", "search", json!({"n": 2}))
                ]},
            ])
        );
    }

    #[test]
    fn nested_result_and_text_merge_with_preceding_tool_result() {
        let items = vec![
            user("x"),
            call("fc_0", "search"),
            call("fc_1", "pay"),
            result("fc_0", "{}"),
            Item::UserMessage {
                content: vec![
                    ContentPart::ToolResult {
                        call_id: "fc_1".into(),
                        content: vec![ContentPart::Text { text: "{}".into() }],
                    },
                    ContentPart::Text {
                        text: "next".into(),
                    },
                ],
            },
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            t.contents.last().unwrap(),
            &json!({"role": "user", "parts": [
                fr("fc_0", "search", json!({})),
                fr("fc_1", "pay", json!({})),
                {"text": "next"}
            ]})
        );
    }

    #[test]
    fn system_between_user_messages_does_not_split_them() {
        let items = vec![
            user("a"),
            Item::System {
                content: vec![ContentPart::Text {
                    text: "be terse".into(),
                }],
            },
            user("b"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([{"role": "user", "parts": [{"text": "a"}, {"text": "b"}]}])
        );
        assert_eq!(t.system.unwrap()["parts"][0]["text"], "be terse");
    }

    #[test]
    fn reasoning_only_assistant_message_is_dropped_and_neighbours_merge() {
        let items = vec![
            user("a"),
            Item::AssistantMessage {
                content: vec![ContentPart::Reasoning { text: "r".into() }],
                agent: None,
            },
            user("b"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([{"role": "user", "parts": [{"text": "a"}, {"text": "b"}]}])
        );
    }

    #[test]
    fn assistant_text_tool_use_and_tool_call_form_one_model_content() {
        let items = vec![
            user("a"),
            Item::AssistantMessage {
                content: vec![
                    ContentPart::Text { text: "t".into() },
                    ContentPart::ToolUse {
                        call_id: "fc_0".into(),
                        name: "search".into(),
                        args: json!({}),
                    },
                ],
                agent: None,
            },
            call("fc_1", "pay"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(t.contents.len(), 2);
        assert_eq!(
            t.contents[1],
            json!({"role": "model", "parts": [{"text": "t"}, fc("fc_0", "search"), fc("fc_1", "pay")]})
        );
    }

    #[test]
    fn empty_text_part_is_dropped() {
        let items = vec![
            user("a"),
            call("fc_0", "search"),
            result("fc_0", "{}"),
            Item::UserMessage {
                content: vec![ContentPart::Text {
                    text: String::new(),
                }],
            },
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(
            t.contents.last().unwrap(),
            &json!({"role": "user", "parts": [fr("fc_0", "search", json!({}))]})
        );
    }

    #[test]
    fn only_empty_user_text_is_an_empty_conversation_error() {
        assert!(items_to_contents(&[user("")]).is_err());
    }

    #[test]
    fn result_before_its_call_errors() {
        let items = vec![user("a"), result("fc_0", "{}"), call("fc_0", "search")];
        let err = items_to_contents(&items).err().expect("must error");
        assert!(err.to_string().contains("comes before its call"), "{err}");
    }
}
