//! Native structured output (responseMimeType + responseSchema) + conflict guard.

use paigasus_helikon_core::{ModelError, ResponseFormat, ToolChoice, ToolDef};
use serde_json::Value;

use super::schema::sanitize_schema;

/// Map a `ResponseFormat` to `(responseMimeType, responseSchema?)`.
///
/// Returns `None` for [`ResponseFormat::Text`] and for `None` input — callers
/// omit the `responseMimeType`/`responseSchema` fields in the Gemini request.
pub(crate) fn response_format_fields(
    rf: Option<&ResponseFormat>,
) -> Option<(String, Option<Value>)> {
    match rf {
        Some(ResponseFormat::JsonObject) => Some(("application/json".to_owned(), None)),
        Some(ResponseFormat::JsonSchema { schema, .. }) => {
            Some(("application/json".to_owned(), Some(sanitize_schema(schema))))
        }
        // ResponseFormat is #[non_exhaustive]; treat unknown variants as Text (no directive).
        _ => None,
    }
}

/// Reject structured output combined with function calling.
///
/// Gemini does not support `responseSchema` together with tools. Inspects only
/// the active request (not history) so the loop's finalize-after-tool-use case
/// (tools empty, no active choice) is allowed.
pub(crate) fn validate_conflict(
    rf: Option<&ResponseFormat>,
    tools: &[ToolDef],
    choice: Option<&ToolChoice>,
) -> Result<(), ModelError> {
    let structured = matches!(
        rf,
        Some(ResponseFormat::JsonObject) | Some(ResponseFormat::JsonSchema { .. })
    );
    if !structured {
        return Ok(());
    }
    // ToolChoice::None and absent choice are both "inactive"; anything else is active.
    let active_choice = !matches!(choice, None | Some(ToolChoice::None));
    if !tools.is_empty() || active_choice {
        return Err(ModelError::Other(anyhow::anyhow!(
            "gemini does not support structured output (responseSchema) together with \
             function calling; omit tools / set ToolChoice::None"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_object_sets_mime_only() {
        let (mime, schema) = response_format_fields(Some(&ResponseFormat::JsonObject)).unwrap();
        assert_eq!(mime, "application/json");
        assert!(schema.is_none());
    }

    #[test]
    fn json_schema_sets_mime_and_sanitized_schema() {
        let rf = ResponseFormat::JsonSchema {
            name: "Out".into(),
            schema: json!({ "type": "object", "additionalProperties": false, "properties": {} }),
            strict: true,
        };
        let (mime, schema) = response_format_fields(Some(&rf)).unwrap();
        assert_eq!(mime, "application/json");
        assert!(schema.unwrap().get("additionalProperties").is_none());
    }

    #[test]
    fn text_and_none_produce_nothing() {
        assert!(response_format_fields(Some(&ResponseFormat::Text)).is_none());
        assert!(response_format_fields(None).is_none());
    }

    #[test]
    fn structured_output_with_tools_conflicts() {
        let rf = ResponseFormat::JsonObject;
        let tdef = vec![ToolDef {
            name: "t".into(),
            description: "".into(),
            schema: json!({}),
        }];
        assert!(validate_conflict(Some(&rf), &tdef, None).is_err());
    }

    #[test]
    fn structured_output_with_active_tool_choice_conflicts() {
        let rf = ResponseFormat::JsonObject;
        assert!(validate_conflict(Some(&rf), &[], Some(&ToolChoice::Auto)).is_err());
    }

    #[test]
    fn structured_output_with_choice_none_is_allowed() {
        let rf = ResponseFormat::JsonObject;
        assert!(validate_conflict(Some(&rf), &[], Some(&ToolChoice::None)).is_ok());
    }

    #[test]
    fn structured_output_no_tools_ok() {
        // The finalize-after-tool-use case: tools empty, history may contain function parts.
        assert!(validate_conflict(Some(&ResponseFormat::JsonObject), &[], None).is_ok());
    }
}
