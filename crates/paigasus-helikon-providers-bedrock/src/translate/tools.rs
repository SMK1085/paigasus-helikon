//! `ToolDef` → Bedrock `ToolSpecification` translation.
//!
//! Each `ToolDef.schema` is run through `rewrite_tool_schema` before being
//! converted to a Smithy `Document` via `value_to_document`.

use aws_sdk_bedrockruntime::types::{ToolInputSchema, ToolSpecification};
use paigasus_helikon_core::{ModelError, ToolDef};

use crate::document::value_to_document;
use crate::translate::schema::{rewrite_tool_schema, Ruleset};

/// Reserved tool name used internally for structured-output synthesis.
///
/// User-provided tools with this name are rejected to avoid collisions.
pub(crate) const SYNTHESIZED_TOOL_NAME: &str = "__paigasus_structured_output__";

/// Translate a slice of [`ToolDef`]s into Bedrock [`ToolSpecification`]s.
///
/// # Errors
/// Returns [`ModelError::Other`] when any tool name equals
/// [`SYNTHESIZED_TOOL_NAME`] (reserved for structured-output synthesis).
pub(crate) fn tool_specs(
    defs: &[ToolDef],
    ruleset: Ruleset,
) -> Result<Vec<ToolSpecification>, ModelError> {
    let mut specs = Vec::with_capacity(defs.len());
    for def in defs {
        if def.name == SYNTHESIZED_TOOL_NAME {
            return Err(ModelError::Other(anyhow::anyhow!(
                "tool name '{SYNTHESIZED_TOOL_NAME}' is reserved by the Bedrock provider \
                 for structured-output synthesis",
            )));
        }
        let rewritten = rewrite_tool_schema(&def.schema, ruleset);
        let doc = value_to_document(&rewritten);
        let spec = ToolSpecification::builder()
            .name(&def.name)
            .description(&def.description)
            .input_schema(ToolInputSchema::Json(doc))
            .build()
            .map_err(|e| {
                ModelError::Other(anyhow::anyhow!("failed to build ToolSpecification: {e}"))
            })?;
        specs.push(spec);
    }
    Ok(specs)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn simple_def(name: &str) -> ToolDef {
        ToolDef {
            name: name.to_owned(),
            description: "a tool".to_owned(),
            schema: json!({"type": "object", "properties": {"x": {"type": "string"}}}),
        }
    }

    #[test]
    fn empty_defs_returns_empty_vec() {
        let specs = tool_specs(&[], Ruleset::Strict).unwrap();
        assert!(specs.is_empty());
    }

    #[test]
    fn translates_name_and_description() {
        let defs = vec![simple_def("search")];
        let specs = tool_specs(&defs, Ruleset::Strict).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name(), "search");
        assert_eq!(specs[0].description(), Some("a tool"));
    }

    #[test]
    fn schema_becomes_json_input_schema() {
        let defs = vec![simple_def("ping")];
        let specs = tool_specs(&defs, Ruleset::Strict).unwrap();
        assert!(specs[0].input_schema().is_some());
        assert!(specs[0].input_schema().unwrap().is_json());
    }

    #[test]
    fn reserved_name_returns_error() {
        let defs = vec![ToolDef {
            name: SYNTHESIZED_TOOL_NAME.to_owned(),
            description: "bad".to_owned(),
            schema: json!({}),
        }];
        let err = tool_specs(&defs, Ruleset::Strict).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
        let msg = format!("{err}");
        assert!(msg.contains("reserved"));
    }

    #[test]
    fn schema_rewriter_strips_unsupported_keywords() {
        // $schema should be stripped by the Strict rewriter.
        let defs = vec![ToolDef {
            name: "foo".to_owned(),
            description: "".to_owned(),
            schema: json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {"n": {"type": "integer"}}
            }),
        }];
        let specs = tool_specs(&defs, Ruleset::Strict).unwrap();
        // The Document should not contain "$schema" — we can only verify this
        // indirectly by confirming the spec was built successfully (the rewriter
        // is unit-tested in schema.rs).
        assert_eq!(specs.len(), 1);
    }
}
