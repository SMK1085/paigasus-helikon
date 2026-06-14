#![cfg(feature = "web")]
#![allow(missing_docs)]

use std::sync::Arc;

use async_trait::async_trait;
use paigasus_helikon_core::{CancellationToken, Tool, ToolContext, TracerHandle};
use paigasus_helikon_tools::{SearchBackend, SearchResult, WebSearchTool};

fn ctx() -> ToolContext<()> {
    ToolContext::new(
        Arc::new(()),
        TracerHandle::default(),
        CancellationToken::new(),
        0,
        8,
    )
}

struct ScriptedBackend(Vec<SearchResult>);

#[async_trait]
impl SearchBackend for ScriptedBackend {
    fn name(&self) -> &str {
        "scripted"
    }
    async fn search(&self, _q: &str, _l: usize) -> Result<Vec<SearchResult>, anyhow::Error> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn returns_normalized_results_from_backend() {
    let backend = ScriptedBackend(vec![SearchResult::new(
        "Helikon",
        "https://example.com/helikon",
        "the SDK",
        None,
    )]);
    let tool = WebSearchTool::builder(Arc::new(backend)).build::<()>();
    let out = tool
        .invoke(&ctx(), serde_json::json!({ "query": "helikon" }))
        .await
        .unwrap();
    let results = out.content["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["title"], "Helikon");
    assert_eq!(results[0]["url"], "https://example.com/helikon");
    // content is None -> omitted
    assert!(results[0].get("content").is_none());
}
