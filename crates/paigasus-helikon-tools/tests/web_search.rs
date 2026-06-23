#![allow(missing_docs)]
#![cfg(feature = "web")]

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

#[tokio::test]
async fn blocked_domains_drops_matching_results() {
    let backend = ScriptedBackend(vec![
        SearchResult::new("A", "https://good.example/a", "s", None),
        SearchResult::new("B", "https://evil.test/b", "s", None),
        SearchResult::new("C", "https://api.evil.test/c", "s", None), // subdomain
    ]);
    let tool = WebSearchTool::builder(Arc::new(backend))
        .blocked_domains(["evil.test"])
        .build::<()>();
    let out = tool
        .invoke(&ctx(), serde_json::json!({ "query": "x" }))
        .await
        .unwrap();
    let results = out.content["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["url"], "https://good.example/a");
}

#[tokio::test]
async fn allowed_domains_keeps_only_matching_results() {
    let backend = ScriptedBackend(vec![
        SearchResult::new("A", "https://docs.rs/x", "s", None),
        SearchResult::new("B", "https://crates.io/y", "s", None),
        SearchResult::new("C", "https://api.docs.rs/z", "s", None), // subdomain
    ]);
    let tool = WebSearchTool::builder(Arc::new(backend))
        .allowed_domains(["docs.rs"])
        .build::<()>();
    let out = tool
        .invoke(&ctx(), serde_json::json!({ "query": "x" }))
        .await
        .unwrap();
    let results = out.content["results"].as_array().unwrap();
    let urls: Vec<&str> = results.iter().map(|r| r["url"].as_str().unwrap()).collect();
    // exact set — guards against a too-permissive matcher (e.g. `notdocs.rs`)
    assert_eq!(urls, vec!["https://docs.rs/x", "https://api.docs.rs/z"]);
}

#[tokio::test]
async fn empty_allowed_domains_keeps_all_results() {
    // An empty allow-list normalizes to "no filter", not "reject everything".
    let backend = ScriptedBackend(vec![
        SearchResult::new("A", "https://a.example/1", "s", None),
        SearchResult::new("B", "https://b.example/2", "s", None),
    ]);
    let tool = WebSearchTool::builder(Arc::new(backend))
        .allowed_domains(Vec::<String>::new())
        .build::<()>();
    let out = tool
        .invoke(&ctx(), serde_json::json!({ "query": "x" }))
        .await
        .unwrap();
    assert_eq!(out.content["results"].as_array().unwrap().len(), 2);
}
