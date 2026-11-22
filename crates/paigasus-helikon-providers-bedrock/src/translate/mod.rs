//! Translation layer between the Paigasus core types and Bedrock Converse API types.

pub mod schema;

pub(crate) mod additional;
mod native;
pub(crate) mod native_schema;
pub(crate) mod request;
pub(crate) mod response_format;
pub(crate) mod tool_choice_none;
pub(crate) mod tools;

use aws_sdk_bedrockruntime::types::{
    AnyToolChoice, AutoToolChoice, InferenceConfiguration, Message, OutputConfig,
    SpecificToolChoice, SystemContentBlock, Tool, ToolChoice as BedrockToolChoice,
    ToolConfiguration,
};
use paigasus_helikon_core::{ModelError, ModelRequest, ToolChoice};

use crate::builder::Config;
use crate::family::ModelFamily; // used by translate_tool_choice
use crate::policy::{NativeNotice, OutputPath};
use crate::translate::additional::AdditionalFields;
use crate::translate::request::items_to_messages;
use crate::translate::response_format::{synthesize, Synthesized};
use crate::translate::schema::Ruleset;
use crate::translate::tool_choice_none::apply_tool_choice_none;
use crate::translate::tools::{partition_tools, tool_specs};

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
    /// Conversation turns. Adjacent same-role turns are joined.
    pub(crate) messages: Vec<Message>,
    /// Tool configuration (tools list + optional tool_choice).
    pub(crate) tool_config: Option<ToolConfiguration>,
    /// Base inference parameters.
    pub(crate) inference_config: Option<InferenceConfiguration>,
    /// Whether the stream translator should remap the synthesized tool's
    /// output to `TokenDelta`.
    pub(crate) synthesizing: bool,
    /// Native structured-output configuration. Only the native path sets it.
    pub(crate) output_config: Option<OutputConfig>,
    /// The changes of meaning that the native path made to the request.
    /// The forced-tool path always leaves this empty.
    pub(crate) notices: Vec<NativeNotice>,
    /// The fields for `additionalModelRequestFields`.
    pub(crate) additional: AdditionalFields,
}

impl PreparedConverse {
    /// Returns `true` when the tool configuration forces a tool call
    /// (`toolChoice` is `any` or `tool`).
    pub(crate) fn has_forced_tool_choice(&self) -> bool {
        matches!(
            self.tool_config
                .as_ref()
                .and_then(ToolConfiguration::tool_choice),
            Some(BedrockToolChoice::Any(_) | BedrockToolChoice::Tool(_))
        ) || self.additional.is_forced()
    }
}

/// The error for `ToolChoice::Required` when the tool list is empty.
fn required_without_tools_error() -> ModelError {
    ModelError::Other(anyhow::anyhow!(
        "ToolChoice::Required requires at least one tool to be defined",
    ))
}

/// Make sure that `name` is the name of a tool in `tools`.
///
/// # Errors
/// Returns [`ModelError::Other`] when no tool in `tools` has the name `name`.
fn check_tool_name(tools: &[Tool], name: &str) -> Result<(), ModelError> {
    let defined = tools
        .iter()
        .any(|t| matches!(t, Tool::ToolSpec(spec) if spec.name() == name));
    if defined {
        Ok(())
    } else {
        Err(ModelError::Other(anyhow::anyhow!(
            "ToolChoice::Tool references tool {:?} which is not defined in the tool list",
            name,
        )))
    }
}

/// Assemble the [`ToolConfiguration`] from the tool list and the optional tool choice.
///
/// Returns `None` when `tools` is empty. A `tool_choice` with no tools is then
/// omitted, because a `ToolConfiguration` needs at least one tool.
///
/// # Errors
/// Returns [`ModelError::Other`] when the SDK builder fails.
fn assemble_tool_config(
    tools: Vec<Tool>,
    tool_choice: Option<BedrockToolChoice>,
) -> Result<Option<ToolConfiguration>, ModelError> {
    if tools.is_empty() {
        if tool_choice.is_some() {
            tracing::debug!(
                target: "paigasus::bedrock::translate",
                "tool_choice requested but no tools provided; omitting ToolConfiguration",
            );
        }
        return Ok(None);
    }
    let mut builder = ToolConfiguration::builder();
    for t in tools {
        builder = builder.tools(t);
    }
    if let Some(tc) = tool_choice {
        builder = builder.tool_choice(tc);
    }
    Ok(Some(builder.build().map_err(|e| {
        ModelError::Other(anyhow::anyhow!("failed to build ToolConfiguration: {e}"))
    })?))
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
/// `path` selects the wire path. On [`OutputPath::Native`] the request goes to
/// the native builder, which sends structured output in `outputConfig` and
/// never forces a tool choice. The errors below are for
/// [`OutputPath::ForcedTool`].
///
/// # Errors
/// - Reserved tool name (`__paigasus_structured_output__` used by the caller).
/// - `ResponseFormat::JsonSchema`/`JsonObject` combined with `ToolChoice::Tool` (conflict).
/// - `ResponseFormat::JsonSchema`/`JsonObject` combined with `ToolChoice::None` (conflict).
/// - No message with content after empty/whitespace-only text is dropped.
/// - The last user message had only empty/whitespace-only text and the request would end with an assistant message.
pub(crate) fn build_request(
    cfg: &Config,
    req: &ModelRequest,
    path: OutputPath,
) -> Result<PreparedConverse, ModelError> {
    if path == OutputPath::Native {
        return native::build_native(cfg, req);
    }
    let family = cfg.family;
    let ruleset = Ruleset::for_family(family);
    let set = partition_tools(&req.tools, ruleset, cfg.sends_typed_tools())?;

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
    if synthesizing_rf && family.supports_forced_tool_choice() && forced_tool {
        return Err(ModelError::Other(anyhow::anyhow!(
            "ResponseFormat::JsonSchema/JsonObject and ToolChoice::Tool are mutually \
             exclusive on Bedrock — use one or the other",
        )));
    }

    // Guard: ResponseFormat::JsonSchema / JsonObject + ToolChoice::None → conflict.
    // With None the crate cannot force the synthesized tool, so the structured
    // format conflicts with None.
    let none_choice = matches!(
        req.model_settings.tool_choice.as_ref(),
        Some(ToolChoice::None),
    );
    if synthesizing_rf && family.supports_forced_tool_choice() && none_choice {
        return Err(ModelError::Other(anyhow::anyhow!(
            "ResponseFormat::JsonSchema/JsonObject and ToolChoice::None are mutually \
             exclusive on Bedrock — structured-output synthesis requires a tool call",
        )));
    }

    // ToolChoice::None means the model MUST NOT call any tool this turn.
    // Without a tool round in the history, omit toolConfig entirely. With a
    // tool round, Converse needs a toolConfig, so the family strategy decides
    // (a none field for Anthropic models, a text history for the others).
    if none_choice {
        let translated = items_to_messages(&req.messages)?;
        let outcome = apply_tool_choice_none(translated.messages, &set, family)?;
        let tool_config = assemble_tool_config(outcome.tools, None)?;
        let inference_config = build_inference_config(req, cfg);
        return Ok(PreparedConverse {
            model_id: cfg.model_id.clone(),
            system: translated.system,
            messages: outcome.messages,
            tool_config,
            inference_config,
            synthesizing: false,
            output_config: None,
            notices: Vec::new(),
            additional: outcome.additional,
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

    // Validate forced tool choices on supported families.
    // (On unsupported families, forced choices were already omitted by translate_tool_choice.)
    if family.supports_forced_tool_choice() {
        match req.model_settings.tool_choice.as_ref() {
            Some(ToolChoice::Required) if !synthesizing => {
                if all_tools.is_empty() {
                    return Err(required_without_tools_error());
                }
            }
            Some(ToolChoice::Tool { name }) => check_tool_name(&all_tools, name)?,
            _ => {}
        }
    }

    let tool_config = assemble_tool_config(all_tools, effective_tc)?;

    // Build InferenceConfiguration.
    let inference_config = build_inference_config(req, cfg);

    Ok(PreparedConverse {
        model_id: cfg.model_id.clone(),
        system: translated.system,
        messages: translated.messages,
        tool_config,
        inference_config,
        synthesizing,
        output_config: None,
        notices: Vec::new(),
        additional: AdditionalFields::default(),
    })
}

/// Translate a core [`ToolChoice`] into a Bedrock [`BedrockToolChoice`].
///
/// Returns `None` when the family does not support forced tool choice (the
/// `tool_choice` field is omitted from the request). `ToolChoice::None` is
/// handled before this function.
fn translate_tool_choice(tc: &ToolChoice, family: ModelFamily) -> Option<BedrockToolChoice> {
    match tc {
        ToolChoice::Auto => Some(BedrockToolChoice::Auto(AutoToolChoice::builder().build())),
        ToolChoice::Required => {
            if !family.supports_forced_tool_choice() {
                tracing::debug!(
                    target: "paigasus::bedrock::translate",
                    ?family,
                    "ToolChoice::Required requested but family does not support forced tool choice; omitting",
                );
                return None;
            }
            Some(BedrockToolChoice::Any(AnyToolChoice::builder().build()))
        }
        ToolChoice::None => {
            // ToolChoice::None is handled before this function; unreachable here.
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

    let mut wire = json!({
        "modelId": p.model_id,
        "system": system,
        "messages": messages,
        "toolConfig": tool_config,
        "inferenceConfig": inference_config,
        "synthesizing": p.synthesizing,
    });

    // Extra request fields: present only when set, so the other snapshots do
    // not change.
    if let Some(v) = p.additional.to_value() {
        wire["additionalModelRequestFields"] = v;
    }

    // Output config: the key is present only on the native path, so the
    // forced-tool snapshots do not change.
    if let Some(oc) = &p.output_config {
        let text_format = oc.text_format().map(|tf| {
            let structure = tf
                .structure()
                .and_then(|s| s.as_json_schema().ok())
                .map(|js| {
                    let schema: Value = serde_json::from_str(js.schema())
                        .expect("the native output schema is a JSON string");
                    json!({"jsonSchema": {"name": js.name(), "schema": schema}})
                });
            json!({"type": tf.r#type().as_str(), "structure": structure})
        });
        wire["outputConfig"] = json!({"textFormat": text_format});
    }
    wire
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
    use crate::policy::{ForcedChoicePolicy, NativeNotice};

    fn make_cfg(model_id: &str) -> Config {
        crate::builder::offline_config(model_id)
    }

    /// A minimal Config for tests — uses a Claude model so synthesis works.
    fn claude_cfg() -> Config {
        make_cfg("anthropic.claude-3-5-sonnet-20241022-v2:0")
    }

    /// A Llama config for testing unsupported-family degradation.
    fn llama_cfg() -> Config {
        make_cfg("meta.llama3-1-70b-instruct-v1:0")
    }

    /// Build the request on the path that the policy of `cfg` selects.
    fn build(cfg: &Config, req: &ModelRequest) -> Result<PreparedConverse, ModelError> {
        build_request(
            cfg,
            req,
            ForcedChoicePolicy::new(&cfg.model_id, cfg.family).path(),
        )
    }

    /// A model that rejects a forced tool choice (the native path).
    fn sonnet55_cfg() -> Config {
        make_cfg("global.anthropic.claude-sonnet-5-5")
    }

    fn none_choice_of(p: &PreparedConverse) -> bool {
        p.additional.tool_choice == Some(crate::translate::additional::AdditionalToolChoice::None)
    }

    fn assert_no_forced_choice(p: &PreparedConverse) {
        assert!(
            !p.has_forced_tool_choice(),
            "forced tool choice on the native path"
        );
    }

    fn user_text(s: &str) -> Item {
        Item::UserMessage {
            content: vec![ContentPart::Text { text: s.to_owned() }],
        }
    }

    #[test]
    fn build_request_rejects_trailing_blank_user() {
        for tool_choice in [None, Some(ToolChoice::None)] {
            let mut req = ModelRequest::new();
            req.messages = vec![
                user_text("q"),
                Item::AssistantMessage {
                    content: vec![ContentPart::Text {
                        text: "a".to_owned(),
                    }],
                    agent: None,
                },
                user_text(""),
            ];
            if let Some(tc) = tool_choice {
                req.model_settings.tool_choice = Some(tc);
            }
            let err = build(&claude_cfg(), &req).unwrap_err();
            assert!(matches!(err, ModelError::Other(_)));
            assert!(
                err.to_string()
                    .contains("would end with an assistant message"),
                "{err}"
            );
        }
    }

    #[test]
    fn empty_messages_returns_error() {
        let req = ModelRequest::new();
        let err = build(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn plain_text_turn_no_tools() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        let p = build(&claude_cfg(), &req).unwrap();
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
        let p = build(&claude_cfg(), &req).unwrap();
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
        let p = build(&claude_cfg(), &req).unwrap();
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
    fn json_schema_on_global_sonnet_5_synthesizes() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("make a person")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Person".to_owned(),
            schema: json!({"type": "object"}),
            strict: false,
        });
        let p = build(&make_cfg("global.anthropic.claude-sonnet-5"), &req).unwrap();
        assert!(p.synthesizing);
        let tc = p.tool_config.as_ref().unwrap();
        assert_eq!(tc.tools().len(), 1);
        assert_eq!(
            tc.tools()[0].as_tool_spec().unwrap().name(),
            SYNTHESIZED_TOOL_NAME,
        );
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
        let p = build(&llama_cfg(), &req).unwrap();
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
        let err = build(&claude_cfg(), &req).unwrap_err();
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
        let err = build(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn json_object_plus_tool_choice_none_is_conflict() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.response_format = Some(ResponseFormat::JsonObject);
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let err = build(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn tool_choice_none_with_user_tools_omits_tool_config() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "Ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let p = build(&claude_cfg(), &req).unwrap();
        // toolConfig must be absent — None suppresses tool calls entirely.
        assert!(p.tool_config.is_none());
        assert!(!p.synthesizing);
    }

    #[test]
    fn tool_choice_auto_maps_to_bedrock_auto() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::Auto);
        let p = build(&claude_cfg(), &req).unwrap();
        let tc = p.tool_config.as_ref().unwrap();
        assert!(tc.tool_choice().unwrap().is_auto());
    }

    #[test]
    fn tool_choice_required_maps_to_bedrock_any() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        let p = build(&claude_cfg(), &req).unwrap();
        let tc = p.tool_config.as_ref().unwrap();
        assert!(tc.tool_choice().unwrap().is_any());
    }

    #[test]
    fn tool_choice_tool_maps_to_bedrock_specific_on_claude() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "ping".to_owned(),
        });
        let p = build(&claude_cfg(), &req).unwrap();
        let tc = p.tool_config.as_ref().unwrap();
        let choice = tc.tool_choice().unwrap();
        assert!(choice.is_tool());
        assert_eq!(choice.as_tool().unwrap().name(), "ping");
    }

    #[test]
    fn reserved_tool_name_rejected() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new(SYNTHESIZED_TOOL_NAME, "bad", json!({}))];
        let err = build(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn to_wire_json_projection_is_stable() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        let p = build(&claude_cfg(), &req).unwrap();
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
        let p = build(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_tool_call_and_result() {
        let mut req = ModelRequest::new();
        req.tools = vec![ToolDef::new(
            "get_balance",
            "Get account balance",
            json!({"type": "object", "properties": {"account_id": {"type": "string"}}}),
        )];
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
        let p = build(&claude_cfg(), &req).unwrap();
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
        let p = build(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_tool_choice_auto() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "Ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::Auto);
        let p = build(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_tool_choice_required() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "Ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        let p = build(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_tool_choice_specific_tool() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "Ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "ping".to_owned(),
        });
        let p = build(&claude_cfg(), &req).unwrap();
        assert_json_snapshot!(to_wire_json(&p));
    }

    #[test]
    fn snap_inference_config_temperature_top_p_max_tokens() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.temperature = Some(0.7);
        req.model_settings.top_p = Some(0.9);
        req.model_settings.max_output_tokens = Some(256);
        let p = build(&claude_cfg(), &req).unwrap();
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
        let p = build(&llama_cfg(), &req).unwrap();
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
        let err = build(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn snap_tool_choice_none_with_user_tools_omits_tool_config() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "Ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let p = build(&claude_cfg(), &req).unwrap();
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
        let p = build(&claude_cfg(), &req).unwrap();
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
        let p = build(&claude_cfg(), &req).unwrap();
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
            let sdk_cfg = aws_config::defaults(aws_config::BehaviorVersion::v2026_11_17())
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
            native_tool_types: true,
        };

        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        // No per-request max_output_tokens — builder default should apply.
        let p = build(&cfg_with_default, &req).unwrap();
        let ic = p
            .inference_config
            .as_ref()
            .expect("inferenceConfig present");
        assert_eq!(ic.max_tokens(), Some(2048));
        let w = to_wire_json(&p);
        assert_eq!(w["inferenceConfig"]["maxTokens"], 2048);
    }

    // ── Fix A tests: family-gated conflict guards + ToolChoice::Required ─────

    #[test]
    fn json_schema_plus_tool_choice_tool_on_unsupported_family_is_ok() {
        // On a family that doesn't support synthesis, JsonSchema + ToolChoice::Tool must NOT error
        // (the format degrades to text, so there's no real conflict).
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({}),
            strict: false,
        });
        req.tools = vec![ToolDef::new("search", "search", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "search".to_owned(),
        });
        // Must succeed: on Llama there's no synthesis, so no conflict
        let p = build(&llama_cfg(), &req).unwrap();
        assert!(!p.synthesizing);
    }

    #[test]
    fn json_schema_plus_tool_choice_none_on_unsupported_family_is_ok() {
        // On a family that doesn't support synthesis, JsonSchema + ToolChoice::None must NOT error.
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({}),
            strict: false,
        });
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let p = build(&llama_cfg(), &req).unwrap();
        assert!(!p.synthesizing);
        assert!(p.tool_config.is_none()); // None suppresses tool_config
    }

    #[test]
    fn tool_choice_required_on_unsupported_family_omits_tool_choice() {
        // ToolChoice::Required on Llama must NOT produce toolChoice: {any:{}}
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        let p = build(&llama_cfg(), &req).unwrap();
        // tool_config should still be present (tools exist) but toolChoice should be omitted
        let tc = p.tool_config.as_ref().unwrap();
        // toolChoice absent (None) because family doesn't support it
        assert!(tc.tool_choice().is_none());
    }

    #[test]
    fn tool_choice_required_on_supported_family_maps_to_any() {
        // Sanity check: Required still works on Claude
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        let p = build(&claude_cfg(), &req).unwrap();
        let tc = p.tool_config.as_ref().unwrap();
        assert!(tc.tool_choice().unwrap().is_any());
    }

    // ── Fix B tests: validate forced tool choices ─────────────────────────────

    #[test]
    fn forced_tool_choice_tool_missing_tool_name_returns_error() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "nonexistent".to_owned(),
        });
        let err = build(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn forced_tool_choice_required_no_tools_returns_error() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        // No tools added, ToolChoice::Required on supported family
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        let err = build(&claude_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
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
            let sdk_cfg = aws_config::defaults(aws_config::BehaviorVersion::v2026_11_17())
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
            native_tool_types: true,
        };

        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        req.model_settings.max_output_tokens = Some(256);
        let p = build(&cfg_with_default, &req).unwrap();
        let ic = p
            .inference_config
            .as_ref()
            .expect("inferenceConfig present");
        assert_eq!(ic.max_tokens(), Some(256));
    }

    // ── Native path (SMA-839) ─────────────────────────────────────────────────

    fn transaction_format() -> ResponseFormat {
        ResponseFormat::JsonSchema {
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
        }
    }

    fn search_tool() -> ToolDef {
        ToolDef::new(
            "search",
            "Search",
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        )
    }

    fn get_balance_tool() -> ToolDef {
        ToolDef::new(
            "get_balance",
            "Get account balance",
            json!({"type": "object", "properties": {"account_id": {"type": "string"}}}),
        )
    }

    /// A user turn, one tool call and its result.
    fn tool_round_history() -> Vec<Item> {
        vec![
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
        ]
    }

    fn tool_names(w: &serde_json::Value) -> Vec<String> {
        w["toolConfig"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["toolSpec"]["name"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn json_schema_on_global_sonnet_5_5_uses_native_output() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("make a person")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Person".to_owned(),
            schema: json!({"type": "object"}),
            strict: false,
        });
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert!(p.tool_config.is_none());
        assert!(!p.synthesizing);
        let js = p
            .output_config
            .as_ref()
            .unwrap()
            .text_format()
            .unwrap()
            .structure()
            .unwrap()
            .as_json_schema()
            .unwrap();
        assert_eq!(js.name(), Some("Person"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(js.schema()).unwrap(),
            json!({"type": "object", "additionalProperties": false})
        );
    }

    #[test]
    fn snap_native_json_schema() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("Extract the transaction data.")];
        req.model_settings.response_format = Some(transaction_format());
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert!(!p.synthesizing);
        let w = to_wire_json(&p);
        assert!(w["toolConfig"].is_null());
        assert_eq!(w["outputConfig"]["textFormat"]["type"], "json_schema");
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_native_json_schema_finalizing_turn_anchor() {
        let mut req = ModelRequest::new();
        req.messages = tool_round_history();
        req.model_settings.response_format = Some(transaction_format());
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert!(p.synthesizing);
        let w = to_wire_json(&p);
        assert_eq!(tool_names(&w), vec![SYNTHESIZED_TOOL_NAME.to_owned()]);
        assert!(w["toolConfig"]["toolChoice"].is_null());
        assert_eq!(w["outputConfig"]["textFormat"]["type"], "json_schema");
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_native_json_object_degrades_to_text() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("Give me a JSON object.")];
        req.model_settings.response_format = Some(ResponseFormat::JsonObject);
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert!(!p.synthesizing);
        assert_eq!(p.notices, vec![NativeNotice::JsonObjectAsText]);
        let w = to_wire_json(&p);
        assert!(w.get("outputConfig").is_none());
        assert!(w["toolConfig"].is_null());
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_native_json_object_finalizing_turn_anchor() {
        // Ruling P6: tool history + no user tools → the synthesized tool is the anchor.
        let mut req = ModelRequest::new();
        req.messages = tool_round_history();
        req.model_settings.response_format = Some(ResponseFormat::JsonObject);
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert!(p.synthesizing);
        assert_eq!(p.notices, vec![NativeNotice::JsonObjectAsText]);
        let w = to_wire_json(&p);
        assert!(w.get("outputConfig").is_none());
        assert_eq!(tool_names(&w), vec![SYNTHESIZED_TOOL_NAME.to_owned()]);
        assert!(w["toolConfig"]["toolChoice"].is_null());
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_native_tool_choice_required_omitted() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![search_tool()];
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert_eq!(p.notices, vec![NativeNotice::RequiredOmitted]);
        let w = to_wire_json(&p);
        assert!(w["toolConfig"]["toolChoice"].is_null());
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_native_tool_choice_tool_omitted() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![search_tool()];
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "search".to_owned(),
        });
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert_eq!(p.notices, vec![NativeNotice::ToolOmitted]);
        let w = to_wire_json(&p);
        assert!(w["toolConfig"]["toolChoice"].is_null());
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_native_tool_choice_auto() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![search_tool()];
        req.model_settings.tool_choice = Some(ToolChoice::Auto);
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert!(p.notices.is_empty());
        let w = to_wire_json(&p);
        assert_eq!(w["toolConfig"]["toolChoice"], json!({"auto": {}}));
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_native_tool_choice_none_with_json_schema() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("Extract the transaction data.")];
        req.tools = vec![search_tool()];
        req.model_settings.tool_choice = Some(ToolChoice::None);
        req.model_settings.response_format = Some(transaction_format());
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert!(!p.synthesizing);
        let w = to_wire_json(&p);
        assert!(w["toolConfig"].is_null());
        assert_eq!(w["outputConfig"]["textFormat"]["type"], "json_schema");
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_native_tool_choice_none_json_schema_finalizing_anchor() {
        let mut req = ModelRequest::new();
        req.messages = tool_round_history();
        req.tools = vec![get_balance_tool()];
        req.model_settings.tool_choice = Some(ToolChoice::None);
        req.model_settings.response_format = Some(transaction_format());
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert!(p.synthesizing);
        assert!(!none_choice_of(&p));
        let w = to_wire_json(&p);
        assert_eq!(tool_names(&w), vec![SYNTHESIZED_TOOL_NAME.to_owned()]);
        assert!(w["toolConfig"]["toolChoice"].is_null());
        assert_eq!(w["outputConfig"]["textFormat"]["type"], "json_schema");
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_native_json_schema_plus_tool_choice_tool() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("Extract the transaction data.")];
        req.tools = vec![search_tool()];
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "search".to_owned(),
        });
        req.model_settings.response_format = Some(transaction_format());
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert_eq!(p.notices, vec![NativeNotice::ToolOmitted]);
        let w = to_wire_json(&p);
        assert!(w["toolConfig"]["toolChoice"].is_null());
        assert_eq!(w["outputConfig"]["textFormat"]["type"], "json_schema");
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_native_json_schema_plus_required() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("Extract the transaction data.")];
        req.tools = vec![search_tool()];
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        req.model_settings.response_format = Some(transaction_format());
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert_eq!(p.notices, vec![NativeNotice::RequiredOmitted]);
        let w = to_wire_json(&p);
        assert!(w["toolConfig"]["toolChoice"].is_null());
        assert_eq!(w["outputConfig"]["textFormat"]["type"], "json_schema");
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_native_user_tools_plus_json_schema() {
        let mut req = ModelRequest::new();
        req.messages = tool_round_history();
        req.tools = vec![get_balance_tool()];
        req.model_settings.response_format = Some(transaction_format());
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert!(!p.synthesizing);
        let w = to_wire_json(&p);
        assert_eq!(tool_names(&w), vec!["get_balance".to_owned()]);
        assert_eq!(w["outputConfig"]["textFormat"]["type"], "json_schema");
        assert_json_snapshot!(w);
    }

    #[test]
    fn native_tool_choice_required_no_tools_is_error() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        let err = build(&sonnet55_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
        assert!(
            err.to_string().contains("requires at least one tool"),
            "{err}"
        );
    }

    #[test]
    fn native_tool_choice_required_json_object_no_tools_is_omitted() {
        // Ruling P7: no error; the choice is omitted with a notice.
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        req.model_settings.response_format = Some(ResponseFormat::JsonObject);
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert_no_forced_choice(&p);
        assert!(p.tool_config.is_none());
        assert!(p.output_config.is_none());
        assert!(!p.synthesizing);
        assert_eq!(
            p.notices,
            vec![
                NativeNotice::JsonObjectAsText,
                NativeNotice::RequiredOmitted
            ]
        );
    }

    #[test]
    fn native_tool_choice_tool_unknown_name_is_error() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![search_tool()];
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "missing".to_owned(),
        });
        let err = build(&sonnet55_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
        assert!(
            err.to_string().contains("not defined in the tool list"),
            "{err}"
        );
    }

    #[test]
    fn native_tool_choice_tool_cannot_select_the_anchor() {
        // The anchor is not a user tool. Selecting it by name is an error, also
        // when the history has a tool round and the anchor is sent.
        let mut req = ModelRequest::new();
        req.messages = tool_round_history();
        req.model_settings.response_format = Some(transaction_format());
        req.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: SYNTHESIZED_TOOL_NAME.to_owned(),
        });
        let err = build(&sonnet55_cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
        assert!(
            err.to_string().contains("not defined in the tool list"),
            "{err}"
        );
    }

    #[test]
    fn native_path_reports_no_notices_for_plain_requests() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hello")];
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert!(p.notices.is_empty());
        assert!(p.output_config.is_none());
        assert!(p.tool_config.is_none());
        assert!(!p.synthesizing);
    }

    #[test]
    fn forced_path_reports_no_output_config_or_notices() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("Extract the transaction data.")];
        req.model_settings.response_format = Some(transaction_format());
        let p = build(&claude_cfg(), &req).unwrap();
        assert!(p.output_config.is_none());
        assert!(p.notices.is_empty());
        assert!(p.has_forced_tool_choice());
    }

    fn assert_forced_path_kept(model_id: &str) -> serde_json::Value {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("Extract the transaction data.")];
        req.model_settings.response_format = Some(transaction_format());
        let p = build(&make_cfg(model_id), &req).unwrap();
        let w = to_wire_json(&p);
        assert_eq!(
            w["toolConfig"]["toolChoice"],
            json!({"tool": {"name": "__paigasus_structured_output__"}})
        );
        assert!(w.get("outputConfig").is_none());
        w
    }

    #[test]
    fn snap_forced_path_kept_on_sonnet_5() {
        assert_json_snapshot!(assert_forced_path_kept("global.anthropic.claude-sonnet-5"));
    }

    #[test]
    fn snap_forced_path_kept_on_haiku_5_5() {
        assert_json_snapshot!(assert_forced_path_kept("global.anthropic.claude-haiku-5-5"));
    }

    // ── ToolChoice::None with a tool history (SMA-833) ────────────────────────

    fn nova_cfg() -> Config {
        make_cfg("global.amazon.nova-2-lite-v1:0")
    }

    fn haiku55_cfg() -> Config {
        make_cfg("global.anthropic.claude-haiku-5-5")
    }

    fn none_after_tool_round() -> ModelRequest {
        let mut req = ModelRequest::new();
        req.messages = tool_round_history();
        req.tools = vec![get_balance_tool()];
        req.model_settings.tool_choice = Some(ToolChoice::None);
        req
    }

    #[test]
    fn snap_forced_path_tool_choice_none_after_tool_round_on_claude() {
        let p = build(&haiku55_cfg(), &none_after_tool_round()).unwrap();
        assert!(none_choice_of(&p));
        assert!(!p.synthesizing);
        let w = to_wire_json(&p);
        assert_eq!(tool_names(&w), vec!["get_balance".to_owned()]);
        assert!(w["toolConfig"]["toolChoice"].is_null());
        assert_eq!(
            w["additionalModelRequestFields"],
            json!({"tool_choice": {"type": "none"}})
        );
        assert_json_snapshot!(w);
    }

    #[test]
    fn snap_tool_choice_none_after_tool_round_on_nova() {
        let p = build(&nova_cfg(), &none_after_tool_round()).unwrap();
        assert!(!none_choice_of(&p));
        let w = to_wire_json(&p);
        assert!(w["toolConfig"].is_null());
        assert!(w.get("additionalModelRequestFields").is_none());
        let texts: Vec<String> = w["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["content"].as_array().unwrap().iter())
            .filter_map(|b| b["text"].as_str().map(str::to_owned))
            .collect();
        assert!(
            texts.contains(&r#"[tool call tu_1: get_balance {"account_id":"acc_123"}]"#.to_owned())
        );
        assert!(texts.contains(&r#"[tool result tu_1: "$1,234.56"]"#.to_owned()));
        assert_json_snapshot!(w);
    }

    #[test]
    fn native_tool_choice_none_after_tool_round_on_claude() {
        let p = build(&sonnet55_cfg(), &none_after_tool_round()).unwrap();
        assert!(none_choice_of(&p));
        assert!(p.output_config.is_none());
        let w = to_wire_json(&p);
        assert_eq!(tool_names(&w), vec!["get_balance".to_owned()]);
        assert!(w.get("outputConfig").is_none());
        assert!(w["toolConfig"]["toolChoice"].is_null());
    }

    #[test]
    fn native_json_object_tool_choice_none_keeps_anchor() {
        let mut req = none_after_tool_round();
        req.model_settings.response_format = Some(ResponseFormat::JsonObject);
        let p = build(&sonnet55_cfg(), &req).unwrap();
        assert!(p.synthesizing);
        assert!(!none_choice_of(&p));
        let w = to_wire_json(&p);
        assert_eq!(tool_names(&w), vec![SYNTHESIZED_TOOL_NAME.to_owned()]);
    }

    #[test]
    fn native_text_history_through_build_native_on_nova() {
        let p = native::build_native(&nova_cfg(), &none_after_tool_round()).unwrap();
        assert!(!none_choice_of(&p));
        let w = to_wire_json(&p);
        assert!(w["toolConfig"].is_null());
        assert!(!crate::translate::tool_choice_none::has_tool_round(
            &p.messages
        ));
    }

    #[test]
    fn none_shape_is_the_same_on_both_paths() {
        let req = none_after_tool_round();
        let cfg = haiku55_cfg();
        let forced = to_wire_json(&build_request(&cfg, &req, OutputPath::ForcedTool).unwrap());
        let native = to_wire_json(&build_request(&cfg, &req, OutputPath::Native).unwrap());
        assert_eq!(forced["toolConfig"], native["toolConfig"]);
        assert_eq!(
            forced["additionalModelRequestFields"],
            native["additionalModelRequestFields"]
        );
    }

    #[test]
    fn none_shape_is_the_same_on_both_paths_with_typed_tool() {
        let mut req = ModelRequest::new();
        req.messages = vec![
            user_text("remember this"),
            Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "tu_1".to_owned(),
                    name: "memory".to_owned(),
                    args: json!({"command": "view", "path": "/memories"}),
                }],
                agent: None,
            },
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![ContentPart::Text {
                    text: "empty".to_owned(),
                }],
            },
        ];
        req.tools = vec![ToolDef::new("memory", "memory", json!({"type": "object"}))
            .with_provider_types(vec![paigasus_helikon_core::ProviderToolType::new(
                "anthropic",
                "memory_20250818",
            )])];
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let cfg = haiku55_cfg();
        let forced = to_wire_json(&build_request(&cfg, &req, OutputPath::ForcedTool).unwrap());
        let native = to_wire_json(&build_request(&cfg, &req, OutputPath::Native).unwrap());
        assert_eq!(forced["toolConfig"], native["toolConfig"]);
        assert_eq!(
            forced["additionalModelRequestFields"],
            native["additionalModelRequestFields"]
        );
        assert_eq!(
            tool_names(&forced),
            vec!["__paigasus_structured_output__".to_owned()]
        );
        assert_eq!(
            forced["additionalModelRequestFields"],
            json!({
                "tools": [{"type": "memory_20250818", "name": "memory"}],
                "tool_choice": {"type": "none"}
            })
        );
    }

    #[test]
    fn none_without_tool_round_keeps_shape_on_nova() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![get_balance_tool()];
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let p = build(&nova_cfg(), &req).unwrap();
        assert!(!none_choice_of(&p));
        assert!(to_wire_json(&p)["toolConfig"].is_null());
    }

    #[test]
    fn other_tool_choices_never_set_the_flag() {
        let choices = [
            None,
            Some(ToolChoice::Auto),
            Some(ToolChoice::Required),
            Some(ToolChoice::Tool {
                name: "get_balance".to_owned(),
            }),
        ];
        for cfg in [claude_cfg(), sonnet55_cfg()] {
            for tc in &choices {
                let mut req = none_after_tool_round();
                req.model_settings.tool_choice = tc.clone();
                let p = build(&cfg, &req).unwrap();
                assert!(!none_choice_of(&p), "{tc:?}");
            }
        }
    }
}
