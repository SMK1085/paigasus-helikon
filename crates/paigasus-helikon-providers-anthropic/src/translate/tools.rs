//! `ToolDef` → Anthropic tool entries for the request body.
//!
//! Anthropic accepts permissive schemas — no strict-mode rewriting.

use paigasus_helikon_core::ToolDef;
use serde_json::{json, Value};

use crate::PROVIDER_ID;

/// Translate the request's tool list into Anthropic's `tools:` array.
///
/// With `native_tool_types` on, a tool that declares a provider tool type for
/// [`PROVIDER_ID`] becomes the typed entry: its `params`, then `type` and
/// `name`. No description or schema is sent. Other tools get the generic
/// entry. Cache markers are applied by `translate::cache`, not here.
pub(crate) fn translate_tools(defs: &[ToolDef], native_tool_types: bool) -> Value {
    let arr: Vec<Value> = defs
        .iter()
        .map(|t| {
            let typed = if native_tool_types {
                t.provider_types.iter().find(|p| p.provider == PROVIDER_ID)
            } else {
                None
            };
            match typed {
                Some(pt) => {
                    let mut entry = pt.params.clone();
                    entry.remove("cache_control");
                    entry.insert("type".to_owned(), Value::String(pt.type_name.clone()));
                    entry.insert("name".to_owned(), Value::String(t.name.clone()));
                    tracing::debug!(
                        target: "paigasus::anthropic::translate",
                        tool = %t.name,
                        r#type = %pt.type_name,
                        "typed tool entry",
                    );
                    Value::Object(entry)
                }
                None => json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.schema,
                }),
            }
        })
        .collect();
    Value::Array(arr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::ProviderToolType;

    fn schema() -> Value {
        json!({"type":"object","properties":{"command":{"type":"string"}}})
    }

    fn memory_def() -> ToolDef {
        ToolDef::new("memory", "custom description", schema())
            .with_provider_types(vec![ProviderToolType::new("anthropic", "memory_20250818")])
    }

    fn generic_entry() -> Value {
        json!({"name":"memory","description":"custom description","input_schema":schema()})
    }

    fn with_params(params: Value) -> ToolDef {
        let map = params.as_object().unwrap().clone();
        ToolDef::new("memory", "custom description", schema()).with_provider_types(vec![
            ProviderToolType::new("anthropic", "memory_20250818").with_params(map),
        ])
    }

    #[test]
    fn memory_type_gives_exact_typed_entry() {
        assert_eq!(
            translate_tools(&[memory_def()], true)[0],
            json!({"type": "memory_20250818", "name": "memory"})
        );
    }

    #[test]
    fn params_are_merged() {
        let out = translate_tools(&[with_params(json!({"max_characters": 10000}))], true);
        assert_eq!(
            out[0],
            json!({"type":"memory_20250818","name":"memory","max_characters":10000})
        );
    }

    #[test]
    fn type_and_name_in_params_do_not_override() {
        let out = translate_tools(&[with_params(json!({"type":"x","name":"y"}))], true);
        assert_eq!(out[0]["type"], "memory_20250818");
        assert_eq!(out[0]["name"], "memory");
    }

    #[test]
    fn description_in_params_passes_through() {
        let out = translate_tools(&[with_params(json!({"description":"x"}))], true);
        assert_eq!(out[0]["description"], "x");
        assert_eq!(out[0]["type"], "memory_20250818");
        assert_eq!(out[0]["name"], "memory");
    }

    #[test]
    fn cache_control_in_params_is_removed() {
        let out = translate_tools(
            &[with_params(json!({"cache_control":{"type":"ephemeral"}}))],
            true,
        );
        assert!(out[0].get("cache_control").is_none());
    }

    #[test]
    fn non_anthropic_entry_gives_generic_entry() {
        let d = ToolDef::new("memory", "custom description", schema())
            .with_provider_types(vec![ProviderToolType::new("openai", "x")]);
        assert_eq!(translate_tools(&[d], true)[0], generic_entry());
    }

    #[test]
    fn provider_id_match_is_case_sensitive() {
        let d = ToolDef::new("memory", "custom description", schema())
            .with_provider_types(vec![ProviderToolType::new("Anthropic", "memory_20250818")]);
        assert_eq!(translate_tools(&[d], true)[0], generic_entry());
    }

    #[test]
    fn first_anthropic_entry_wins() {
        let d = ToolDef::new("memory", "custom description", schema()).with_provider_types(vec![
            ProviderToolType::new("anthropic", "memory_20250818"),
            ProviderToolType::new("anthropic", "bash_20250124"),
        ]);
        assert_eq!(translate_tools(&[d], true)[0]["type"], "memory_20250818");
    }

    #[test]
    fn opt_out_gives_generic_entry() {
        let out = translate_tools(&[memory_def()], false);
        assert!(out[0].get("input_schema").is_some());
        assert!(out[0].get("description").is_some());
        assert!(out[0].get("type").is_none());
    }

    #[test]
    fn typed_entry_and_generic_entries_mix() {
        let generic = ToolDef::new("search", "d", schema());
        let out = translate_tools(&[generic, memory_def()], true);
        assert!(out[0].get("input_schema").is_some());
        assert_eq!(out[1], json!({"type": "memory_20250818", "name": "memory"}));
    }

    #[test]
    fn translates_empty_list_to_empty_array() {
        assert_eq!(translate_tools(&[], true), json!([]));
    }

    #[test]
    fn passes_through_name_description_and_schema() {
        let defs = vec![ToolDef::new(
            "search",
            "search the web",
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        )];
        let out = translate_tools(&defs, true);
        assert_eq!(out[0]["name"], "search");
        assert_eq!(out[0]["description"], "search the web");
        assert_eq!(
            out[0]["input_schema"],
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        );
    }
}
