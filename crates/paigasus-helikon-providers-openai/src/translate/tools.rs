//! JSON Schema → OpenAI strict tool schema.
//!
//! Schema normalization is delegated to
//! [`paigasus_helikon_core::schema::strict`]; see that module for the full
//! algorithm description.

use serde_json::Value;

/// Rewrite a JSON Schema for OpenAI strict-mode tool calls.
///
/// Delegates to [`paigasus_helikon_core::schema::strict`], the canonical
/// normalizer. Kept as a crate-private alias so existing call sites and
/// tests are unaffected.
pub(crate) fn to_strict_schema(value: &Value) -> Value {
    paigasus_helikon_core::schema::strict(value)
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
