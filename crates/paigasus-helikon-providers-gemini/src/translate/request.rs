//! Translate core `Item`s into Gemini `contents` + `systemInstruction`.

use paigasus_helikon_core::{ContentPart, Item, MediaSource, ModelError};
use serde_json::{json, Value};

/// `contents` + optional `systemInstruction`.
pub(crate) struct TranslatedContents {
    pub(crate) system: Option<Value>,
    pub(crate) contents: Vec<Value>,
}

/// Translate core items into Gemini `contents`. Returns an error on an empty
/// or system-only conversation (Gemini 400s on empty contents).
pub(crate) fn items_to_contents(items: &[Item]) -> Result<TranslatedContents, ModelError> {
    // Build call_id -> name map from all tool calls (ToolResult has no name).
    let mut call_names: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for it in items {
        match it {
            Item::ToolCall { call_id, name, .. } => {
                call_names.insert(call_id.as_str(), name.as_str());
            }
            Item::AssistantMessage { content, .. } => {
                for p in content {
                    if let ContentPart::ToolUse { call_id, name, .. } = p {
                        call_names.insert(call_id.as_str(), name.as_str());
                    }
                }
            }
            _ => {}
        }
    }

    let mut system_parts: Vec<Value> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();

    for it in items {
        match it {
            Item::System { content } => {
                system_parts.extend(text_parts(content));
            }
            Item::UserMessage { content } => {
                contents.push(json!({ "role": "user", "parts": content_parts(content) }));
            }
            Item::AssistantMessage { content, .. } => {
                contents.push(json!({ "role": "model", "parts": assistant_parts(content) }));
            }
            Item::ToolCall {
                call_id,
                name,
                args,
            } => {
                contents.push(json!({
                    "role": "model",
                    "parts": [ { "functionCall": { "id": call_id, "name": name, "args": args } } ]
                }));
            }
            Item::ToolResult { call_id, content } => {
                let name = call_names.get(call_id.as_str()).ok_or_else(|| {
                    ModelError::Other(anyhow::anyhow!(
                        "tool result references unknown call_id {call_id}"
                    ))
                })?;
                contents.push(json!({
                    "role": "user",
                    "parts": [ {
                        "functionResponse": {
                            "id": call_id,
                            "name": name,
                            "response": tool_response_object(content),
                        }
                    } ]
                }));
            }
            _ => {}
        }
    }

    if contents.is_empty() {
        return Err(ModelError::Other(anyhow::anyhow!(
            "gemini request has no user/model turns (empty or system-only conversation)"
        )));
    }

    let system = (!system_parts.is_empty()).then(|| json!({ "parts": system_parts }));
    Ok(TranslatedContents { system, contents })
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

fn content_parts(content: &[ContentPart]) -> Vec<Value> {
    let mut out = Vec::new();
    for p in content {
        match p {
            ContentPart::Text { text } => out.push(json!({ "text": text })),
            ContentPart::Image {
                source: MediaSource::Base64 { mime_type, data },
            } => {
                out.push(json!({ "inlineData": { "mimeType": mime_type, "data": data } }));
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
    out
}

fn assistant_parts(content: &[ContentPart]) -> Vec<Value> {
    let mut out = Vec::new();
    for p in content {
        match p {
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
        assert!(items_to_contents(&items).is_err());
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
}
