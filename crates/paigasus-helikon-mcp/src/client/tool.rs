//! `McpTool` — adapts a remote MCP tool to core's `Tool<Ctx>` trait.

use std::marker::PhantomData;
use std::sync::LazyLock;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use rmcp::model::CallToolResult;

use crate::client::handle::McpServerHandle;

/// Map an MCP `CallToolResult` into core's `ToolOutput`/`ToolError`.
///
/// - `is_error == Some(true)` → `ToolError::Other` carrying the text content.
/// - `structured_content` (when present) becomes `ToolOutput.content` as-is.
/// - Otherwise a single text content becomes a JSON string; anything else is
///   serialized as a JSON array of content blocks.
pub(crate) fn map_call_result(result: CallToolResult) -> Result<ToolOutput, ToolError> {
    if result.is_error == Some(true) {
        // Known gap: a structured_error result also carries
        // structured_content; today only the text rendering reaches the
        // model. Surface the structured value if a real server needs it
        // (file a follow-up ticket when one does).
        let msg = content_text(&result.content);
        return Err(ToolError::Other(anyhow::anyhow!(
            "MCP tool returned an error: {msg}"
        )));
    }
    if let Some(v) = result.structured_content {
        return Ok(ToolOutput::new(v));
    }
    let content = result.content;
    if content.len() == 1 && content[0].as_text().is_some() {
        let text = content.into_iter().next().unwrap();
        // SAFETY: guarded by `as_text().is_some()` above.
        let rmcp::model::RawContent::Text(t) = text.raw else {
            unreachable!()
        };
        return Ok(ToolOutput::new(serde_json::Value::String(t.text)));
    }
    let arr =
        serde_json::to_value(&content).map_err(|e| ToolError::Other(anyhow::Error::from(e)))?;
    Ok(ToolOutput::new(arr))
}

/// Concatenate the text parts of a content vec for error messages.
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

/// Placeholder schema advertised by lazy-mode tools.
static PLACEHOLDER_SCHEMA: LazyLock<serde_json::Value> =
    LazyLock::new(|| serde_json::json!({ "type": "object", "additionalProperties": true }));

/// A remote MCP tool adapted to core's [`Tool`].
///
/// `Ctx` is a phantom: MCP tools never read the user context, so one handle
/// serves agents of any context type. `ToolEffect::Write` is never produced —
/// server-declared annotations are untrusted metadata and must not unlock
/// `AcceptEdits` auto-approval; `read_only_hint == true` maps to `ReadOnly`,
/// everything else to `SideEffect`.
pub struct McpTool<Ctx> {
    handle: McpServerHandle,
    wire_name: String,
    name: String,
    description: String,
    schema: serde_json::Value,
    output_schema: Option<serde_json::Value>,
    effect: ToolEffect,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> McpTool<Ctx> {
    pub(crate) fn new(handle: McpServerHandle, tool: &rmcp::model::Tool, lazy: bool) -> Self {
        let wire_name = tool.name.to_string();
        let name = handle.prefixed(&wire_name);
        let mut description = tool.description.as_deref().unwrap_or_default().to_owned();
        let schema = if lazy {
            let hint = format!(
                "(Full input schema available via the `{}` tool.)",
                handle.prefixed("search_tools")
            );
            if !description.is_empty() {
                description.push(' ');
            }
            description.push_str(&hint);
            PLACEHOLDER_SCHEMA.clone()
        } else {
            serde_json::Value::Object((*tool.input_schema).clone())
        };
        let output_schema = if lazy {
            None
        } else {
            tool.output_schema
                .as_ref()
                .map(|s| serde_json::Value::Object((**s).clone()))
        };
        let effect = match &tool.annotations {
            Some(a) if a.read_only_hint == Some(true) => ToolEffect::ReadOnly,
            _ => ToolEffect::SideEffect,
        };
        Self {
            handle,
            wire_name,
            name,
            description,
            schema,
            output_schema,
            effect,
            _ctx: PhantomData,
        }
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for McpTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }

    fn output_schema(&self) -> Option<&serde_json::Value> {
        self.output_schema.as_ref()
    }

    fn effect(&self) -> ToolEffect {
        self.effect
    }

    async fn invoke(
        &self,
        ctx: &ToolContext<Ctx>,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let arguments = match args {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            other => {
                return Err(ToolError::InvalidArgs {
                    schema_errors: vec![format!(
                        "MCP tools take a JSON object as arguments, got: {other}"
                    )],
                })
            }
        };
        // Race the remote call against the run's cancellation token: rmcp's
        // `call_tool` waits indefinitely, so a hung MCP server must not hang
        // the whole agent run past a cancel.
        let cancel = ctx.cancel().clone();
        let call = self.handle.call_tool_raw(&self.wire_name, arguments);
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(ToolError::Other(anyhow::anyhow!(
                    "MCP tool call `{}` cancelled",
                    self.name
                )));
            }
            r = call => r,
        };
        let result = result.map_err(|e| ToolError::Other(anyhow::Error::from(e)))?;
        map_call_result(result)
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
        let arr = out.content.as_array().unwrap();
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "a");
        assert_eq!(arr[1]["text"], "b");
    }

    #[test]
    fn is_error_maps_to_tool_error() {
        let r = CallToolResult::error(vec![Content::text("kaboom")]);
        let err = map_call_result(r).unwrap_err();
        assert!(err.to_string().contains("kaboom"));
    }
}
