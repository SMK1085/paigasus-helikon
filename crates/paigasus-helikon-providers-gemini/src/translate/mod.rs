//! Translation between Paigasus carrier types and Gemini wire format.

pub(crate) mod request;
pub(crate) mod response_format;
pub(crate) mod schema;
pub(crate) mod tools;

use paigasus_helikon_core::{ModelError, ModelRequest};
use serde_json::{Map, Value};

use crate::builder::Config;
use request::items_to_contents;
use response_format::{response_format_fields, validate_conflict};
use tools::{function_calling_config, function_declarations};

/// Fully-assembled Gemini request body.
#[derive(Debug)]
pub(crate) struct PreparedRequest {
    pub(crate) body: Value,
}

/// Assemble the Gemini `generateContent` request body, running all guards.
pub(crate) fn build_request(
    cfg: &Config,
    req: &ModelRequest,
) -> Result<PreparedRequest, ModelError> {
    let s = &req.model_settings;

    validate_conflict(
        s.response_format.as_ref(),
        &req.tools,
        s.tool_choice.as_ref(),
    )?;

    // tool_choice Tool/Required require tools.
    if matches!(
        s.tool_choice,
        Some(paigasus_helikon_core::ToolChoice::Required)
            | Some(paigasus_helikon_core::ToolChoice::Tool { .. })
    ) && req.tools.is_empty()
    {
        return Err(ModelError::Other(anyhow::anyhow!(
            "tool_choice requires at least one tool"
        )));
    }

    // tool_choice Tool{name} must reference a declared tool.
    if let Some(paigasus_helikon_core::ToolChoice::Tool { name }) = &s.tool_choice {
        if !req.tools.iter().any(|tool| tool.name == *name) {
            return Err(ModelError::Other(anyhow::anyhow!(
                "tool_choice references unknown tool `{name}`"
            )));
        }
    }

    let translated = items_to_contents(&req.messages)?;
    let mut body = Map::new();
    body.insert("contents".into(), Value::Array(translated.contents));
    if let Some(sys) = translated.system {
        body.insert("systemInstruction".into(), sys);
    }

    let decls = function_declarations(&req.tools);
    if !decls.is_empty() {
        body.insert("tools".into(), Value::Array(decls));
    }
    let all_names: Vec<String> = req.tools.iter().map(|t| t.name.clone()).collect();
    if let Some(fcc) = function_calling_config(s.tool_choice.as_ref(), &all_names) {
        body.insert(
            "toolConfig".into(),
            serde_json::json!({ "functionCallingConfig": fcc }),
        );
    }

    let mut gen = Map::new();
    if let Some(t) = s.temperature {
        gen.insert("temperature".into(), serde_json::json!(t));
    }
    if let Some(p) = s.top_p {
        gen.insert("topP".into(), serde_json::json!(p));
    }
    if let Some(m) = s.max_output_tokens {
        gen.insert("maxOutputTokens".into(), serde_json::json!(m));
    }
    if let Some((mime, schema)) = response_format_fields(s.response_format.as_ref()) {
        gen.insert("responseMimeType".into(), Value::String(mime));
        if let Some(sc) = schema {
            gen.insert("responseSchema".into(), sc);
        }
    }
    if !gen.is_empty() {
        body.insert("generationConfig".into(), Value::Object(gen));
    }

    // model_id is carried by the URL, not the body (Developer/Vertex differ);
    // reference it here so the orchestrator owns the full config surface.
    let _ = &cfg.model_id;
    Ok(PreparedRequest {
        body: Value::Object(body),
    })
}

#[cfg(test)]
mod snap {
    use super::*;
    use crate::builder::{Config, Transport};
    use paigasus_helikon_core::{
        ContentPart, Item, ModelCapabilities, ModelRequest, ResponseFormat, ToolChoice, ToolDef,
    };
    use serde_json::json;

    fn cfg() -> Config {
        Config {
            http: reqwest::Client::new(),
            base_url: None,
            model_id: "gemini-2.5-flash".into(),
            transport: Transport::Developer,
            auth: crate::auth::Auth::ApiKey("k".into()),
            capabilities: ModelCapabilities::empty(),
        }
    }
    fn user(s: &str) -> Item {
        Item::UserMessage {
            content: vec![ContentPart::Text { text: s.into() }],
        }
    }
    fn body(req: ModelRequest) -> serde_json::Value {
        build_request(&cfg(), &req).unwrap().body
    }

    #[test]
    fn snap_plain_text_turn() {
        let mut r = ModelRequest::new();
        r.messages = vec![user("hello")];
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_generation_config() {
        let mut r = ModelRequest::new();
        r.messages = vec![user("hi")];
        r.model_settings.temperature = Some(0.7);
        r.model_settings.top_p = Some(0.9);
        r.model_settings.max_output_tokens = Some(256);
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_system_instruction() {
        let mut r = ModelRequest::new();
        r.messages = vec![
            Item::System {
                content: vec![ContentPart::Text {
                    text: "be terse".into(),
                }],
            },
            user("hi"),
        ];
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_tool_declarations_and_choice_auto() {
        let mut r = ModelRequest::new();
        r.messages = vec![user("search")];
        r.tools = vec![ToolDef {
            name: "search".into(),
            description: "search".into(),
            schema: json!({ "type": "object", "properties": { "q": { "type": "string" } } }),
        }];
        r.model_settings.tool_choice = Some(ToolChoice::Auto);
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_tool_call_and_result() {
        let mut r = ModelRequest::new();
        r.messages = vec![
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
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_parallel_same_name_tool_calls() {
        let mut r = ModelRequest::new();
        r.messages = vec![
            user("two searches"),
            Item::ToolCall {
                call_id: "fc_0".into(),
                name: "search".into(),
                args: json!({"q":"a"}),
            },
            Item::ToolCall {
                call_id: "fc_1".into(),
                name: "search".into(),
                args: json!({"q":"b"}),
            },
            Item::ToolResult {
                call_id: "fc_0".into(),
                content: vec![ContentPart::Text {
                    text: "{\"n\":1}".into(),
                }],
            },
            Item::ToolResult {
                call_id: "fc_1".into(),
                content: vec![ContentPart::Text {
                    text: "{\"n\":2}".into(),
                }],
            },
        ];
        let b = body(r);
        // Both responses carry the real name "search" but distinct ids.
        assert_eq!(
            b["contents"][3]["parts"][0]["functionResponse"]["name"],
            "search"
        );
        assert_eq!(
            b["contents"][3]["parts"][0]["functionResponse"]["id"],
            "fc_0"
        );
        assert_eq!(
            b["contents"][4]["parts"][0]["functionResponse"]["id"],
            "fc_1"
        );
        insta::assert_json_snapshot!(b);
    }

    #[test]
    fn snap_structured_output_json_schema() {
        let mut r = ModelRequest::new();
        r.messages = vec![user("extract")];
        r.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Person".into(),
            schema: json!({ "type": "object", "properties": { "name": { "type": "string" } }, "additionalProperties": false }),
            strict: true,
        });
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_inline_image() {
        let mut r = ModelRequest::new();
        r.messages = vec![Item::UserMessage {
            content: vec![
                ContentPart::Text {
                    text: "what is this".into(),
                },
                ContentPart::Image {
                    source: paigasus_helikon_core::MediaSource::Base64 {
                        mime_type: "image/png".into(),
                        data: "AAAA".into(),
                    },
                },
            ],
        }];
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn structured_output_plus_tools_errors() {
        let mut r = ModelRequest::new();
        r.messages = vec![user("x")];
        r.tools = vec![ToolDef {
            name: "t".into(),
            description: "".into(),
            schema: json!({}),
        }];
        r.model_settings.response_format = Some(ResponseFormat::JsonObject);
        let err = build_request(&cfg(), &r).unwrap_err();
        insta::assert_snapshot!(err.to_string());
    }

    #[test]
    fn finalize_after_tool_use_allowed() {
        // tools: [] + JsonSchema, with prior function parts in history -> no error.
        let mut r = ModelRequest::new();
        r.messages = vec![
            user("q"),
            Item::ToolCall {
                call_id: "fc_0".into(),
                name: "search".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "fc_0".into(),
                content: vec![ContentPart::Text { text: "{}".into() }],
            },
        ];
        r.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Out".into(),
            schema: json!({ "type": "object", "properties": {} }),
            strict: true,
        });
        assert!(build_request(&cfg(), &r).is_ok());
    }

    #[test]
    fn empty_conversation_errors() {
        let r = ModelRequest::new();
        assert!(build_request(&cfg(), &r).is_err());
    }

    #[test]
    fn tool_choice_naming_undeclared_tool_errors() {
        let mut r = ModelRequest::new();
        r.messages = vec![user("go")];
        r.tools = vec![ToolDef {
            name: "search".into(),
            description: "search".into(),
            schema: json!({ "type": "object", "properties": {} }),
        }];
        r.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "missing".into(),
        });
        assert!(build_request(&cfg(), &r).is_err());
    }
}
