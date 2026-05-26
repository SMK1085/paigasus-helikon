//! `ToolDef` → Anthropic tool entries for the request body.
//!
//! Anthropic accepts permissive schemas — no strict-mode rewriting.

use paigasus_helikon_core::ToolDef;
use serde_json::{json, Value};

/// Translate the request's tool list into Anthropic's `tools:` array.
/// Cache markers are applied by `translate::cache`, not here.
pub(crate) fn translate_tools(defs: &[ToolDef]) -> Value {
    let arr: Vec<Value> = defs
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.schema,
            })
        })
        .collect();
    Value::Array(arr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_empty_list_to_empty_array() {
        assert_eq!(translate_tools(&[]), json!([]));
    }

    #[test]
    fn passes_through_name_description_and_schema() {
        let defs = vec![ToolDef {
            name: "search".to_owned(),
            description: "search the web".to_owned(),
            schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        }];
        let out = translate_tools(&defs);
        assert_eq!(out[0]["name"], "search");
        assert_eq!(out[0]["description"], "search the web");
        assert_eq!(
            out[0]["input_schema"],
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        );
    }
}
