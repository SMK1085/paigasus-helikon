//! [`WebSearchTool`] and the swappable [`SearchBackend`] trait.

use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

/// Upper bound on results requested from a backend, regardless of the model's
/// `limit`.
const HARD_MAX_RESULTS: usize = 20;

/// One normalized search hit.
#[derive(Debug, Clone, serde::Serialize)]
#[non_exhaustive]
pub struct SearchResult {
    /// Result title.
    pub title: String,
    /// Result URL.
    pub url: String,
    /// Short snippet / description.
    pub snippet: String,
    /// Richer page content when the backend supplies it (Tavily); `None`
    /// otherwise. Omitted from the serialized output when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl SearchResult {
    /// Construct a [`SearchResult`]. Required for external backends because the
    /// struct is `#[non_exhaustive]`.
    pub fn new(
        title: impl Into<String>,
        url: impl Into<String>,
        snippet: impl Into<String>,
        content: Option<String>,
    ) -> Self {
        Self {
            title: title.into(),
            url: url.into(),
            snippet: snippet.into(),
            content,
        }
    }
}

/// A swappable search provider. Implement this to add a backend (Brave and
/// Tavily ship in this crate).
#[async_trait]
pub trait SearchBackend: Send + Sync {
    /// Backend name, for diagnostics.
    fn name(&self) -> &str;
    /// Run `query`. Implementors MUST return at most `limit` normalized results.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, anyhow::Error>;
}

/// Arguments for [`WebSearchTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WebSearchArgs {
    /// The search query.
    query: String,
    /// Maximum number of results (defaults to the tool's configured maximum).
    limit: Option<usize>,
}

/// Builder for [`WebSearchTool`]. Start from [`WebSearchTool::builder`].
pub struct WebSearchToolBuilder {
    backend: Arc<dyn SearchBackend>,
    max_results: usize,
}

impl WebSearchToolBuilder {
    /// Default and ceiling for the per-call result count. Default 5; clamped to
    /// `1..=20` so the stored value always holds the field invariant.
    pub fn max_results(mut self, n: usize) -> Self {
        self.max_results = n.clamp(1, HARD_MAX_RESULTS);
        self
    }

    /// Finish building.
    pub fn build<Ctx>(self) -> WebSearchTool<Ctx> {
        WebSearchTool {
            backend: self.backend,
            max_results: self.max_results,
            schema: serde_json::to_value(schemars::schema_for!(WebSearchArgs))
                .expect("WebSearchArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

/// Runs a query through a swappable [`SearchBackend`]. `effect() = SideEffect`.
pub struct WebSearchTool<Ctx = ()> {
    backend: Arc<dyn SearchBackend>,
    max_results: usize,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl WebSearchTool<()> {
    /// Start building a `WebSearchTool` over `backend` (default 5 results).
    pub fn builder(backend: Arc<dyn SearchBackend>) -> WebSearchToolBuilder {
        WebSearchToolBuilder {
            backend,
            max_results: 5,
        }
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for WebSearchTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "WebSearch"
    }

    fn description(&self) -> &str {
        "Search the web and return a list of results (title, url, snippet). \
         Use it to find pages, then WebFetch a result URL to read it."
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::SideEffect
    }

    async fn invoke(&self, _ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: WebSearchArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
                schema_errors: vec![e.to_string()],
            })?;
        let limit = args
            .limit
            .unwrap_or(self.max_results)
            .clamp(1, HARD_MAX_RESULTS);
        let results =
            self.backend.search(&args.query, limit).await.map_err(|e| {
                ToolError::Other(anyhow::anyhow!("[{}] {e:#}", self.backend.name()))
            })?;
        Ok(ToolOutput::new(serde_json::json!({ "results": results })))
    }
}
