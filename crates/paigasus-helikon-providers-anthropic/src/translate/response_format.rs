//! `ResponseFormat` → synthesized forced tool + guards.

use paigasus_helikon_core::{ResponseFormat, ToolChoice, ToolDef};
use serde_json::{json, Value};

/// Reserved name for the synthesized output tool. Caller-provided tools
/// with this name are rejected by [`validate_tool_names`].
pub(crate) const SYNTHESIZED_TOOL_NAME: &str = "__paigasus_structured_output__";

/// Outcome of [`synthesize_for_response_format`].
pub(crate) struct Synthesized {
    /// Extra tool to append to the `tools` array.
    pub(crate) tool: Option<Value>,
    /// `tool_choice` value to write into the request body.
    pub(crate) tool_choice: Option<Value>,
    /// True when we synthesized — the stream translator uses this to
    /// remap the synthesized tool's `input_json_delta` events to `TokenDelta`.
    pub(crate) synthesizing: bool,
}

/// Reject user tools whose name collides with the reserved synthesized name.
///
/// Runs **regardless of `ResponseFormat`** — a stray collision pollutes
/// the request schema even when synthesis is inactive.
pub(crate) fn validate_tool_names(defs: &[ToolDef]) -> Result<(), String> {
    for d in defs {
        if d.name == SYNTHESIZED_TOOL_NAME {
            return Err(format!(
                "tool name '{SYNTHESIZED_TOOL_NAME}' is reserved by the Anthropic provider \
                 for structured-output synthesis",
            ));
        }
    }
    Ok(())
}

/// Reject (ResponseFormat::JsonSchema|JsonObject) + ToolChoice::Tool combinations.
pub(crate) fn validate_conflict(
    rf: Option<&ResponseFormat>,
    tc: Option<&ToolChoice>,
) -> Result<(), String> {
    let synthesizing = matches!(
        rf,
        Some(ResponseFormat::JsonSchema { .. }) | Some(ResponseFormat::JsonObject),
    );
    let forced_tool = matches!(tc, Some(ToolChoice::Tool { .. }));
    if synthesizing && forced_tool {
        return Err(
            "ResponseFormat::JsonSchema/JsonObject and ToolChoice::Tool are \
             mutually exclusive on Anthropic"
                .to_owned(),
        );
    }
    Ok(())
}

/// Build the synthesized tool + tool_choice value for the given response format.
/// Returns `Synthesized { synthesizing: false, .. }` for `Text` / `None`.
pub(crate) fn synthesize_for_response_format(rf: Option<&ResponseFormat>) -> Synthesized {
    match rf {
        Some(ResponseFormat::JsonSchema { name, schema, .. }) => Synthesized {
            tool: Some(json!({
                "name": SYNTHESIZED_TOOL_NAME,
                "description": format!("Return data matching the {name} schema."),
                "input_schema": schema,
            })),
            tool_choice: Some(json!({"type": "tool", "name": SYNTHESIZED_TOOL_NAME})),
            synthesizing: true,
        },
        Some(ResponseFormat::JsonObject) => Synthesized {
            tool: Some(json!({
                "name": SYNTHESIZED_TOOL_NAME,
                "description": "Return a JSON object.",
                "input_schema": {"type": "object"},
            })),
            tool_choice: Some(json!({"type": "tool", "name": SYNTHESIZED_TOOL_NAME})),
            synthesizing: true,
        },
        _ => Synthesized { tool: None, tool_choice: None, synthesizing: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_name_rejected_in_user_tools() {
        let defs = vec![ToolDef {
            name: SYNTHESIZED_TOOL_NAME.to_owned(),
            description: "x".to_owned(),
            schema: json!({}),
        }];
        let err = validate_tool_names(&defs).unwrap_err();
        assert!(err.contains("reserved"));
    }

    #[test]
    fn normal_tool_names_pass() {
        let defs = vec![ToolDef {
            name: "search".to_owned(),
            description: "x".to_owned(),
            schema: json!({}),
        }];
        assert!(validate_tool_names(&defs).is_ok());
    }

    #[test]
    fn json_schema_and_tool_choice_tool_conflict() {
        let rf = ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({}),
            strict: true,
        };
        let tc = ToolChoice::Tool { name: "search".to_owned() };
        assert!(validate_conflict(Some(&rf), Some(&tc)).is_err());
    }

    #[test]
    fn json_schema_with_no_tool_choice_passes() {
        let rf = ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({}),
            strict: true,
        };
        assert!(validate_conflict(Some(&rf), None).is_ok());
    }

    #[test]
    fn text_format_no_synthesis() {
        let s = synthesize_for_response_format(Some(&ResponseFormat::Text));
        assert!(!s.synthesizing);
        assert!(s.tool.is_none());
        assert!(s.tool_choice.is_none());
    }

    #[test]
    fn json_schema_produces_synthesized_tool() {
        let rf = ResponseFormat::JsonSchema {
            name: "Person".to_owned(),
            schema: json!({"type": "object"}),
            strict: false,
        };
        let s = synthesize_for_response_format(Some(&rf));
        assert!(s.synthesizing);
        let t = s.tool.unwrap();
        assert_eq!(t["name"], SYNTHESIZED_TOOL_NAME);
        assert!(t["description"].as_str().unwrap().contains("Person"));
        assert_eq!(t["input_schema"], json!({"type": "object"}));
        assert_eq!(
            s.tool_choice.unwrap(),
            json!({"type": "tool", "name": SYNTHESIZED_TOOL_NAME}),
        );
    }

    #[test]
    fn json_object_produces_synthesized_tool_with_object_schema() {
        let s = synthesize_for_response_format(Some(&ResponseFormat::JsonObject));
        assert!(s.synthesizing);
        assert_eq!(s.tool.unwrap()["input_schema"], json!({"type": "object"}));
    }
}
