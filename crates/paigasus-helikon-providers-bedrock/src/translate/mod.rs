//! Translation layer between the Paigasus core types and Bedrock Converse API types.

pub mod schema;

pub(crate) mod request;
pub(crate) mod response_format;
pub(crate) mod tools;

use aws_sdk_bedrockruntime::types::{
    AnyToolChoice, AutoToolChoice, InferenceConfiguration, Message, SpecificToolChoice,
    SystemContentBlock, Tool, ToolChoice as BedrockToolChoice, ToolConfiguration,
};
use paigasus_helikon_core::{ModelError, ModelRequest, ToolChoice};

use crate::builder::Config;
use crate::family::ModelFamily; // used by translate_tool_choice
use crate::translate::request::items_to_messages;
use crate::translate::response_format::{synthesize, Synthesized};
use crate::translate::schema::Ruleset;
use crate::translate::tools::tool_specs;

/// A fully-assembled Bedrock Converse request, ready to send.
///
/// The fields map onto the Bedrock SDK `converse` / `converse_stream` input
/// parameters. Callers should inspect `synthesizing` to decide how to route
/// stream events (synthesized structured output → `TokenDelta` remapping).
#[derive(Debug)]
pub(crate) struct PreparedConverse {
    /// Bedrock model identifier (may include cross-region profile prefix).
    pub(crate) model_id: String,
    /// System prompt blocks.
    pub(crate) system: Vec<SystemContentBlock>,
    /// Strictly-alternating conversation turns.
    pub(crate) messages: Vec<Message>,
    /// Tool configuration (tools list + optional tool_choice).
    pub(crate) tool_config: Option<ToolConfiguration>,
    /// Base inference parameters.
    pub(crate) inference_config: Option<InferenceConfiguration>,
    /// Whether the stream translator should remap the synthesized tool's
    /// output to `TokenDelta`.
    pub(crate) synthesizing: bool,
}

/// Build an [`InferenceConfiguration`] from per-request settings and the builder config.
///
/// `maxTokens` is set only when the caller explicitly provided
/// `max_output_tokens` on the request or the builder was configured with
/// [`crate::BedrockModelBuilder::max_output_tokens_default`]. Omitting
/// `maxTokens` lets each Bedrock model apply its own correct default, which
/// avoids `ValidationException` on models whose limit is below any hardcoded
/// value.
///
/// Returns `None` when all three fields (temperature, top_p, max_tokens) are
/// absent — Bedrock accepts a missing `inferenceConfig` and all its fields are
/// individually optional.
fn build_inference_config(req: &ModelRequest, cfg: &Config) -> Option<InferenceConfiguration> {
    let max_tokens = req
        .model_settings
        .max_output_tokens
        .or(cfg.max_output_default);

    // When no inference field is set, omit the config block entirely.
    if req.model_settings.temperature.is_none()
        && req.model_settings.top_p.is_none()
        && max_tokens.is_none()
    {
        return None;
    }

    let mut b = InferenceConfiguration::builder();
    if let Some(t) = req.model_settings.temperature {
        b = b.temperature(t);
    }
    if let Some(p) = req.model_settings.top_p {
        b = b.top_p(p);
    }
    if let Some(mt) = max_tokens {
        b = b.max_tokens(mt as i32);
    }
    Some(b.build())
}

/// Assemble a [`PreparedConverse`] from the builder `Config` and a [`ModelRequest`].
///
/// # Errors
/// - Reserved tool name (`__paigasus_structured_output__` used by the caller).
/// - `ResponseFormat::JsonSchema`/`JsonObject` combined with `ToolChoice::Tool` (conflict).
/// - `ResponseFormat::JsonSchema`/`JsonObject` combined with `ToolChoice::None` (conflict).
/// - Empty conversation (no non-system messages).
pub(crate) fn build_request(
    cfg: &Config,
    req: &ModelRequest,
) -> Result<PreparedConverse, ModelError> {
    let family = cfg.family;
    let ruleset = Ruleset::for_family(family);

    // Guard: ResponseFormat::JsonSchema / JsonObject + ToolChoice::Tool → conflict.
    let synthesizing_rf = matches!(
        req.model_settings.response_format.as_ref(),
        Some(paigasus_helikon_core::ResponseFormat::JsonSchema { .. })
            | Some(paigasus_helikon_core::ResponseFormat::JsonObject),
    );
    let forced_tool = matches!(
        req.model_settings.tool_choice.as_ref(),
        Some(ToolChoice::Tool { .. }),
    );
    if synthesizing_rf && forced_tool {
        return Err(ModelError::Other(anyhow::anyhow!(
            "ResponseFormat::JsonSchema/JsonObject and ToolChoice::Tool are mutually \
             exclusive on Bedrock — use one or the other",
        )));
    }

    // Guard: ResponseFormat::JsonSchema / JsonObject + ToolChoice::None → conflict.
    // Synthesis requires injecting a tool; ToolChoice::None suppresses toolConfig entirely.
    let none_choice = matches!(
        req.model_settings.tool_choice.as_ref(),
        Some(ToolChoice::None),
    );
    if synthesizing_rf && none_choice {
        return Err(ModelError::Other(anyhow::anyhow!(
            "ResponseFormat::JsonSchema/JsonObject and ToolChoice::None are mutually \
             exclusive on Bedrock — structured-output synthesis requires a tool call",
        )));
    }

    // ToolChoice::None means the model MUST NOT call any tool this turn.
    // Omit toolConfig entirely (do not send user tools either).
    if none_choice {
        let translated = items_to_messages(&req.messages)?;
        let inference_config = build_inference_config(req, cfg);
        return Ok(PreparedConverse {
            model_id: cfg.model_id.clone(),
            system: translated.system,
            messages: translated.messages,
            tool_config: None,
            inference_config,
            synthesizing: false,
        });
    }

    // Translate messages.
    let translated = items_to_messages(&req.messages)?;

    // Translate user tool defs (also validates reserved name).
    let user_specs = tool_specs(&req.tools, ruleset)?;

    // Synthesize structured output tool (if applicable).
    let Synthesized {
        tool: synth_tool,
        tool_choice: synth_tc,
        synthesizing,
    } = synthesize(req.model_settings.response_format.as_ref(), family, ruleset)?;

    // Merge tool lists: user tools first, synthesized appended.
    let mut all_tools: Vec<Tool> = user_specs.into_iter().map(Tool::ToolSpec).collect();
    if let Some(st) = synth_tool {
        all_tools.push(Tool::ToolSpec(st));
    }

    // Build ToolConfiguration (only when there are tools or a tool_choice).
    let caller_tc = req
        .model_settings
        .tool_choice
        .as_ref()
        .and_then(|tc| translate_tool_choice(tc, family));

    // Synthesis overrides caller tool_choice.
    // synth_tool.is_some() always pushes to all_tools above, so a synthesized tool_choice is never silently dropped here.
    let effective_tc = synth_tc.or(caller_tc);

    let tool_config = if !all_tools.is_empty() {
        let mut builder = ToolConfiguration::builder();
        for t in all_tools {
            builder = builder.tools(t);
        }
        if let Some(tc) = effective_tc {
            builder = builder.tool_choice(tc);
        }
        Some(builder.build().map_err(|e| {
            ModelError::Other(anyhow::anyhow!("failed to build ToolConfiguration: {e}"))
        })?)
    } else if let Some(tc) = effective_tc {
        // tool_choice without tools (unusual but pass through)
        let mut builder = ToolConfiguration::builder();
        builder = builder.tool_choice(tc);
        // ToolConfiguration requires at least one tool — skip tool_choice when
        // no tools are present to avoid a build error.
        tracing::debug!(
            target: "paigasus::bedrock::translate",
            "tool_choice requested but no tools provided; omitting ToolConfiguration",
        );
        let _ = builder; // suppress unused warning
        None
    } else {
        None
    };

    // Build InferenceConfiguration.
    let inference_config = build_inference_config(req, cfg);

    Ok(PreparedConverse {
        model_id: cfg.model_id.clone(),
        system: translated.system,
        messages: translated.messages,
        tool_config,
        inference_config,
        synthesizing,
    })
}

/// Translate a core [`ToolChoice`] into a Bedrock [`BedrockToolChoice`].
///
/// Returns `None` when the family does not support forced tool choice (the
/// `tool_choice` field is omitted from the request). `ToolChoice::None` is
/// handled before this function is called (it suppresses `toolConfig` entirely).
fn translate_tool_choice(tc: &ToolChoice, family: ModelFamily) -> Option<BedrockToolChoice> {
    match tc {
        ToolChoice::Auto => Some(BedrockToolChoice::Auto(AutoToolChoice::builder().build())),
        ToolChoice::Required => Some(BedrockToolChoice::Any(AnyToolChoice::builder().build())),
        ToolChoice::None => {
            // ToolChoice::None is handled early in build_request; unreachable here.
            None
        }
        ToolChoice::Tool { name } => {
            if !family.supports_forced_tool_choice() {
                tracing::debug!(
                    target: "paigasus::bedrock::translate",
                    ?family,
                    tool = %name,
                    "ToolChoice::Tool requested but family does not support forced tool choice; omitting",
                );
                return None;
            }
            match SpecificToolChoice::builder().name(name).build() {
                Ok(s) => Some(BedrockToolChoice::Tool(s)),
                Err(e) => {
                    tracing::warn!(
                        target: "paigasus::bedrock::translate",
                        err = %e,
                        "failed to build SpecificToolChoice; omitting tool_choice",
                    );
                    None
                }
            }
        }
        _ => {
            tracing::debug!(
                target: "paigasus::bedrock::translate",
                "unknown ToolChoice variant; omitting",
            );
            None
        }
    }
}

// ── Wire-projection (snapshot tests) ─────────────────────────────────────────

/// Project a [`PreparedConverse`] into a stable [`serde_json::Value`] for
/// snapshot tests.
///
/// This projection is deliberately **not** based on `Debug` output of the SDK
/// types, which can change across SDK version bumps. Instead, it is a hand-written
/// extraction of the semantically relevant fields.
#[cfg(test)]
pub(crate) fn to_wire_json(p: &PreparedConverse) -> serde_json::Value {
    use aws_sdk_bedrockruntime::types::{
        ContentBlock, SystemContentBlock, ToolChoice as SdkToolChoice, ToolResultContentBlock,
    };
    use serde_json::{json, Value};

    // System blocks
    let system: Vec<Value> = p
        .system
        .iter()
        .map(|s| match s {
            SystemContentBlock::Text(t) => json!({"text": t}),
            _ => json!({"unknown": true}),
        })
        .collect();

    // Messages
    let messages: Vec<Value> = p
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                aws_sdk_bedrockruntime::types::ConversationRole::User => "user",
                aws_sdk_bedrockruntime::types::ConversationRole::Assistant => "assistant",
                _ => "unknown",
            };
            let content: Vec<Value> = m
                .content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text(t) => json!({"text": t}),
                    ContentBlock::ToolUse(tu) => json!({
                        "toolUse": {
                            "toolUseId": tu.tool_use_id,
                            "name": tu.name,
                        }
                    }),
                    ContentBlock::ToolResult(tr) => {
                        let text_content = tr
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
                            "toolResult": {
                                "toolUseId": tr.tool_use_id,
                                "content": [{"text": text_content}],
                            }
                        })
                    }
                    _ => json!({"unknown": true}),
                })
                .collect();
            json!({"role": role, "content": content})
        })
        .collect();

    // Tool config
    let tool_config: Value = match &p.tool_config {
        None => Value::Null,
        Some(tc) => {
            let tools: Vec<Value> = tc
                .tools()
                .iter()
                .map(|t| match t {
                    Tool::ToolSpec(spec) => {
                        let schema_placeholder = match spec.input_schema() {
                            Some(s) if s.is_json() => json!("<Document>"),
                            _ => Value::Null,
                        };
                        json!({
                            "toolSpec": {
                                "name": spec.name(),
                                "description": spec.description(),
                                "inputSchema": {"json": schema_placeholder},
                            }
                        })
                    }
                    _ => json!({"unknown": true}),
                })
                .collect();
            let tc_value: Value = match tc.tool_choice() {
                None => Value::Null,
                Some(SdkToolChoice::Auto(_)) => json!({"auto": {}}),
                Some(SdkToolChoice::Any(_)) => json!({"any": {}}),
                Some(SdkToolChoice::Tool(s)) => json!({"tool": {"name": s.name()}}),
                _ => json!({"unknown": true}),
            };
            json!({
                "tools": tools,
                "toolChoice": tc_value,
            })
        }
    };

    // Inference config
    let inference_config: Value = match &p.inference_config {
        None => Value::Null,
        Some(ic) => {
            let mut m = serde_json::Map::new();
            if let Some(t) = ic.temperature() {
                m.insert("temperature".into(), json!(t));
            }
            if let Some(p) = ic.top_p() {
                m.insert("topP".into(), json!(p));
            }
            if let Some(mt) = ic.max_tokens() {
                m.insert("maxTokens".into(), json!(mt));
            }
            Value::Object(m)
        }
    };

    json!({
        "modelId": p.model_id,
        "system": system,
        "messages": messages,
        "toolConfig": tool_config,
        "inferenceConfig": inference_config,
        "synthesizing": p.synthesizing,
    })
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::tools::SYNTHESIZED_TOOL_NAME;
    use insta::assert_json_snapshot;
    use paigasus_helikon_core::{
        ContentPart, Item, ModelRequest, ResponseFormat, ToolChoice, ToolDef,
    };
    use serde_json::json;

    use crate::capabilities::caps_for;
    use crate::family::ModelFamily;

    fn make_cfg(model_id: &str) -> Config {
        let family = ModelFamily::from_model_id(model_id);
        let capabilities = caps_for(family);
        // Build an offline Bedrock client for test configs.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");
        let client = rt.block_on(async {
            let sdk_cfg = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12())
                .region(aws_config::Region::new("us-east-1"))
                .test_credentials()
                .load()
                .await;
            aws_sdk_bedrockruntime::Client::new(&sdk_cfg)
        });
        Config {
            client,
            model_id: model_id.to_owned(),
            family,
            capabilities,
            max_output_default: None,
        }
    }

    /// A minimal Config for tests — uses a Claude model so synthesis works.
    fn claude_cfg() -> Config {
        make_cfg("anthropic.claude-3-5-sonnet-20241022-v2:0")
    }

    /// A Llama config for testing unsupported-family degradation.
    fn llama_cfg() -> Config {
        make_cfg("meta.llama3-1-70b-instruct-v1:0")
    }

    fn user_text(s: &str) -> Item {
        Item::UserMessage {
            content: vec![ContentPart::Text { text: s.to_owned() }],
        }
    }

    #[test]
    fn empty_messages_returns_error() {
        let req = ModelRequest::new();
        let err = build_request(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn plain_text_turn_no_tools() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        let p = build_request(&claude_cfg(), &req).unwrap();
        assert_eq!(p.model_id, "anthropic.claude-3-5-sonnet-20241022-v2:0");
        assert_eq!(p.messages.len(), 1);
        assert!(p.tool_config.is_none());
        assert!(!p.synthesizing);
    }

    #[test]
    fn inference_config_fields_mapped() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.temperature = Some(0.7);
        req.model_settings.top_p = Some(0.9);
        req.model_settings.max_output_tokens = Some(512);
        let p = build_request(&claude_cfg(), &req).unwrap();
        let ic = p.inference_config.as_ref().unwrap();
        assert!((ic.temperature().unwrap() - 0.7f32).abs() < 1e-5);
        assert!((ic.top_p().unwrap() - 0.9f32).abs() < 1e-5);
        assert_eq!(ic.max_tokens(), Some(512));
    }

    #[test]
    fn json_schema_on_claude_synthesizes() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("make a person")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Person".to_owned(),
            schema: json!({"type": "object"}),
            strict: false,
        });
        let p = build_request(&claude_cfg(), &req).unwrap();
        assert!(p.synthesizing);
        let tc = p.tool_config.as_ref().unwrap();
        assert_eq!(tc.tools().len(), 1);
        assert_eq!(
            tc.tools()[0].as_tool_spec().unwrap().name(),
            SYNTHESIZED_TOOL_NAME,
        );
        // tool_choice should be Tool variant pointing to synthesized name.
        let tc_choice = tc.tool_choice().unwrap();
        assert!(tc_choice.is_tool());
        assert_eq!(tc_choice.as_tool().unwrap().name(), SYNTHESIZED_TOOL_NAME,);
    }

    #[test]
    fn json_schema_on_llama_degrades_to_no_synthesis() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("make a thing")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({"type": "object"}),
            strict: false,
        });
        let p = build_request(&llama_cfg(), &req).unwrap();
        assert!(!p.synthesizing);
        assert!(p.tool_config.is_none());
    }

    #[test]
    fn json_schema_plus_tool_choice_tool_is_conflict() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({}),
            strict: false,
        });
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "search".to_owned(),
        });
        let err = build_request(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn json_schema_plus_tool_choice_none_is_conflict() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({}),
            strict: false,
        });
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let err = build_request(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn json_object_plus_tool_choice_none_is_conflict() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.response_format = Some(ResponseFormat::JsonObject);
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let err = build_request(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn tool_choice_none_with_user_tools_omits_tool_config() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef {
            name: "ping".to_owned(),
            description: "Ping".to_owned(),
            schema: json!({"type": "object"}),
        }];
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let p = build_request(&claude_cfg(), &req).unwrap();
        // toolConfig must be absent — None suppresses tool calls entirely.
        assert!(p.tool_config.is_none());
        assert!(!p.synthesizing);
    }

    #[test]
    fn tool_choice_auto_maps_to_bedrock_auto() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef {
            name: "ping".to_owned(),
            description: "ping".to_owned(),
            schema: json!({"type": "object"}),
        }];
        req.model_settings.tool_choice = Some(ToolChoice::Auto);
        let p = build_request(&claude_cfg(), &req).unwrap();
        let tc = p.tool_config.as_ref().unwrap();
        assert!(tc.tool_choice().unwrap().is_auto());
    }

    #[test]
    fn tool_choice_required_maps_to_bedrock_any() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef {
            name: "ping".to_owned(),
            description: "ping".to_owned(),
            schema: json!({"type": "object"}),
        }];
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        let p = build_request(&claude_cfg(), &req).unwrap();
        let tc = p.tool_config.as_ref().unwrap();
        assert!(tc.tool_choice().unwrap().is_any());
    }

    #[test]
    fn tool_choice_tool_maps_to_bedrock_specific_on_claude() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef {
            name: "ping".to_owned(),
            description: "ping".to_owned(),
            schema: json!({"type": "object"}),
        }];
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "ping".to_owned(),
        });
        let p = build_request(&claude_cfg(), &req).unwrap();
        let tc = p.tool_config.as_ref().unwrap();
        let choice = tc.tool_choice().unwrap();
        assert!(choice.is_tool());
        assert_eq!(choice.as_tool().unwrap().name(), "ping");
    }

    #[test]
    fn reserved_tool_name_rejected() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef {
            name: SYNTHESIZED_TOOL_NAME.to_owned(),
            description: "bad".to_owned(),
            schema: json!({}),
        }];
        let err = build_request(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn to_wire_json_projection_is_stable() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        let p = build_request(&claude_cfg(), &req).unwrap();
        let w = to_wire_json(&p);
        assert_eq!(w["modelId"], "anthropic.claude-3-5-sonnet-20241022-v2:0");
        assert_eq!(w["messages"][0]["role"], "user");
        assert_eq!(w["messages"][0]["content"][0]["text"], "hello");
        assert!(!w["synthesizing"].as_bool().unwrap());
    }

    // ── Snapshot tests (migrated from tests/converse_request.rs) ─────────────

    #[test]
    fn snap_plain_text_turn() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        let p = build_request(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_tool_call_and_result() {
        let mut req = ModelRequest::new();
        req.tools = vec![ToolDef {
            name: "get_balance".to_owned(),
            description: "Get account balance".to_owned(),
            schema: json!({"type": "object", "properties": {"account_id": {"type": "string"}}}),
        }];
        req.messages = vec![
            user_text("What's my balance?"),
            Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "tu_1".to_owned(),
                    name: "get_balance".to_owned(),
                    args: json!({"account_id": "acc_123"}),
                }],
                agent: None,
            },
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![ContentPart::Text {
                    text: "$1,234.56".to_owned(),
                }],
            },
        ];
        let p = build_request(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_structured_output_json_schema_on_claude_synthesizes() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("Extract the transaction data.")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Transaction".to_owned(),
            schema: json!({
                "type": "object",
                "properties": {
                    "amount": {"type": "number"},
                    "currency": {"type": "string"},
                },
                "required": ["amount", "currency"]
            }),
            strict: false,
        });
        let p = build_request(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_tool_choice_auto() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef {
            name: "ping".to_owned(),
            description: "Ping".to_owned(),
            schema: json!({"type": "object"}),
        }];
        req.model_settings.tool_choice = Some(ToolChoice::Auto);
        let p = build_request(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_tool_choice_required() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef {
            name: "ping".to_owned(),
            description: "Ping".to_owned(),
            schema: json!({"type": "object"}),
        }];
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        let p = build_request(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_tool_choice_specific_tool() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef {
            name: "ping".to_owned(),
            description: "Ping".to_owned(),
            schema: json!({"type": "object"}),
        }];
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "ping".to_owned(),
        });
        let p = build_request(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_inference_config_temperature_top_p_max_tokens() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.temperature = Some(0.7);
        req.model_settings.top_p = Some(0.9);
        req.model_settings.max_output_tokens = Some(256);
        let p = build_request(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_unsupported_family_json_schema_degrades_to_no_synthesis() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("classify this")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Category".to_owned(),
            schema: json!({"type": "object", "properties": {"label": {"type": "string"}}}),
            strict: false,
        });
        let p = build_request(&llama_cfg(), &req).unwrap();
        let wire = to_wire_json(&p);
        assert_eq!(wire["synthesizing"], false);
        assert_json_snapshot!(wire);
    }

    #[test]
    fn snap_conflict_json_schema_plus_tool_choice_tool_returns_err() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({}),
            strict: false,
        });
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "some_tool".to_owned(),
        });
        let err = build_request(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn snap_tool_choice_none_with_user_tools_omits_tool_config() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef {
            name: "ping".to_owned(),
            description: "Ping".to_owned(),
            schema: json!({"type": "object"}),
        }];
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let p = build_request(&claude_cfg(), &req).unwrap();
        // toolConfig must be null in wire representation
        assert_json_snapshot!(to_wire_json(&p));
    }

    // ── maxTokens omission tests ──────────────────────────────────────────────

    #[test]
    fn no_max_output_tokens_omits_max_tokens_from_wire() {
        // When the caller sets neither max_output_tokens nor builder default,
        // inferenceConfig must be absent (no maxTokens, no temperature, no topP).
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        let p = build_request(&claude_cfg(), &req).unwrap();
        // inferenceConfig should be None — no fields to populate.
        assert!(
            p.inference_config.is_none(),
            "inferenceConfig should be absent when no inference fields are set",
        );
        // Wire JSON must not contain maxTokens.
        let w = to_wire_json(&p);
        assert_eq!(
            w["inferenceConfig"],
            serde_json::Value::Null,
            "wire inferenceConfig should be null when nothing is set",
        );
    }

    #[test]
    fn explicit_max_output_tokens_sets_max_tokens_on_wire() {
        // When the caller sets max_output_tokens the wire must carry maxTokens.
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        req.model_settings.max_output_tokens = Some(512);
        let p = build_request(&claude_cfg(), &req).unwrap();
        let ic = p
            .inference_config
            .as_ref()
            .expect("inferenceConfig present");
        assert_eq!(ic.max_tokens(), Some(512));
        let w = to_wire_json(&p);
        assert_eq!(w["inferenceConfig"]["maxTokens"], 512);
    }

    #[test]
    fn builder_default_max_output_tokens_sets_max_tokens_on_wire() {
        // When only the builder default is set (no per-request value), the wire
        // must carry that default as maxTokens.
        let family = ModelFamily::from_model_id("anthropic.claude-3-5-sonnet-20241022-v2:0");
        let capabilities = caps_for(family);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");
        let client = rt.block_on(async {
            let sdk_cfg = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12())
                .region(aws_config::Region::new("us-east-1"))
                .test_credentials()
                .load()
                .await;
            aws_sdk_bedrockruntime::Client::new(&sdk_cfg)
        });
        let cfg_with_default = Config {
            client,
            model_id: "anthropic.claude-3-5-sonnet-20241022-v2:0".to_owned(),
            family,
            capabilities,
            max_output_default: Some(2048),
        };

        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        // No per-request max_output_tokens — builder default should apply.
        let p = build_request(&cfg_with_default, &req).unwrap();
        let ic = p
            .inference_config
            .as_ref()
            .expect("inferenceConfig present");
        assert_eq!(ic.max_tokens(), Some(2048));
        let w = to_wire_json(&p);
        assert_eq!(w["inferenceConfig"]["maxTokens"], 2048);
    }

    #[test]
    fn per_request_max_output_overrides_builder_default() {
        // Per-request value takes precedence over builder default.
        let family = ModelFamily::from_model_id("anthropic.claude-3-5-sonnet-20241022-v2:0");
        let capabilities = caps_for(family);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt");
        let client = rt.block_on(async {
            let sdk_cfg = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12())
                .region(aws_config::Region::new("us-east-1"))
                .test_credentials()
                .load()
                .await;
            aws_sdk_bedrockruntime::Client::new(&sdk_cfg)
        });
        let cfg_with_default = Config {
            client,
            model_id: "anthropic.claude-3-5-sonnet-20241022-v2:0".to_owned(),
            family,
            capabilities,
            max_output_default: Some(2048),
        };

        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        req.model_settings.max_output_tokens = Some(256);
        let p = build_request(&cfg_with_default, &req).unwrap();
        let ic = p
            .inference_config
            .as_ref()
            .expect("inferenceConfig present");
        assert_eq!(ic.max_tokens(), Some(256));
    }
}
