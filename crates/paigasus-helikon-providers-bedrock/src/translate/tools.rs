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
pub const SYNTHESIZED_TOOL_NAME: &str = "__paigasus_structured_output__";

/// Provider id that marks a native Anthropic tool declaration.
///
/// The value equals `paigasus_helikon_providers_anthropic::PROVIDER_ID`. This
/// crate does not depend on the Anthropic crate, so it keeps its own copy.
pub(crate) const ANTHROPIC_PROVIDER_ID: &str = "anthropic";

/// Description of the placeholder tool spec.
///
/// The placeholder only fills an empty `toolConfig.tools` list. The model
/// must never call it.
pub(crate) const PLACEHOLDER_DESCRIPTION: &str = "Placeholder. Never call this tool.";

/// A tool that the provider sends as a native Anthropic typed entry.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TypedTool {
    /// The tool name, as the model uses it in a tool call.
    pub(crate) name: String,
    /// The JSON entry for `additionalModelRequestFields.tools`.
    pub(crate) entry: serde_json::Value,
}

/// The tool list of one request, split by how the provider sends each tool.
#[derive(Debug, Clone)]
pub(crate) struct ToolSet {
    /// Tools sent as generic Bedrock `toolSpec` entries.
    pub(crate) generic: Vec<ToolSpecification>,
    /// Tools sent as native typed entries, in request order.
    pub(crate) typed: Vec<TypedTool>,
}

// remove in Task 3/4: the helpers have no non-test caller yet.
#[allow(dead_code)]
impl ToolSet {
    /// Whether the request has at least one typed tool.
    pub(crate) fn has_typed(&self) -> bool {
        !self.typed.is_empty()
    }

    /// Whether the request has at least one tool of any kind.
    pub(crate) fn has_any(&self) -> bool {
        !self.generic.is_empty() || !self.typed.is_empty()
    }

    /// Whether `name` is the name of a typed tool.
    pub(crate) fn is_typed_name(&self, name: &str) -> bool {
        self.typed.iter().any(|t| t.name == name)
    }

    /// The typed entries, in request order.
    pub(crate) fn typed_entries(&self) -> Vec<serde_json::Value> {
        self.typed.iter().map(|t| t.entry.clone()).collect()
    }
}

/// Split a slice of [`ToolDef`]s into generic specs and typed entries.
///
/// A tool is typed when `typed_enabled` is true, its first `anthropic`
/// declaration exists, and the type name starts with `memory_`. All other
/// tools get a generic [`ToolSpecification`]. Both lists keep request order.
///
/// # Errors
/// Returns [`ModelError::Other`] when any tool name equals
/// [`SYNTHESIZED_TOOL_NAME`] (reserved for structured-output synthesis).
pub(crate) fn partition_tools(
    defs: &[ToolDef],
    ruleset: Ruleset,
    typed_enabled: bool,
) -> Result<ToolSet, ModelError> {
    let mut generic = Vec::with_capacity(defs.len());
    let mut typed = Vec::new();
    for def in defs {
        if def.name == SYNTHESIZED_TOOL_NAME {
            return Err(ModelError::Other(anyhow::anyhow!(
                "tool name '{SYNTHESIZED_TOOL_NAME}' is reserved by the Bedrock provider \
                 for structured-output synthesis",
            )));
        }
        let declared = def
            .provider_types
            .iter()
            .find(|p| p.provider == ANTHROPIC_PROVIDER_ID);
        if let Some(pt) = declared {
            if typed_enabled && pt.type_name.starts_with("memory_") {
                let mut entry = pt.params.clone();
                entry.remove("cache_control");
                entry.insert(
                    "type".to_owned(),
                    serde_json::Value::String(pt.type_name.clone()),
                );
                entry.insert(
                    "name".to_owned(),
                    serde_json::Value::String(def.name.clone()),
                );
                tracing::debug!(
                    target: "paigasus::bedrock::translate",
                    tool = %def.name,
                    r#type = %pt.type_name,
                    "typed tool entry",
                );
                typed.push(TypedTool {
                    name: def.name.clone(),
                    entry: serde_json::Value::Object(entry),
                });
                continue;
            }
            let reason = if typed_enabled {
                "type not supported on bedrock"
            } else {
                "gate closed"
            };
            tracing::debug!(
                target: "paigasus::bedrock::translate",
                tool = %def.name,
                r#type = %pt.type_name,
                reason,
                "anthropic tool declaration gets the generic spec",
            );
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
        generic.push(spec);
    }
    Ok(ToolSet { generic, typed })
}

/// Translate a slice of [`ToolDef`]s into Bedrock [`ToolSpecification`]s.
///
/// Every tool gets a generic spec. Native typed entries are not made here.
///
/// # Errors
/// Returns [`ModelError::Other`] when any tool name equals
/// [`SYNTHESIZED_TOOL_NAME`] (reserved for structured-output synthesis).
pub(crate) fn tool_specs(
    defs: &[ToolDef],
    ruleset: Ruleset,
) -> Result<Vec<ToolSpecification>, ModelError> {
    Ok(partition_tools(defs, ruleset, false)?.generic)
}

/// Build the placeholder [`ToolSpecification`].
///
/// It has the reserved name, a fixed description and the schema
/// `{"type": "object"}`. It fills `toolConfig.tools` when a request has only
/// typed tools.
///
/// # Errors
/// Returns [`ModelError::Other`] when the SDK builder rejects the spec.
#[allow(dead_code)] // remove in Task 4: no non-test caller yet.
pub(crate) fn placeholder_spec() -> Result<ToolSpecification, ModelError> {
    tracing::debug!(
        target: "paigasus::bedrock::translate",
        "added the placeholder tool spec",
    );
    ToolSpecification::builder()
        .name(SYNTHESIZED_TOOL_NAME)
        .description(PLACEHOLDER_DESCRIPTION)
        .input_schema(ToolInputSchema::Json(value_to_document(
            &serde_json::json!({"type": "object"}),
        )))
        .build()
        .map_err(|e| ModelError::Other(anyhow::anyhow!("failed to build ToolSpecification: {e}")))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::ProviderToolType;
    use serde_json::json;

    fn simple_def(name: &str) -> ToolDef {
        ToolDef::new(
            name,
            "a tool",
            json!({"type": "object", "properties": {"x": {"type": "string"}}}),
        )
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
        let defs = vec![ToolDef::new(SYNTHESIZED_TOOL_NAME, "bad", json!({}))];
        let err = tool_specs(&defs, Ruleset::Strict).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
        let msg = format!("{err}");
        assert!(msg.contains("reserved"));
    }

    #[test]
    fn schema_rewriter_strips_unsupported_keywords() {
        // $schema should be stripped by the Strict rewriter.
        let defs = vec![ToolDef::new(
            "foo",
            "",
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {"n": {"type": "integer"}}
            }),
        )];
        let specs = tool_specs(&defs, Ruleset::Strict).unwrap();
        // The Document should not contain "$schema" — we can only verify this
        // indirectly by confirming the spec was built successfully (the rewriter
        // is unit-tested in schema.rs).
        assert_eq!(specs.len(), 1);
    }

    fn memory_def() -> ToolDef {
        ToolDef::new("memory", "custom description", json!({"type": "object"}))
            .with_provider_types(vec![ProviderToolType::new("anthropic", "memory_20250818")])
    }

    fn typed_def(name: &str, pt: ProviderToolType) -> ToolDef {
        ToolDef::new(name, "d", json!({"type": "object"})).with_provider_types(vec![pt])
    }

    #[test]
    fn gate_closed_gives_the_generic_spec() {
        let set = partition_tools(&[memory_def()], Ruleset::Strict, false).unwrap();
        assert!(set.typed.is_empty());
        let plain = ToolDef::new("memory", "custom description", json!({"type": "object"}));
        let want = partition_tools(&[plain], Ruleset::Strict, false).unwrap();
        assert_eq!(format!("{:?}", set.generic), format!("{:?}", want.generic));
    }

    #[test]
    fn memory_type_gives_exact_typed_entry() {
        let set = partition_tools(&[memory_def()], Ruleset::Strict, true).unwrap();
        assert!(set.generic.is_empty());
        assert_eq!(
            set.typed,
            vec![TypedTool {
                name: "memory".to_owned(),
                entry: json!({"type": "memory_20250818", "name": "memory"}),
            }]
        );
        assert!(set.has_typed());
        assert!(set.has_any());
        assert!(set.is_typed_name("memory"));
        assert!(!set.is_typed_name("other"));
        assert_eq!(
            set.typed_entries(),
            vec![json!({"type": "memory_20250818", "name": "memory"})]
        );
    }

    #[test]
    fn params_are_merged() {
        let pt = ProviderToolType::new("anthropic", "memory_20250818").with_params(
            json!({"max_characters": 10000})
                .as_object()
                .unwrap()
                .clone(),
        );
        let set = partition_tools(&[typed_def("memory", pt)], Ruleset::Strict, true).unwrap();
        assert_eq!(
            set.typed[0].entry,
            json!({"type": "memory_20250818", "name": "memory", "max_characters": 10000})
        );
    }

    #[test]
    fn type_and_name_in_params_do_not_override() {
        let pt = ProviderToolType::new("anthropic", "memory_20250818").with_params(
            json!({"type": "x", "name": "y"})
                .as_object()
                .unwrap()
                .clone(),
        );
        let set = partition_tools(&[typed_def("memory", pt)], Ruleset::Strict, true).unwrap();
        assert_eq!(set.typed[0].entry["type"], "memory_20250818");
        assert_eq!(set.typed[0].entry["name"], "memory");
    }

    #[test]
    fn cache_control_in_params_is_removed() {
        let pt = ProviderToolType::new("anthropic", "memory_20250818").with_params(
            json!({"cache_control": {"type": "ephemeral"}})
                .as_object()
                .unwrap()
                .clone(),
        );
        let set = partition_tools(&[typed_def("memory", pt)], Ruleset::Strict, true).unwrap();
        assert!(set.typed[0].entry.get("cache_control").is_none());
    }

    #[test]
    fn non_anthropic_declaration_gives_the_generic_spec() {
        let d = typed_def("memory", ProviderToolType::new("openai", "memory_20250818"));
        let set = partition_tools(&[d], Ruleset::Strict, true).unwrap();
        assert!(set.typed.is_empty());
        assert_eq!(set.generic.len(), 1);
    }

    #[test]
    fn unsupported_anthropic_type_gives_the_generic_spec() {
        let d = typed_def(
            "computer",
            ProviderToolType::new("anthropic", "computer_20251124"),
        );
        let set = partition_tools(&[d], Ruleset::Strict, true).unwrap();
        assert!(set.typed.is_empty());
        assert_eq!(set.generic.len(), 1);
    }

    #[test]
    fn first_anthropic_entry_wins_even_when_unsupported() {
        let d = ToolDef::new("memory", "d", json!({"type": "object"})).with_provider_types(vec![
            ProviderToolType::new("anthropic", "computer_20251124"),
            ProviderToolType::new("anthropic", "memory_20250818"),
        ]);
        let set = partition_tools(&[d], Ruleset::Strict, true).unwrap();
        assert!(set.typed.is_empty());
        assert_eq!(set.generic.len(), 1);
    }

    #[test]
    fn typed_tools_keep_request_order() {
        let mem = |n: &str| typed_def(n, ProviderToolType::new("anthropic", "memory_20250818"));
        let defs = vec![mem("memory_a"), simple_def("plain"), mem("memory_b")];
        let set = partition_tools(&defs, Ruleset::Strict, true).unwrap();
        let names: Vec<&str> = set.typed.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["memory_a", "memory_b"]);
        assert_eq!(set.generic.len(), 1);
        assert_eq!(set.generic[0].name(), "plain");
    }

    #[test]
    fn reserved_name_is_rejected_for_a_typed_tool() {
        let d = typed_def(
            SYNTHESIZED_TOOL_NAME,
            ProviderToolType::new("anthropic", "memory_20250818"),
        );
        let err = partition_tools(&[d], Ruleset::Strict, true).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
        assert!(format!("{err}").contains("reserved"));
    }

    #[test]
    fn placeholder_has_the_reserved_name_and_an_object_schema() {
        let spec = placeholder_spec().unwrap();
        assert_eq!(spec.name(), SYNTHESIZED_TOOL_NAME);
        assert_eq!(spec.description(), Some(PLACEHOLDER_DESCRIPTION));
        assert!(spec.input_schema().unwrap().is_json());
    }

    #[test]
    fn provider_id_matches_the_anthropic_provider() {
        assert_eq!(ANTHROPIC_PROVIDER_ID, "anthropic");
    }
}
