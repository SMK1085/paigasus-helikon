//! The request on the native structured-output path.
//!
//! Some models reject a forced tool choice (`toolChoice: any` or `tool`). For
//! these models the provider sends a `JsonSchema` response format in
//! `outputConfig.textFormat` and never forces a tool call. Each change of
//! meaning is recorded as a [`NativeNotice`] on the result.

use aws_sdk_bedrockruntime::types::{
    AutoToolChoice, JsonSchemaDefinition, OutputConfig, OutputFormat, OutputFormatStructure,
    OutputFormatType, Tool, ToolChoice as BedrockToolChoice,
};
use paigasus_helikon_core::{ModelError, ModelRequest, ResponseFormat, ToolChoice};

use crate::builder::Config;
use crate::policy::NativeNotice;
use crate::translate::additional::AdditionalFields;
use crate::translate::native_schema::native_output_schema;
use crate::translate::request::items_to_messages;
use crate::translate::response_format::synthesize;
use crate::translate::schema::Ruleset;
use crate::translate::tool_choice_none::{apply_tool_choice_none, has_tool_round};
use crate::translate::tools::{partition_tools, placeholder_spec, ToolSet};
use crate::translate::{
    assemble_tool_config, build_inference_config, check_tool_name, required_without_tools_error,
    PreparedConverse,
};

/// Assemble a [`PreparedConverse`] for the native path.
///
/// - `JsonSchema` goes to `outputConfig`. The `strict` flag has no effect.
/// - `JsonObject` degrades to text, because native output cannot express a
///   free-form object.
/// - `ToolChoice::Required` and `ToolChoice::Tool` are omitted. `Auto` stays.
/// - `ToolChoice::None` without a structured format, after a tool round: the
///   family strategy of `apply_tool_choice_none` decides the tools and the
///   history.
/// - After a tool round with no user tools, the synthesized tool is sent as a
///   `toolConfig` anchor without a `toolChoice`, because Bedrock needs a
///   `toolConfig` when the history holds a tool call or a tool result.
///
/// # Errors
/// - Reserved tool name (`__paigasus_structured_output__` used by the caller).
/// - `ToolChoice::Required` with no tool and no structured response format.
/// - `ToolChoice::Tool` with a name that is not in the tool list.
/// - The message errors of `items_to_messages`.
/// - An SDK builder error.
pub(crate) fn build_native(
    cfg: &Config,
    req: &ModelRequest,
) -> Result<PreparedConverse, ModelError> {
    let family = cfg.family;
    let ruleset = Ruleset::for_family(family);
    let rf = req.model_settings.response_format.as_ref();
    let tool_choice = req.model_settings.tool_choice.as_ref();
    let mut notices = Vec::new();

    let mut translated = items_to_messages(&req.messages)?;

    // Response format.
    let structured = matches!(
        rf,
        Some(ResponseFormat::JsonSchema { .. } | ResponseFormat::JsonObject)
    );
    let output_config = match rf {
        Some(ResponseFormat::JsonSchema { name, schema, .. }) => {
            Some(json_schema_output_config(name, schema)?)
        }
        Some(ResponseFormat::JsonObject) => {
            notices.push(NativeNotice::JsonObjectAsText);
            None
        }
        _ => None,
    };

    // User tools. `ToolChoice::None` sends no typed tools. The family strategy
    // of `apply_tool_choice_none` decides the tools and the history after a
    // tool round without a structured format. A structured request keeps the
    // anchor below and never splits the tools, so a reserved name cannot fail
    // a path that sends no tools.
    let mut additional = AdditionalFields::default();
    let mut set = ToolSet {
        generic: Vec::new(),
        typed: Vec::new(),
    };
    let mut sent_typed = false;
    let mut all_tools: Vec<Tool> = if matches!(tool_choice, Some(ToolChoice::None)) {
        if structured {
            Vec::new()
        } else {
            let outcome = apply_tool_choice_none(
                std::mem::take(&mut translated.messages),
                &req.tools,
                ruleset,
                cfg.sends_typed_tools(),
                family,
            )?;
            translated.messages = outcome.messages;
            additional = outcome.additional;
            outcome.tools
        }
    } else {
        set = partition_tools(&req.tools, ruleset, cfg.sends_typed_tools())?;
        sent_typed = set.has_typed();
        if sent_typed {
            additional.tools = set.typed_entries();
        }
        set.generic.iter().cloned().map(Tool::ToolSpec).collect()
    };

    // Check a caller-selected tool name against the user tools only. The anchor
    // below must not make an undefined name valid.
    if let Some(ToolChoice::Tool { name }) = tool_choice {
        check_tool_name(&all_tools, &set, name)?;
    }

    // Anchor: a structured format, no user tool of any kind and a tool round in
    // the history. With a typed tool the placeholder fills `toolConfig`
    // instead, and the request does not synthesize.
    let mut synthesizing = false;
    if all_tools.is_empty() && has_tool_round(&translated.messages) {
        if sent_typed {
            all_tools.push(Tool::ToolSpec(placeholder_spec()?));
        } else if structured {
            if let Some(anchor) = synthesize(rf, family, ruleset)?.tool {
                all_tools.push(Tool::ToolSpec(anchor));
                synthesizing = true;
            }
        }
    }

    // Tool choice. The native path never forces a tool call.
    let choice = match tool_choice {
        None | Some(ToolChoice::None) => None,
        Some(ToolChoice::Auto) if sent_typed => None,
        Some(ToolChoice::Auto) => Some(BedrockToolChoice::Auto(AutoToolChoice::builder().build())),
        Some(ToolChoice::Required) => {
            if all_tools.is_empty() && !set.has_any() && !structured {
                return Err(required_without_tools_error());
            }
            notices.push(NativeNotice::RequiredOmitted);
            None
        }
        Some(ToolChoice::Tool { .. }) => {
            notices.push(NativeNotice::ToolOmitted);
            None
        }
        Some(_) => {
            tracing::debug!(
                target: "paigasus::bedrock::translate",
                "unknown ToolChoice variant; omitting",
            );
            None
        }
    };

    let tool_config = assemble_tool_config(all_tools, choice)?;
    let inference_config = build_inference_config(req, cfg);

    Ok(PreparedConverse {
        model_id: cfg.model_id.clone(),
        system: translated.system,
        messages: translated.messages,
        tool_config,
        inference_config,
        synthesizing,
        output_config,
        notices,
        additional,
    })
}

/// Make the `outputConfig` for a `JsonSchema` response format.
fn json_schema_output_config(
    name: &str,
    schema: &serde_json::Value,
) -> Result<OutputConfig, ModelError> {
    let builder_error = |e| {
        ModelError::Other(anyhow::anyhow!(
            "failed to build the native output config: {e}"
        ))
    };
    let schema = serde_json::to_string(&native_output_schema(schema)).map_err(|e| {
        ModelError::Other(anyhow::anyhow!(
            "failed to serialize the native output schema: {e}"
        ))
    })?;
    let definition = JsonSchemaDefinition::builder()
        .schema(schema)
        .name(name)
        .build()
        .map_err(builder_error)?;
    let format = OutputFormat::builder()
        .r#type(OutputFormatType::JsonSchema)
        .structure(OutputFormatStructure::JsonSchema(definition))
        .build()
        .map_err(builder_error)?;
    Ok(OutputConfig::builder().text_format(format).build())
}
