//! `ResponseFormat` → synthesized forced-tool + tool_choice for Bedrock Converse.
//!
//! When the model family supports forced tool choice, `JsonSchema` / `JsonObject`
//! response formats are synthesized into a hidden tool named [`SYNTHESIZED_TOOL_NAME`]
//! with a matching `toolChoice: {tool: {name}}` that forces the model to call it.
//! Families that do NOT support forced tool choice (e.g. Llama, Titan) return
//! `synthesizing: false`; the caller degrades to plain text.

use aws_sdk_bedrockruntime::types::{
    SpecificToolChoice, ToolChoice as BedrockToolChoice, ToolInputSchema, ToolSpecification,
};
use paigasus_helikon_core::{ModelError, ResponseFormat};

use crate::document::value_to_document;
use crate::family::ModelFamily;
use crate::translate::schema::{rewrite_tool_schema, Ruleset};
use crate::translate::tools::SYNTHESIZED_TOOL_NAME;

/// Outcome of [`synthesize`].
pub(crate) struct Synthesized {
    /// Synthesized tool to append to the tool list (if any).
    pub(crate) tool: Option<ToolSpecification>,
    /// `toolChoice` to include in `ToolConfiguration` (if any).
    pub(crate) tool_choice: Option<BedrockToolChoice>,
    /// `true` when the stream translator should remap the synthesized tool's
    /// output to a `TokenDelta`.
    pub(crate) synthesizing: bool,
}

/// Build the synthesized tool + `toolChoice` for the given response format.
///
/// Returns `synthesizing: false` for:
/// - `ResponseFormat::Text` / `None` (no synthesis needed),
/// - any format on a family that does not support forced tool choice (degrades
///   to text).
///
/// # Errors
/// Currently infallible (reserved-name and conflict guards are in `mod.rs`
/// where `tool_choice` is also available).
pub(crate) fn synthesize(
    rf: Option<&ResponseFormat>,
    family: ModelFamily,
    ruleset: Ruleset,
) -> Result<Synthesized, ModelError> {
    let no_synthesis = Ok(Synthesized {
        tool: None,
        tool_choice: None,
        synthesizing: false,
    });

    match rf {
        Some(ResponseFormat::JsonSchema { name, schema, .. }) => {
            if !family.supports_forced_tool_choice() {
                tracing::debug!(
                    target: "paigasus::bedrock::translate",
                    family = ?family,
                    "ResponseFormat::JsonSchema not synthesized — family does not support \
                     forced tool choice; degrading to Text",
                );
                return no_synthesis;
            }
            let rewritten = rewrite_tool_schema(schema, ruleset);
            let doc = value_to_document(&rewritten);
            let spec = ToolSpecification::builder()
                .name(SYNTHESIZED_TOOL_NAME)
                .description(format!("Return data matching the {name} schema."))
                .input_schema(ToolInputSchema::Json(doc))
                .build()
                .map_err(|e| {
                    ModelError::Other(anyhow::anyhow!("failed to build synthesized tool: {e}"))
                })?;
            let tc = build_specific_tool_choice(SYNTHESIZED_TOOL_NAME)?;
            Ok(Synthesized {
                tool: Some(spec),
                tool_choice: Some(tc),
                synthesizing: true,
            })
        }

        Some(ResponseFormat::JsonObject) => {
            if !family.supports_forced_tool_choice() {
                tracing::debug!(
                    target: "paigasus::bedrock::translate",
                    family = ?family,
                    "ResponseFormat::JsonObject not synthesized — family does not support \
                     forced tool choice; degrading to Text",
                );
                return no_synthesis;
            }
            let schema = serde_json::json!({"type": "object"});
            let rewritten = rewrite_tool_schema(&schema, ruleset);
            let doc = value_to_document(&rewritten);
            let spec = ToolSpecification::builder()
                .name(SYNTHESIZED_TOOL_NAME)
                .description("Return a JSON object.")
                .input_schema(ToolInputSchema::Json(doc))
                .build()
                .map_err(|e| {
                    ModelError::Other(anyhow::anyhow!("failed to build synthesized tool: {e}"))
                })?;
            let tc = build_specific_tool_choice(SYNTHESIZED_TOOL_NAME)?;
            Ok(Synthesized {
                tool: Some(spec),
                tool_choice: Some(tc),
                synthesizing: true,
            })
        }

        _ => no_synthesis,
    }
}

fn build_specific_tool_choice(name: &str) -> Result<BedrockToolChoice, ModelError> {
    let specific = SpecificToolChoice::builder()
        .name(name)
        .build()
        .map_err(|e| {
            ModelError::Other(anyhow::anyhow!("failed to build SpecificToolChoice: {e}"))
        })?;
    Ok(BedrockToolChoice::Tool(specific))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::ResponseFormat;
    use serde_json::json;

    fn strict_ruleset() -> Ruleset {
        Ruleset::for_family(ModelFamily::Anthropic)
    }

    #[test]
    fn text_format_returns_no_synthesis() {
        let s = synthesize(
            Some(&ResponseFormat::Text),
            ModelFamily::Anthropic,
            strict_ruleset(),
        )
        .unwrap();
        assert!(!s.synthesizing);
        assert!(s.tool.is_none());
        assert!(s.tool_choice.is_none());
    }

    #[test]
    fn none_format_returns_no_synthesis() {
        let s = synthesize(None, ModelFamily::Anthropic, strict_ruleset()).unwrap();
        assert!(!s.synthesizing);
    }

    #[test]
    fn json_schema_on_anthropic_synthesizes_tool_and_tool_choice() {
        let rf = ResponseFormat::JsonSchema {
            name: "Person".to_owned(),
            schema: json!({"type": "object", "properties": {"name": {"type": "string"}}}),
            strict: false,
        };
        let s = synthesize(Some(&rf), ModelFamily::Anthropic, strict_ruleset()).unwrap();
        assert!(s.synthesizing);
        let tool = s.tool.unwrap();
        assert_eq!(tool.name(), SYNTHESIZED_TOOL_NAME);
        assert!(tool.description().unwrap().contains("Person"));
        assert!(tool.input_schema().unwrap().is_json());
        // tool_choice should be Tool variant
        let tc = s.tool_choice.unwrap();
        assert!(tc.is_tool());
        assert_eq!(tc.as_tool().unwrap().name(), SYNTHESIZED_TOOL_NAME);
    }

    #[test]
    fn json_schema_on_llama_returns_no_synthesis() {
        let rf = ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({"type": "object"}),
            strict: false,
        };
        let s = synthesize(Some(&rf), ModelFamily::Llama, strict_ruleset()).unwrap();
        assert!(!s.synthesizing);
        assert!(s.tool.is_none());
        assert!(s.tool_choice.is_none());
    }

    #[test]
    fn json_schema_on_titan_returns_no_synthesis() {
        let rf = ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({"type": "object"}),
            strict: false,
        };
        let s = synthesize(Some(&rf), ModelFamily::AmazonTitan, strict_ruleset()).unwrap();
        assert!(!s.synthesizing);
    }

    #[test]
    fn json_object_on_anthropic_synthesizes_with_object_schema() {
        let s = synthesize(
            Some(&ResponseFormat::JsonObject),
            ModelFamily::Anthropic,
            strict_ruleset(),
        )
        .unwrap();
        assert!(s.synthesizing);
        let tool = s.tool.unwrap();
        assert_eq!(tool.name(), SYNTHESIZED_TOOL_NAME);
        assert!(tool.input_schema().is_some());
    }

    #[test]
    fn json_object_on_nova_synthesizes() {
        let s = synthesize(
            Some(&ResponseFormat::JsonObject),
            ModelFamily::AmazonNova,
            strict_ruleset(),
        )
        .unwrap();
        assert!(s.synthesizing);
    }

    #[test]
    fn json_object_on_llama_returns_no_synthesis() {
        let s = synthesize(
            Some(&ResponseFormat::JsonObject),
            ModelFamily::Llama,
            strict_ruleset(),
        )
        .unwrap();
        assert!(!s.synthesizing);
    }
}
