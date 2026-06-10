//! The lazy-mode `search_tools` meta-tool.

use std::marker::PhantomData;
use std::sync::LazyLock;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use crate::client::handle::McpServerHandle;

/// Input schema for the `search_tools` meta-tool.
static SEARCH_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Keyword or substring matched (case-insensitively) \
                                against tool names and descriptions."
            }
        },
        "required": ["query"]
    })
});

/// Lazy-mode meta-tool: searches the connected server's cached tool list and
/// returns matching tools' real names, descriptions, and input schemas.
pub(crate) struct SearchTool<Ctx> {
    handle: McpServerHandle,
    name: String,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl<Ctx> SearchTool<Ctx> {
    /// Build the meta-tool for `handle` (name gets the configured prefix).
    pub(crate) fn new(handle: McpServerHandle) -> Self {
        let name = handle.prefixed("search_tools");
        Self {
            handle,
            name,
            _ctx: PhantomData,
        }
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for SearchTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Search this MCP server's tools by keyword. Returns matching tools' \
         names, descriptions, and full input schemas. Call this before using \
         a tool whose schema you don't know."
    }

    fn schema(&self) -> &serde_json::Value {
        &SEARCH_SCHEMA
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn invoke(
        &self,
        _ctx: &ToolContext<Ctx>,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs {
                schema_errors: vec!["missing required string field `query`".to_owned()],
            })?
            .to_lowercase();

        let matches: Vec<serde_json::Value> = self
            .handle
            .cached_tools()
            .iter()
            .filter(|t| {
                t.name.to_lowercase().contains(&query)
                    || t.description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&query))
            })
            .map(|t| {
                serde_json::json!({
                    "name": self.handle.prefixed(&t.name),
                    "description": t.description.as_deref().unwrap_or_default(),
                    "input_schema": serde_json::Value::Object((*t.input_schema).clone()),
                })
            })
            .collect();

        Ok(ToolOutput::new(serde_json::Value::Array(matches)))
    }
}
