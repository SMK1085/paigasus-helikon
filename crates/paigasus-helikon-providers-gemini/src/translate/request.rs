//! Translate core `Item`s into Gemini `contents` + `systemInstruction`.

use crate::PROVIDER_DATA_KEY;
use paigasus_helikon_core::{ContentPart, Item, MediaSource, ModelError};
use serde_json::{json, Value};
use std::collections::HashSet;

/// The dummy thought signature for a `functionCall` part that has no valid
/// signature in the current turn.
///
/// Source: this is the dummy value that Google documents for a history with
/// no signature. The SMA-820 probe and the SMA-827 probe (capture 6) got
/// HTTP 200 with it on `gemini-3.8-flash` (Developer API). Google warns that
/// the dummy can make answers worse, so the translator uses it only as the
/// fallback.
pub(crate) const DUMMY_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

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

/// One wire part, with side data that never goes on the wire.
struct Part {
    json: Value,
    /// The position of a `functionCall` in the model response that the stream
    /// recorded. `None` for every other part, and for a call with no data.
    index: Option<u64>,
}

impl Part {
    fn plain(json: Value) -> Self {
        Self { json, index: None }
    }

    fn is_call(&self) -> bool {
        self.json.get("functionCall").is_some()
    }

    fn is_response(&self) -> bool {
        self.json.get("functionResponse").is_some()
    }
}

/// The Gemini provider data of the latest `AssistantMessage`, as
/// `(call_id, data)`. Each `functionCall` takes and removes the first entry
/// with its id.
struct Pending<'a> {
    entries: Vec<(&'a str, &'a Value)>,
    model_id: &'a str,
    api: &'a str,
}

impl<'a> Pending<'a> {
    /// Replace the list with the Gemini provider data of one message.
    fn load(&mut self, content: &'a [ContentPart]) {
        self.entries = content
            .iter()
            .filter_map(|p| match p {
                ContentPart::ProviderData {
                    provider,
                    call_id: Some(call_id),
                    data,
                } if provider == PROVIDER_DATA_KEY => Some((call_id.as_str(), data)),
                _ => None,
            })
            .collect();
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    /// Build the `functionCall` part for one call. A valid signature goes on
    /// the part. The stream index goes into the side data.
    fn function_call(&mut self, call_id: &str, name: &str, args: &Value) -> Part {
        let data = self
            .entries
            .iter()
            .position(|(id, _)| *id == call_id)
            .map(|pos| self.entries.remove(pos).1);
        let mut json = json!({ "functionCall": { "id": call_id, "name": name, "args": args } });
        if let Some(sig) = data.and_then(|d| self.valid_signature(d)) {
            json["thoughtSignature"] = Value::String(sig.to_owned());
        }
        Part {
            json,
            index: data.and_then(|d| d.get("index")).and_then(Value::as_u64),
        }
    }

    /// The signature in `data` when it is a string and `data` names this
    /// model and this API. Anything else is no signature, never an error.
    fn valid_signature<'d>(&self, data: &'d Value) -> Option<&'d str> {
        let sig = data.get("thoughtSignature")?.as_str()?;
        (data.get("model").and_then(Value::as_str) == Some(self.model_id)
            && data.get("api").and_then(Value::as_str) == Some(self.api))
        .then_some(sig)
    }
}

/// Translate core items into Gemini `contents`. Returns an error on an empty
/// or system-only conversation (Gemini 400s on empty contents).
///
/// `model_id` and `api` are the configured model and transport. A thought
/// signature goes back only when its provider data names both.
pub(crate) fn items_to_contents(
    items: &[Item],
    model_id: &str,
    api: &str,
) -> Result<TranslatedContents, ModelError> {
    let mut calls = OpenCalls::new(items);
    let mut pending = Pending {
        entries: Vec::new(),
        model_id,
        api,
    };
    let mut system_parts: Vec<Value> = Vec::new();
    let mut entries: Vec<(&'static str, Vec<Part>)> = Vec::new();

    for it in items {
        match it {
            Item::System { content } => {
                system_parts.extend(text_parts(content));
            }
            Item::UserMessage { content } => {
                // A tool result does not end the model response, so a message
                // of only tool results keeps the pending list.
                if content
                    .iter()
                    .any(|p| !matches!(p, ContentPart::ToolResult { .. }))
                {
                    pending.clear();
                }
                let parts = user_parts(content, &mut calls)?;
                entries.push(("user", parts.into_iter().map(Part::plain).collect()));
            }
            Item::AssistantMessage { content, .. } => {
                pending.load(content);
                for p in content {
                    if let ContentPart::ToolUse { call_id, name, .. } = p {
                        calls.open(call_id, name);
                    }
                }
                entries.push(("model", assistant_parts(content, &mut pending)));
            }
            Item::ToolCall {
                call_id,
                name,
                args,
            } => {
                calls.open(call_id, name);
                entries.push(("model", vec![pending.function_call(call_id, name, args)]));
            }
            Item::ToolResult { call_id, content } => {
                if let Some(name) = calls.close(call_id)? {
                    entries.push((
                        "user",
                        vec![Part::plain(json!({
                            "functionResponse": {
                                "id": call_id,
                                "name": name,
                                "response": tool_response_object(content),
                            }
                        }))],
                    ));
                }
            }
            _ => {}
        }
    }

    let mut merged = merge_same_role(entries);
    for (role, parts) in &mut merged {
        if *role == "model" {
            restore_call_order(parts);
        }
    }
    if uses_dummy_signature(model_id) {
        add_dummy_signatures(&mut merged);
    }
    let contents: Vec<Value> = merged
        .into_iter()
        .map(|(role, parts)| {
            let parts: Vec<Value> = parts.into_iter().map(|p| p.json).collect();
            json!({ "role": role, "parts": parts })
        })
        .collect();
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
/// Two `user` entries merge only when both hold only `functionResponse` parts
/// or both hold none. Gemini 3 starts the current turn at the last `user`
/// content that is not a tool result, and it checks thought signatures only in
/// that turn. SMA-820 added this boundary because a text and a
/// `functionResponse` in one content kept an unsigned `functionCall` in the
/// current turn, and Gemini returned HTTP 400. The SMA-827 probe (capture 8)
/// got HTTP 200 for the merged shape with the real signature, so the boundary
/// is not needed when signatures are present. It stays until Sven decides; see
/// the SMA-827 spec section "Left for Sven". A content that mixes results and
/// other parts merges with nothing, so a later text never joins a tool result.
/// An entry with no parts is dropped first, so it never emits `"parts": []`.
fn merge_same_role(entries: Vec<(&'static str, Vec<Part>)>) -> Vec<(&'static str, Vec<Part>)> {
    #[derive(PartialEq, Clone, Copy)]
    enum Class {
        AllResponses,
        NoResponses,
        Mixed,
    }
    let class = |parts: &[Part]| {
        let n = parts.iter().filter(|p| p.is_response()).count();
        if n == parts.len() {
            Class::AllResponses
        } else if n == 0 {
            Class::NoResponses
        } else {
            Class::Mixed
        }
    };
    // Each merged entry keeps its class, so a merge never rescans the parts.
    let mut merged: Vec<(&'static str, Class, Vec<Part>)> = Vec::new();
    for (role, parts) in entries {
        if parts.is_empty() {
            continue;
        }
        let incoming = class(&parts);
        match merged.last_mut() {
            // A merge needs equal classes that are not `Mixed`, so the stored
            // class stays correct after it.
            Some((last_role, last_class, last_parts))
                if *last_role == role
                    && (role != "user"
                        || (*last_class != Class::Mixed && *last_class == incoming)) =>
            {
                last_parts.extend(parts)
            }
            _ => merged.push((role, incoming, parts)),
        }
    }
    merged
        .into_iter()
        .map(|(role, _, parts)| (role, parts))
        .collect()
}

/// Sort the `functionCall` parts of one `model` content by their stream index,
/// when every call has one. The sort is stable, and the other parts keep their
/// positions. This restores the order that Gemini sent.
fn restore_call_order(parts: &mut [Part]) {
    let slots: Vec<usize> = (0..parts.len()).filter(|&i| parts[i].is_call()).collect();
    if slots.is_empty() || slots.iter().any(|&i| parts[i].index.is_none()) {
        return;
    }
    let mut calls: Vec<Part> = slots
        .iter()
        .map(|&i| std::mem::replace(&mut parts[i], Part::plain(Value::Null)))
        .collect();
    calls.sort_by_key(|p| p.index);
    for (slot, part) in slots.into_iter().zip(calls) {
        parts[slot] = part;
    }
}

/// Say whether a request for `model_id` may carry [`DUMMY_THOUGHT_SIGNATURE`].
///
/// The dummy is live-tested only on Gemini 3. Requests for a Gemini 1.x or
/// 2.x id stay as they were before SMA-827 and get no dummy. Every other id
/// (Gemini 3 and later, unknown ids) gets it. The check uses the part of the id
/// after the last `/`, so `models/gemini-2.5-flash` and Vertex resource paths work.
fn uses_dummy_signature(model_id: &str) -> bool {
    let name = model_id.rsplit('/').next().unwrap_or(model_id);
    !(name.starts_with("gemini-1") || name.starts_with("gemini-2"))
}

/// Put [`DUMMY_THOUGHT_SIGNATURE`] on each `functionCall` part of a `model`
/// content in the current turn when none of its calls has a signature.
///
/// The current turn is every content after the last `user` content with no
/// `functionResponse` part (all contents when there is no such content).
/// Gemini checks signatures only in that turn. A content with at least one
/// signature is sent as received.
fn add_dummy_signatures(merged: &mut [(&'static str, Vec<Part>)]) {
    let start = merged
        .iter()
        .rposition(|(role, parts)| *role == "user" && !parts.iter().any(Part::is_response))
        .map_or(0, |i| i + 1);
    for (role, parts) in &mut merged[start..] {
        if *role != "model" {
            continue;
        }
        let has_call = parts.iter().any(Part::is_call);
        let has_signature = parts
            .iter()
            .any(|p| p.is_call() && p.json.get("thoughtSignature").is_some());
        if !has_call || has_signature {
            continue;
        }
        for p in parts.iter_mut().filter(|p| p.is_call()) {
            p.json["thoughtSignature"] = Value::String(DUMMY_THOUGHT_SIGNATURE.to_owned());
        }
    }
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
            // Provider data is valid only in an assistant message.
            ContentPart::ProviderData { .. } => {}
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

/// Translate the parts of an `Item::AssistantMessage`. A `ToolUse` part takes
/// its provider data from `pending`. A `ProviderData` part is never a wire
/// part: the caller read the Gemini ones into `pending`, and a foreign one is
/// ignored with no warning.
fn assistant_parts(content: &[ContentPart], pending: &mut Pending<'_>) -> Vec<Part> {
    let mut out = Vec::new();
    for p in content {
        match p {
            ContentPart::Text { text } if text.is_empty() => {}
            ContentPart::Text { text } => out.push(Part::plain(json!({ "text": text }))),
            ContentPart::ToolUse {
                call_id,
                name,
                args,
            } => {
                out.push(pending.function_call(call_id, name, args));
            }
            ContentPart::ProviderData { .. } => {}
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

    const M: &str = "gemini-3.8-flash";
    const A: &str = "developer";

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
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(t.system.unwrap()["parts"][0]["text"], "be terse");
        assert_eq!(t.contents.len(), 1);
        assert_eq!(t.contents[0]["role"], "user");
        assert_eq!(t.contents[0]["parts"][0]["text"], "hi");
    }

    #[test]
    fn empty_and_system_only_error() {
        assert!(items_to_contents(&[], M, A).is_err());
        let sys = vec![Item::System {
            content: vec![ContentPart::Text { text: "x".into() }],
        }];
        assert!(items_to_contents(&sys, M, A).is_err());
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let err = items_to_contents(&items, M, A).err().expect("must error");
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "x"}]},
                {"role": "model", "parts": [fc_dummy("fc_0", "search"), fc_dummy("fc_1", "search"), fc_dummy("fc_2", "search")]},
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "x"}]},
                {"role": "model", "parts": [fc_dummy("fc_0", "search"), fc_dummy("fc_1", "search")]},
                {"role": "user", "parts": [
                    fr("fc_0", "search", json!({"n": 1})),
                    fr("fc_1", "search", json!({"n": 2}))
                ]},
            ])
        );
    }

    #[test]
    fn mixed_nested_result_message_does_not_merge_with_a_preceding_tool_result() {
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
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "x"}]},
                {"role": "model", "parts": [fc_dummy("fc_0", "search"), fc_dummy("fc_1", "pay")]},
                {"role": "user", "parts": [fr("fc_0", "search", json!({}))]},
                {"role": "user", "parts": [fr("fc_1", "pay", json!({})), {"text": "next"}]},
            ])
        );
    }

    #[test]
    fn user_text_after_a_mixed_tool_result_message_stays_separate() {
        let items = vec![
            user("x"),
            call("fc_0", "search"),
            Item::UserMessage {
                content: vec![
                    ContentPart::ToolResult {
                        call_id: "fc_0".into(),
                        content: vec![ContentPart::Text { text: "{}".into() }],
                    },
                    ContentPart::Text { text: "a".into() },
                ],
            },
            user("b"),
        ];
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "x"}]},
                {"role": "model", "parts": [fc("fc_0", "search")]},
                {"role": "user", "parts": [fr("fc_0", "search", json!({})), {"text": "a"}]},
                {"role": "user", "parts": [{"text": "b"}]},
            ])
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
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
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(t.contents.len(), 2);
        assert_eq!(
            t.contents[1],
            json!({"role": "model", "parts": [{"text": "t"}, fc_dummy("fc_0", "search"), fc_dummy("fc_1", "pay")]})
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
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            t.contents.last().unwrap(),
            &json!({"role": "user", "parts": [fr("fc_0", "search", json!({}))]})
        );
    }

    #[test]
    fn only_empty_user_text_is_an_empty_conversation_error() {
        assert!(items_to_contents(&[user("")], M, A).is_err());
    }

    #[test]
    fn result_before_its_call_errors() {
        let items = vec![user("a"), result("fc_0", "{}"), call("fc_0", "search")];
        let err = items_to_contents(&items, M, A).err().expect("must error");
        assert!(err.to_string().contains("comes before its call"), "{err}");
    }

    // SMA-827: thought signatures.

    /// A Gemini `ProviderData` part as the stream writes it for a signed call.
    fn signed(call_id: &str, index: u64, sig: &str) -> ContentPart {
        pd(
            call_id,
            json!({"index": index, "thoughtSignature": sig, "model": M, "api": A}),
        )
    }

    /// A Gemini `ProviderData` part as the stream writes it for an unsigned call.
    fn unsigned(call_id: &str, index: u64) -> ContentPart {
        pd(call_id, json!({"index": index}))
    }

    fn pd(call_id: &str, data: Value) -> ContentPart {
        ContentPart::ProviderData {
            provider: PROVIDER_DATA_KEY.into(),
            call_id: Some(call_id.into()),
            data,
        }
    }

    fn assistant(content: Vec<ContentPart>) -> Item {
        Item::AssistantMessage {
            content,
            agent: None,
        }
    }

    fn fc_sig(id: &str, name: &str, sig: &str) -> Value {
        json!({"functionCall": {"id": id, "name": name, "args": {}}, "thoughtSignature": sig})
    }

    fn fc_dummy(id: &str, name: &str) -> Value {
        fc_sig(id, name, DUMMY_THOUGHT_SIGNATURE)
    }

    #[test]
    fn dummy_thought_signature_value() {
        assert_eq!(DUMMY_THOUGHT_SIGNATURE, "skip_thought_signature_validator");
    }

    #[test]
    fn signature_goes_on_its_call() {
        let items = vec![
            user("q"),
            assistant(vec![signed("call_1", 0, "SIG_A")]),
            call("call_1", "search"),
            result("call_1", "{}"),
        ];
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "q"}]},
                {"role": "model", "parts": [fc_sig("call_1", "search", "SIG_A")]},
                {"role": "user", "parts": [fr("call_1", "search", json!({}))]},
            ])
        );
    }

    fn parallel_items() -> Vec<Item> {
        vec![
            user("q"),
            assistant(vec![
                signed("call_z", 0, "SIG_A"),
                unsigned("call_a", 1),
                unsigned("call_m", 2),
            ]),
            call("call_a", "search"),
            call("call_m", "search"),
            call("call_z", "search"),
            result("call_a", "{}"),
            result("call_m", "{}"),
            result("call_z", "{}"),
        ]
    }

    #[test]
    fn parallel_calls_restore_stream_order() {
        let t = items_to_contents(&parallel_items(), M, A).unwrap();
        assert_eq!(
            t.contents[1],
            json!({"role": "model", "parts": [
                fc_sig("call_z", "search", "SIG_A"),
                fc("call_a", "search"),
                fc("call_m", "search"),
            ]})
        );
        // The results keep the history order.
        assert_eq!(
            responses(&t),
            pairs(&[
                ("call_a", "search"),
                ("call_m", "search"),
                ("call_z", "search")
            ])
        );
    }

    #[test]
    fn signature_pairs_with_tool_use_in_the_same_message() {
        let tool_use = |id: &str| ContentPart::ToolUse {
            call_id: id.into(),
            name: "search".into(),
            args: json!({}),
        };
        let items = vec![
            user("q"),
            assistant(vec![
                tool_use("b"),
                tool_use("a"),
                signed("a", 0, "SIG_A"),
                unsigned("b", 1),
            ]),
            Item::UserMessage {
                content: vec![
                    ContentPart::ToolResult {
                        call_id: "a".into(),
                        content: vec![ContentPart::Text { text: "{}".into() }],
                    },
                    ContentPart::ToolResult {
                        call_id: "b".into(),
                        content: vec![ContentPart::Text { text: "{}".into() }],
                    },
                ],
            },
        ];
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            t.contents[1],
            json!({"role": "model", "parts": [
                fc_sig("a", "search", "SIG_A"),
                fc("b", "search"),
            ]})
        );
    }

    #[test]
    fn repeated_call_id_keeps_signature_per_round() {
        let items = vec![
            user("q"),
            assistant(vec![signed("fc_0", 0, "SIG_1")]),
            call("fc_0", "search"),
            result("fc_0", "{}"),
            assistant(vec![signed("fc_0", 0, "SIG_2")]),
            call("fc_0", "pay"),
            result("fc_0", "{}"),
        ];
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "q"}]},
                {"role": "model", "parts": [fc_sig("fc_0", "search", "SIG_1")]},
                {"role": "user", "parts": [fr("fc_0", "search", json!({}))]},
                {"role": "model", "parts": [fc_sig("fc_0", "pay", "SIG_2")]},
                {"role": "user", "parts": [fr("fc_0", "pay", json!({}))]},
            ])
        );
    }

    #[test]
    fn tool_result_between_calls_keeps_pending() {
        let items = vec![
            user("q"),
            assistant(vec![signed("a", 0, "SIG_A"), signed("b", 1, "SIG_B")]),
            call("a", "search"),
            result("a", "{}"),
            call("b", "pay"),
        ];
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "q"}]},
                {"role": "model", "parts": [fc_sig("a", "search", "SIG_A")]},
                {"role": "user", "parts": [fr("a", "search", json!({}))]},
                {"role": "model", "parts": [fc_sig("b", "pay", "SIG_B")]},
            ])
        );
    }

    #[test]
    fn unsigned_history_gets_dummy_in_current_turn_only() {
        let items = vec![
            user("first"),
            call("fc_0", "search"),
            call("fc_1", "search"),
            result("fc_0", "{}"),
            result("fc_1", "{}"),
            assistant_text("done"),
            user("second"),
            call("fc_0", "pay"),
            call("fc_1", "pay"),
            result("fc_0", "{}"),
            result("fc_1", "{}"),
        ];
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "first"}]},
                {"role": "model", "parts": [fc("fc_0", "search"), fc("fc_1", "search")]},
                {"role": "user", "parts": [fr("fc_0", "search", json!({})), fr("fc_1", "search", json!({}))]},
                {"role": "model", "parts": [{"text": "done"}]},
                {"role": "user", "parts": [{"text": "second"}]},
                {"role": "model", "parts": [fc_dummy("fc_0", "pay"), fc_dummy("fc_1", "pay")]},
                {"role": "user", "parts": [fr("fc_0", "pay", json!({})), fr("fc_1", "pay", json!({}))]},
            ])
        );
    }

    #[test]
    fn gemini_2_history_gets_no_dummy() {
        for model in [
            "gemini-2.5-flash",
            "models/gemini-2.0-flash",
            "projects/p/locations/l/publishers/google/models/gemini-2.5-pro",
            "gemini-1.5-pro",
        ] {
            let items = vec![user("q"), call("call_1", "search")];
            let t = items_to_contents(&items, model, A).unwrap();
            assert_eq!(
                t.contents[1],
                json!({"role": "model", "parts": [fc("call_1", "search")]}),
                "{model}"
            );
        }
    }

    #[test]
    fn gemini_3_and_unknown_ids_keep_the_dummy() {
        for model in ["models/gemini-3.8-flash", "gemini-4-pro", "my-tuned-model"] {
            let items = vec![user("q"), call("call_1", "search")];
            let t = items_to_contents(&items, model, A).unwrap();
            assert_eq!(
                t.contents[1],
                json!({"role": "model", "parts": [fc_dummy("call_1", "search")]}),
                "{model}"
            );
        }
    }

    #[test]
    fn gemini_2_real_signature_is_still_sent_back() {
        let model = "gemini-2.5-flash";
        let items = vec![
            user("q"),
            assistant(vec![pd(
                "call_1",
                json!({"index": 0, "thoughtSignature": "SIG_A", "model": model, "api": A}),
            )]),
            call("call_1", "search"),
        ];
        let t = items_to_contents(&items, model, A).unwrap();
        assert_eq!(
            t.contents[1],
            json!({"role": "model", "parts": [fc_sig("call_1", "search", "SIG_A")]})
        );
    }

    #[test]
    fn signature_from_other_model_or_api_gets_dummy() {
        for data in [
            json!({"index": 0, "thoughtSignature": "SIG_A", "model": "gemini-3.6-flash", "api": A}),
            json!({"index": 0, "thoughtSignature": "SIG_A", "model": M, "api": "vertex"}),
        ] {
            let items = vec![
                user("q"),
                assistant(vec![pd("call_1", data.clone())]),
                call("call_1", "search"),
            ];
            let t = items_to_contents(&items, M, A).unwrap();
            assert_eq!(
                t.contents[1],
                json!({"role": "model", "parts": [fc_dummy("call_1", "search")]}),
                "{data}"
            );
        }
    }

    #[test]
    fn malformed_signature_is_absent() {
        let items = vec![
            user("q"),
            assistant(vec![pd(
                "call_1",
                json!({"index": 0, "thoughtSignature": 5, "model": M, "api": A}),
            )]),
            call("call_1", "search"),
        ];
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            t.contents[1],
            json!({"role": "model", "parts": [fc_dummy("call_1", "search")]})
        );
    }

    #[test]
    fn mixed_content_is_sent_as_received() {
        let items = vec![
            user("q"),
            assistant(vec![signed("call_1", 0, "SIG_A"), unsigned("call_2", 1)]),
            call("call_1", "search"),
            call("call_2", "search"),
        ];
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            t.contents[1],
            json!({"role": "model", "parts": [
                fc_sig("call_1", "search", "SIG_A"),
                fc("call_2", "search"),
            ]})
        );
    }

    #[test]
    fn foreign_provider_data_is_ignored_silently() {
        let items = vec![
            user("q"),
            assistant(vec![
                ContentPart::ProviderData {
                    provider: "anthropic".into(),
                    call_id: Some("call_1".into()),
                    data: json!({"index": 0, "thoughtSignature": "SIG_A", "model": M, "api": A}),
                },
                ContentPart::Text { text: "t".into() },
            ]),
            call("call_1", "search"),
        ];
        let t = items_to_contents(&items, M, A).unwrap();
        // The foreign part is not a wire part and gives no signature.
        assert_eq!(
            Value::Array(t.contents),
            json!([
                {"role": "user", "parts": [{"text": "q"}]},
                {"role": "model", "parts": [{"text": "t"}, fc_dummy("call_1", "search")]},
            ])
        );
    }

    #[test]
    fn provider_data_in_user_message_is_dropped() {
        let items = vec![Item::UserMessage {
            content: vec![
                ContentPart::Text { text: "q".into() },
                signed("call_1", 0, "SIG_A"),
            ],
        }];
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([{"role": "user", "parts": [{"text": "q"}]}])
        );
    }

    #[test]
    fn provider_data_only_message_emits_no_content() {
        let items = vec![
            user("a"),
            assistant(vec![signed("call_1", 0, "SIG_A")]),
            user("b"),
        ];
        let t = items_to_contents(&items, M, A).unwrap();
        assert_eq!(
            Value::Array(t.contents),
            json!([{"role": "user", "parts": [{"text": "a"}, {"text": "b"}]}])
        );
    }

    #[test]
    fn no_side_data_reaches_the_wire() {
        let t = items_to_contents(&parallel_items(), M, A).unwrap();
        let body = serde_json::to_string(&Value::Array(t.contents)).unwrap();
        assert!(!body.contains("\"index\""), "{body}");
        assert!(!body.contains("provider"), "{body}");
    }
}
