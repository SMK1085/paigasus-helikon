//! The lazy-mode `search_tools` meta-tool.

use std::marker::PhantomData;
use std::sync::LazyLock;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};

use crate::client::handle::McpServerHandle;

/// Maximum number of tools returned by a single `search_tools` call.
///
/// When the total number of matching tools exceeds this cap the response
/// includes `"truncated": true` alongside `"total_matches"` so the caller can
/// refine its query.
const MAX_SEARCH_RESULTS: usize = 20;

/// Input schema for the `search_tools` meta-tool.
static SEARCH_SCHEMA: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Keyword or substring matched (case-insensitively) \
                                against tool names and descriptions.",
                "minLength": 1
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

/// Core filtering and capping logic extracted for unit-testability.
///
/// Returns a tuple of:
/// - the capped list of matching tool objects (at most [`MAX_SEARCH_RESULTS`] entries), and
/// - the **total** number of matching tools before the cap was applied.
///
/// `prefixer` maps a raw tool name to its (potentially prefixed) display name.
fn search_matches(
    tools: &[rmcp::model::Tool],
    prefixer: impl Fn(&str) -> String,
    query: &str,
) -> (Vec<serde_json::Value>, usize) {
    let query_lc = query.to_lowercase();
    let all_matches: Vec<serde_json::Value> = tools
        .iter()
        .filter(|t| {
            t.name.to_lowercase().contains(&query_lc)
                || t.description
                    .as_deref()
                    .is_some_and(|d| d.to_lowercase().contains(&query_lc))
        })
        .map(|t| {
            serde_json::json!({
                "name": prefixer(&t.name),
                "description": t.description.as_deref().unwrap_or_default(),
                "input_schema": serde_json::Value::Object((*t.input_schema).clone()),
            })
        })
        .collect();

    let total = all_matches.len();
    let capped: Vec<serde_json::Value> = all_matches.into_iter().take(MAX_SEARCH_RESULTS).collect();
    (capped, total)
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
         names, descriptions, and full input schemas (capped at 20 results — \
         refine your query if you need a shorter list). Call this before using \
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
        // Validate: field must exist.
        let raw = args.get("query").ok_or_else(|| ToolError::InvalidArgs {
            schema_errors: vec!["missing required field `query`".to_owned()],
        })?;

        // Validate: field must be a string.
        let query_str = raw.as_str().ok_or_else(|| ToolError::InvalidArgs {
            schema_errors: vec![format!("`query` must be a string, got {raw}")],
        })?;

        // Validate: string must be non-empty.
        if query_str.is_empty() {
            return Err(ToolError::InvalidArgs {
                schema_errors: vec!["`query` must be a non-empty string".to_owned()],
            });
        }

        let cached = self.handle.cached_tools();
        let prefixer = |name: &str| self.handle.prefixed(name);
        let (matched, total) = search_matches(cached, prefixer, query_str);

        let truncated = total > MAX_SEARCH_RESULTS;
        let mut obj = serde_json::json!({
            "tools": matched,
            "total_matches": total,
        });
        if truncated {
            obj["truncated"] = serde_json::Value::Bool(true);
        }

        Ok(ToolOutput::new(obj))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{JsonObject, Tool as RmcpTool};
    use serde_json::json;
    use std::sync::Arc;

    fn make_tool(name: &str, description: &str) -> RmcpTool {
        let schema: JsonObject = match json!({"type": "object"}) {
            serde_json::Value::Object(o) => o,
            _ => unreachable!(),
        };
        RmcpTool::new(name.to_owned(), description.to_owned(), Arc::new(schema))
    }

    /// Build 25 synthetic tools: names "tool_00" .. "tool_24", all with
    /// description "synthetic" so every query containing "synthetic" matches all.
    fn twenty_five_tools() -> Vec<RmcpTool> {
        (0..25)
            .map(|i| make_tool(&format!("tool_{i:02}"), "synthetic item"))
            .collect()
    }

    #[test]
    fn cap_is_applied_and_total_reflects_full_count() {
        let tools = twenty_five_tools();
        let (capped, total) = search_matches(&tools, |n| n.to_owned(), "synthetic");
        assert_eq!(total, 25, "all 25 tools should match");
        assert_eq!(capped.len(), MAX_SEARCH_RESULTS, "output is capped at 20");
    }

    #[test]
    fn untruncated_results_have_matching_len_and_total() {
        let tools = twenty_five_tools();
        // "tool_01" matches exactly one entry.
        let (matched, total) = search_matches(&tools, |n| n.to_owned(), "tool_01");
        assert_eq!(total, 1);
        assert_eq!(matched.len(), 1);
    }

    #[test]
    fn prefixer_is_applied_to_names() {
        let tools = vec![make_tool("echo", "Echo a message")];
        let (matched, _) = search_matches(&tools, |n| format!("fs_{n}"), "echo");
        assert_eq!(matched[0]["name"], "fs_echo");
    }
}
