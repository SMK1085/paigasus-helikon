//! JSON Schema → OpenAI strict tool schema.
//!
//! OpenAI strict mode requires:
//! 1. `additionalProperties: false` on every object.
//! 2. Every property in `required` (no truly-optional fields — `Option<T>`
//!    must use `"type": ["T", "null"]` + present in `required`).
//!
//! `to_strict_schema` does (1) and (2). schemars 1.x emits `Option<T>` as
//! `"type": ["T", "null"]` natively (verified) so the proc-macro path
//! round-trips cleanly. Hand-authored `oneOf: [_, {type: "null"}]`
//! patterns are NOT collapsed — they pass through and may produce an
//! OpenAI strict-mode rejection (`ModelError::Other`). Deferred per YAGNI.

use serde_json::Value;

/// Rewrite a JSON Schema for OpenAI strict-mode tool calls.
///
/// Recursively:
/// 1. Sets `additionalProperties: false` on every object.
/// 2. Promotes every key in each object's `properties` into `required`.
/// 3. Recurses into object `properties` and array `items`.
///
/// Schemas that produce strict-mode rejections (hand-authored
/// `oneOf: [_, {type: "null"}]`, unsupported `pattern`, etc.) are passed
/// through unmodified — OpenAI surfaces the rejection at request time as
/// `ModelError::Other`.
pub(crate) fn to_strict_schema(value: &Value) -> Value {
    let mut out = value.clone();
    rewrite_in_place(&mut out);
    out
}

fn rewrite_in_place(v: &mut Value) {
    if let Some(obj) = v.as_object_mut() {
        let is_object_schema = obj
            .get("type")
            .and_then(|t| t.as_str())
            .map(|s| s == "object")
            .unwrap_or(false);

        if is_object_schema {
            obj.insert("additionalProperties".to_owned(), Value::Bool(false));

            if let Some(props) = obj.get("properties").and_then(|p| p.as_object()) {
                let keys: Vec<String> = props.keys().cloned().collect();
                let required = Value::Array(keys.into_iter().map(Value::String).collect());
                obj.insert("required".to_owned(), required);
            }
        }

        // Recurse into `properties` children.
        if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
            for (_, child) in props.iter_mut() {
                rewrite_in_place(child);
            }
        }
        // Recurse into array `items`.
        if let Some(items) = obj.get_mut("items") {
            rewrite_in_place(items);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flat_object_adds_additional_properties_false() {
        let input = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
            }
        });
        let out = to_strict_schema(&input);
        assert_eq!(out["additionalProperties"], json!(false));
    }

    #[test]
    fn flat_object_promotes_all_keys_into_required() {
        let input = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age":  {"type": "integer"},
            }
        });
        let out = to_strict_schema(&input);
        let req = out["required"].as_array().unwrap();
        let mut keys: Vec<&str> = req.iter().map(|v| v.as_str().unwrap()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["age", "name"]);
    }

    #[test]
    fn nested_object_gets_strict_treatment() {
        let input = json!({
            "type": "object",
            "properties": {
                "user": {
                    "type": "object",
                    "properties": {"id": {"type": "string"}}
                }
            }
        });
        let out = to_strict_schema(&input);
        assert_eq!(
            out["properties"]["user"]["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            out["properties"]["user"]["required"].as_array().unwrap(),
            &vec![json!("id")]
        );
    }

    #[test]
    fn array_of_objects_recurses_into_items() {
        let input = json!({
            "type": "object",
            "properties": {
                "tags": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"name": {"type": "string"}}
                    }
                }
            }
        });
        let out = to_strict_schema(&input);
        assert_eq!(
            out["properties"]["tags"]["items"]["additionalProperties"],
            json!(false)
        );
    }

    #[test]
    fn explicit_additional_properties_true_is_overridden_to_false() {
        let input = json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {"k": {"type": "string"}}
        });
        let out = to_strict_schema(&input);
        assert_eq!(out["additionalProperties"], json!(false));
    }

    #[test]
    fn option_t_emitted_as_type_array_is_preserved() {
        // Pins schemars 1.x's native Option<T> emission shape. If schemars
        // regresses to oneOf-style nullability, this test fails and we
        // revisit per the spec's deferred-YAGNI note.
        let input = json!({
            "type": "object",
            "properties": {
                "since": {"type": ["string", "null"]},
                "kind":  {"type": "string"},
            }
        });
        let out = to_strict_schema(&input);
        assert_eq!(
            out["properties"]["since"]["type"],
            json!(["string", "null"])
        );
        let mut req: Vec<String> = out["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        req.sort();
        assert_eq!(req, vec!["kind", "since"]);
    }

    #[test]
    fn snapshot_complex_tool_args() {
        let input = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "filters": {
                    "type": "object",
                    "properties": {
                        "since": {"type": ["string", "null"]},
                        "limit": {"type": "integer"},
                    }
                },
                "tags": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"name": {"type": "string"}}
                    }
                }
            }
        });
        let out = to_strict_schema(&input);
        insta::assert_json_snapshot!(out);
    }
}
