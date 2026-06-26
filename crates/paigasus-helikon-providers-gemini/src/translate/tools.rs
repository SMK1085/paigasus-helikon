//! Translate core tool defs + tool choice into Gemini `tools` / `toolConfig`.

use paigasus_helikon_core::{ToolChoice, ToolDef};
use serde_json::{json, Value};

use super::schema::sanitize_schema;

/// Build the Gemini `tools` array (empty when there are no tool defs).
pub(crate) fn function_declarations(defs: &[ToolDef]) -> Vec<Value> {
    if defs.is_empty() {
        return Vec::new();
    }
    let decls: Vec<Value> = defs
        .iter()
        .map(|d| {
            json!({
                "name": d.name,
                "description": d.description,
                "parameters": sanitize_schema(&d.schema),
            })
        })
        .collect();
    vec![json!({ "functionDeclarations": decls })]
}

/// Build `toolConfig.functionCallingConfig`, or `None` when no choice is set.
pub(crate) fn function_calling_config(
    choice: Option<&ToolChoice>,
    all_names: &[String],
) -> Option<Value> {
    let c = choice?;
    let v = match c {
        ToolChoice::Auto => json!({ "mode": "AUTO" }),
        ToolChoice::None => json!({ "mode": "NONE" }),
        ToolChoice::Required => json!({ "mode": "ANY", "allowedFunctionNames": all_names }),
        ToolChoice::Tool { name } => json!({ "mode": "ANY", "allowedFunctionNames": [name] }),
        // ToolChoice is #[non_exhaustive]; new variants default to Auto.
        _ => json!({ "mode": "AUTO" }),
    };
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs() -> Vec<ToolDef> {
        vec![ToolDef {
            name: "search".into(),
            description: "search the web".into(),
            schema: json!({ "type": "object", "properties": { "q": { "type": "string" } }, "additionalProperties": false }),
        }]
    }

    #[test]
    fn declarations_wrap_and_sanitize() {
        let out = function_declarations(&defs());
        let fd = &out[0]["functionDeclarations"][0];
        assert_eq!(fd["name"], "search");
        assert_eq!(fd["description"], "search the web");
        // additionalProperties stripped by sanitizer
        assert!(fd["parameters"].get("additionalProperties").is_none());
        assert_eq!(fd["parameters"]["properties"]["q"]["type"], "string");
    }

    #[test]
    fn no_tools_is_empty() {
        assert!(function_declarations(&[]).is_empty());
    }

    #[test]
    fn choice_modes() {
        let names = vec!["search".to_owned()];
        assert_eq!(
            function_calling_config(Some(&ToolChoice::Auto), &names).unwrap()["mode"],
            "AUTO"
        );
        assert_eq!(
            function_calling_config(Some(&ToolChoice::None), &names).unwrap()["mode"],
            "NONE"
        );
        let req = function_calling_config(Some(&ToolChoice::Required), &names).unwrap();
        assert_eq!(req["mode"], "ANY");
        assert_eq!(req["allowedFunctionNames"], json!(["search"]));
        let one = function_calling_config(
            Some(&ToolChoice::Tool {
                name: "search".into(),
            }),
            &names,
        )
        .unwrap();
        assert_eq!(one["mode"], "ANY");
        assert_eq!(one["allowedFunctionNames"], json!(["search"]));
    }

    #[test]
    fn no_choice_is_none() {
        assert!(function_calling_config(None, &[]).is_none());
    }
}
