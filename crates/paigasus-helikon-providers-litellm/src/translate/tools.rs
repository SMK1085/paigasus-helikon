//! `ToolDef` → LiteLLM `tools[]`, and `ToolChoice` → `tool_choice`.
//!
//! Schema normalisation delegates to [`paigasus_helikon_core::schema::strict`],
//! the canonical normaliser the OpenAI provider also uses — no schema logic is
//! duplicated here.

use paigasus_helikon_core::{ToolChoice, ToolDef};
use serde_json::{json, Value};

/// Translate tool definitions into the OpenAI `tools` array shape.
pub(crate) fn to_tools(defs: &[ToolDef]) -> Value {
    Value::Array(
        defs.iter()
            .map(|d| {
                json!({
                    "type": "function",
                    "function": {
                        "name": d.name,
                        "description": d.description,
                        "parameters": paigasus_helikon_core::schema::strict(&d.schema),
                    }
                })
            })
            .collect(),
    )
}

/// Translate the caller's tool-selection preference.
pub(crate) fn to_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::Required => json!("required"),
        ToolChoice::None => json!("none"),
        ToolChoice::Tool { name } => json!({"type": "function", "function": {"name": name}}),
        // ToolChoice is #[non_exhaustive]; unknown future variants fall back to Auto.
        _ => json!("auto"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> ToolDef {
        ToolDef {
            name: "get_weather".into(),
            description: "Look up weather".into(),
            schema: json!({
                "type": "object",
                "properties": {"city": {"type": "string"}}
            }),
        }
    }

    #[test]
    fn tool_def_becomes_a_function_entry() {
        let v = to_tools(&[def()]);
        assert_eq!(v[0]["type"], "function");
        assert_eq!(v[0]["function"]["name"], "get_weather");
        assert_eq!(v[0]["function"]["description"], "Look up weather");
    }

    #[test]
    fn tool_schema_runs_through_the_strict_normaliser() {
        let v = to_tools(&[def()]);
        let params = &v[0]["function"]["parameters"];
        assert_eq!(params["additionalProperties"], json!(false));
        assert_eq!(params["required"], json!(["city"]));
    }

    #[test]
    fn empty_defs_produce_an_empty_array() {
        assert_eq!(to_tools(&[]), json!([]));
    }

    #[test]
    fn tool_choice_variants_map_correctly() {
        assert_eq!(to_tool_choice(&ToolChoice::Auto), json!("auto"));
        assert_eq!(to_tool_choice(&ToolChoice::Required), json!("required"));
        assert_eq!(to_tool_choice(&ToolChoice::None), json!("none"));
        assert_eq!(
            to_tool_choice(&ToolChoice::Tool { name: "f".into() }),
            json!({"type": "function", "function": {"name": "f"}})
        );
    }
}
