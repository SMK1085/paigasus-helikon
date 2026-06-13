//! [`TavilyBackend`] — the Tavily search API behind [`SearchBackend`].

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use crate::web::http::build_client;
use crate::web::search::{SearchBackend, SearchResult};

const DEFAULT_UA: &str = concat!("paigasus-helikon-tools/", env!("CARGO_PKG_VERSION"));
const DEFAULT_ENDPOINT: &str = "https://api.tavily.com/search";
const SNIPPET_CHARS: usize = 200;

/// Tavily search API backend.
pub struct TavilyBackend {
    client: reqwest::Client,
    api_key: String,
    endpoint: String,
}

impl TavilyBackend {
    /// Build a backend with an explicit API key.
    pub fn new(api_key: impl Into<String>) -> Result<Self, anyhow::Error> {
        let client = build_client(DEFAULT_UA, Duration::from_secs(30), true)
            .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))?;
        Ok(Self {
            client,
            api_key: api_key.into(),
            endpoint: DEFAULT_ENDPOINT.to_owned(),
        })
    }

    /// Build a backend, reading the key from `TAVILY_API_KEY`.
    pub fn from_env() -> Result<Self, anyhow::Error> {
        let key = std::env::var("TAVILY_API_KEY")
            .map_err(|_| anyhow::anyhow!("TAVILY_API_KEY is not set"))?;
        Self::new(key)
    }

    #[cfg(test)]
    fn with_endpoint(api_key: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            client: build_client(DEFAULT_UA, Duration::from_secs(30), true).unwrap(),
            api_key: api_key.into(),
            endpoint: endpoint.into(),
        }
    }
}

#[async_trait]
impl SearchBackend for TavilyBackend {
    fn name(&self) -> &str {
        "tavily"
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, anyhow::Error> {
        let req_body = serde_json::json!({
            "api_key": self.api_key,
            "query": query,
            "max_results": limit,
        });
        let resp = self
            .client
            .post(&self.endpoint)
            .json(&req_body)
            .send()
            .await
            .map_err(|e| super::sanitize_err("tavily", &e))?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!(
                "tavily request failed: HTTP {}",
                resp.status().as_u16()
            ));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| super::sanitize_err("tavily", &e))?;
        Ok(parse_tavily(&body, limit))
    }
}

/// Map a Tavily response body to normalized results. Tavily returns a `content`
/// chunk per result; the snippet is that content truncated to [`SNIPPET_CHARS`].
fn parse_tavily(body: &Value, limit: usize) -> Vec<SearchResult> {
    body.get("results")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .take(limit)
                .map(|item| {
                    let content = item
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned);
                    let snippet = content
                        .as_deref()
                        .map(|c| c.chars().take(SNIPPET_CHARS).collect::<String>())
                        .unwrap_or_default();
                    SearchResult::new(
                        item.get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default(),
                        item.get("url").and_then(|v| v.as_str()).unwrap_or_default(),
                        snippet,
                        content,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_tavily_fixture() {
        let body: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tavily_search.json"
        )))
        .unwrap();
        let results = parse_tavily(&body, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Mount Helicon");
        assert_eq!(results[0].url, "https://example.com/helicon");
        assert!(results[0].content.is_some());
        assert!(!results[0].snippet.is_empty());
        assert!(results[0].snippet.chars().count() <= SNIPPET_CHARS);
    }

    #[tokio::test]
    async fn error_never_leaks_api_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let secret = "tvly-SUPER-SECRET-KEY";
        let backend = TavilyBackend::with_endpoint(secret, format!("{}/search", server.uri()));
        let err = backend.search("anything", 3).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(!msg.contains(secret), "key leaked in error: {msg}");
        assert!(
            !msg.contains("api_key"),
            "request body leaked in error: {msg}"
        );
    }
}
