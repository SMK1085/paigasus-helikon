//! Translate [`paigasus_helikon_core::ResponseFormat`] into the JSON shape
//! async-openai accepts on Chat Completions / Responses request bodies.

use crate::translate::tools::to_strict_schema;
use paigasus_helikon_core::ResponseFormat;
use serde_json::{json, Value};

/// Translate to the JSON shape async-openai's request body accepts.
///
/// Returns `None` for [`ResponseFormat::Text`] — callers omit the
/// `response_format` field entirely in that case (matching OpenAI's
/// "no constraint" semantics).
pub(crate) fn to_openai_response_format(format: &ResponseFormat) -> Option<Value> {
    match format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject => Some(json!({"type": "json_object"})),
        ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => {
            let schema = if *strict {
                to_strict_schema(schema)
            } else {
                schema.clone()
            };
            Some(json!({
                "type": "json_schema",
                "json_schema": {
                    "name": name,
                    "schema": schema,
                    "strict": *strict,
                }
            }))
        }
        // Future variants from #[non_exhaustive]; default to "no constraint".
        _ => None,
    }
}

// Silence dead-code warnings until the backend modules (E1+/F1+) consume this.
#[allow(dead_code)]
const _SILENCE_DEAD_CODE: fn(&ResponseFormat) -> Option<Value> = to_openai_response_format;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_returns_none() {
        assert!(to_openai_response_format(&ResponseFormat::Text).is_none());
    }

    #[test]
    fn json_object_returns_json_object_shape() {
        let out = to_openai_response_format(&ResponseFormat::JsonObject).unwrap();
        assert_eq!(out, json!({"type": "json_object"}));
    }

    #[test]
    fn json_schema_strict_runs_through_strict_rewriter() {
        let schema = json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}}
        });
        let fmt = ResponseFormat::JsonSchema {
            name: "Answer".to_owned(),
            schema,
            strict: true,
        };
        let out = to_openai_response_format(&fmt).unwrap();
        assert_eq!(out["type"], "json_schema");
        assert_eq!(out["json_schema"]["name"], "Answer");
        assert_eq!(out["json_schema"]["strict"], true);
        assert_eq!(out["json_schema"]["schema"]["additionalProperties"], false);
        assert_eq!(
            out["json_schema"]["schema"]["required"].as_array().unwrap(),
            &vec![json!("answer")]
        );
    }

    #[test]
    fn json_schema_non_strict_passes_schema_through_untouched() {
        let schema = json!({"type": "object", "properties": {"k": {"type": "string"}}});
        let expected_schema = schema.clone();
        let fmt = ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema,
            strict: false,
        };
        let out = to_openai_response_format(&fmt).unwrap();
        assert_eq!(out["json_schema"]["schema"], expected_schema);
        assert_eq!(out["json_schema"]["strict"], false);
    }
}
