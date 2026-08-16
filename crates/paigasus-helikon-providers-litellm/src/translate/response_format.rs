//! `ResponseFormat` → LiteLLM `response_format`.

use paigasus_helikon_core::ResponseFormat;
use serde_json::{json, Value};

/// Translate the caller's response-shape preference.
///
/// Returns `None` for [`ResponseFormat::Text`] — callers omit the field
/// entirely, matching "no constraint" semantics.
pub(crate) fn to_response_format(format: &ResponseFormat) -> Option<Value> {
    match format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject => Some(json!({"type": "json_object"})),
        ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => {
            let schema = if *strict {
                paigasus_helikon_core::schema::strict(schema)
            } else {
                schema.clone()
            };
            Some(json!({
                "type": "json_schema",
                "json_schema": { "name": name, "schema": schema, "strict": *strict }
            }))
        }
        // ResponseFormat is #[non_exhaustive]; unknown future variants mean
        // "no constraint" rather than a hard error.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_omitted() {
        assert!(to_response_format(&ResponseFormat::Text).is_none());
    }

    #[test]
    fn json_object_maps_directly() {
        assert_eq!(
            to_response_format(&ResponseFormat::JsonObject).unwrap(),
            json!({"type": "json_object"})
        );
    }

    #[test]
    fn strict_json_schema_is_normalised() {
        let f = ResponseFormat::JsonSchema {
            name: "Answer".into(),
            schema: json!({"type": "object", "properties": {"a": {"type": "string"}}}),
            strict: true,
        };
        let v = to_response_format(&f).unwrap();
        assert_eq!(v["type"], "json_schema");
        assert_eq!(v["json_schema"]["name"], "Answer");
        assert_eq!(v["json_schema"]["strict"], true);
        assert_eq!(v["json_schema"]["schema"]["additionalProperties"], false);
    }

    #[test]
    fn non_strict_json_schema_passes_through_untouched() {
        let schema = json!({"type": "object", "properties": {"k": {"type": "string"}}});
        let f = ResponseFormat::JsonSchema {
            name: "X".into(),
            schema: schema.clone(),
            strict: false,
        };
        let v = to_response_format(&f).unwrap();
        assert_eq!(v["json_schema"]["schema"], schema);
        assert_eq!(v["json_schema"]["strict"], false);
    }
}
