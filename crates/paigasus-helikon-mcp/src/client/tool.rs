//! `McpTool` — adapts a remote MCP tool to core's `Tool<Ctx>` trait.

use paigasus_helikon_core::{ToolError, ToolOutput};
use rmcp::model::CallToolResult;

/// Map an MCP `CallToolResult` into core's `ToolOutput`/`ToolError`.
///
/// - `is_error == Some(true)` → `ToolError::Other` carrying the text content.
/// - `structured_content` (when present) becomes `ToolOutput.content` as-is.
/// - Otherwise a single text content becomes a JSON string; anything else is
///   serialized as a JSON array of content blocks.
// used by McpTool (Task 4)
#[allow(dead_code)]
pub(crate) fn map_call_result(result: CallToolResult) -> Result<ToolOutput, ToolError> {
    if result.is_error == Some(true) {
        let msg = content_text(&result.content);
        return Err(ToolError::Other(anyhow::anyhow!(
            "MCP tool returned an error: {msg}"
        )));
    }
    if let Some(v) = result.structured_content {
        return Ok(ToolOutput::new(v));
    }
    let content = result.content;
    if content.len() == 1 {
        if let Some(text) = content[0].as_text() {
            return Ok(ToolOutput::new(serde_json::Value::String(
                text.text.clone(),
            )));
        }
    }
    let arr =
        serde_json::to_value(&content).map_err(|e| ToolError::Other(anyhow::Error::from(e)))?;
    Ok(ToolOutput::new(arr))
}

/// Concatenate the text parts of a content vec for error messages.
// used by McpTool (Task 4)
#[allow(dead_code)]
fn content_text(content: &[rmcp::model::Content]) -> String {
    let parts: Vec<&str> = content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.as_str()))
        .collect();
    if parts.is_empty() {
        "<no text content>".to_owned()
    } else {
        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{CallToolResult, Content};

    #[test]
    fn structured_content_wins() {
        let r = CallToolResult::structured(serde_json::json!({"a": 1}));
        let out = map_call_result(r).unwrap();
        assert_eq!(out.content, serde_json::json!({"a": 1}));
    }

    #[test]
    fn single_text_becomes_string() {
        let r = CallToolResult::success(vec![Content::text("hello")]);
        let out = map_call_result(r).unwrap();
        assert_eq!(out.content, serde_json::json!("hello"));
    }

    #[test]
    fn multi_content_becomes_array() {
        let r = CallToolResult::success(vec![Content::text("a"), Content::text("b")]);
        let out = map_call_result(r).unwrap();
        assert!(out.content.is_array());
        assert_eq!(out.content.as_array().unwrap().len(), 2);
    }

    #[test]
    fn is_error_maps_to_tool_error() {
        let r = CallToolResult::error(vec![Content::text("kaboom")]);
        let err = map_call_result(r).unwrap_err();
        assert!(err.to_string().contains("kaboom"));
    }
}
