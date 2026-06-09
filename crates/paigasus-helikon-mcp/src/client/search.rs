//! Lazy-mode `search_tools` meta-tool. Real search behavior lands in
//! SMA-327 Task 7; this is the minimal compiling shape.

use std::marker::PhantomData;
use std::sync::LazyLock;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use crate::client::handle::McpServerHandle;

/// Input schema for the `search_tools` meta-tool.
static SEARCH_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::json!({
        "type": "object",
        "properties": { "query": { "type": "string" } },
        "required": ["query"]
    })
});

/// Lazy-mode meta-tool that serves the real schemas of the server's tools
/// on demand. Appended to [`McpServerHandle::tools`] output in lazy mode.
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
        "Search this MCP server's tools and return their full input schemas."
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
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        // TODO(SMA-327 Task 7): match `query` against the cached tool
        // descriptors and return their real schemas.
        let _ = self.handle.cached_tools();
        Ok(ToolOutput::new(serde_json::Value::Array(vec![])))
    }
}
