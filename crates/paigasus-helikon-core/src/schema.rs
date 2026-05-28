//! JSON Schema strict-mode normalization.
//!
//! [`strict`] rewrites a schemars-produced JSON Schema to satisfy
//! **OpenAI strict-mode / JSON-Schema** requirements:
//! 1. `additionalProperties: false` on every object.
//! 2. Every key in each object's `properties` promoted into `required`
//!    (no truly-optional fields — `Option<T>` must use `"type": ["T", "null"]`
//!    and stay present in `required`; schemars 1.x emits this natively).
//!
//! This is **not** a provider-neutral transform: it encodes OpenAI's
//! strict-mode quirks. Per-provider normalization for future providers
//! (Bedrock/Gemini, untagged-enum collapsing) is a separate concern.
//! The OpenAI provider calls this; Anthropic uses schemas as-is.

use serde_json::Value;

/// Rewrite a JSON Schema for OpenAI strict-mode structured output.
///
/// Recursively sets `additionalProperties: false` on every object,
/// promotes every key in each object's `properties` into `required`, and
/// recurses into object `properties` and array `items`. Schemas that
/// would produce strict-mode rejections (hand-authored
/// `oneOf: [_, {type: "null"}]`, unsupported `pattern`, etc.) pass
/// through unmodified.
///
/// **Limitation:** `$defs`/`$ref` are not traversed. schemars 1.x emits
/// `$defs` for enums, recursive types, and types referenced more than once;
/// object schemas nested under `$defs` therefore do not receive
/// `additionalProperties: false` or the `required` promotion. Adequate for
/// flat/nested struct outputs; revisit if `$defs`-bearing schemas need strict
/// mode.
pub fn strict(value: &Value) -> Value {
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

        if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
            for (_, child) in props.iter_mut() {
                rewrite_in_place(child);
            }
        }
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
        let input = json!({"type": "object", "properties": {"name": {"type": "string"}}});
        assert_eq!(strict(&input)["additionalProperties"], json!(false));
    }

    #[test]
    fn flat_object_promotes_all_keys_into_required() {
        let input = json!({
            "type": "object",
            "properties": {"name": {"type": "string"}, "age": {"type": "integer"}}
        });
        let out = strict(&input);
        let mut keys: Vec<&str> = out["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["age", "name"]);
    }

    #[test]
    fn nested_object_gets_strict_treatment() {
        let input = json!({
            "type": "object",
            "properties": {"user": {"type": "object", "properties": {"id": {"type": "string"}}}}
        });
        let out = strict(&input);
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
            "properties": {"tags": {"type": "array",
                "items": {"type": "object", "properties": {"name": {"type": "string"}}}}}
        });
        assert_eq!(
            strict(&input)["properties"]["tags"]["items"]["additionalProperties"],
            json!(false)
        );
    }

    #[test]
    fn explicit_additional_properties_true_is_overridden_to_false() {
        let input = json!({"type": "object", "additionalProperties": true, "properties": {"k": {"type": "string"}}});
        assert_eq!(strict(&input)["additionalProperties"], json!(false));
    }

    #[test]
    fn option_t_emitted_as_type_array_is_preserved() {
        let input = json!({
            "type": "object",
            "properties": {"since": {"type": ["string", "null"]}, "kind": {"type": "string"}}
        });
        let out = strict(&input);
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
}
