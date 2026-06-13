//! Concrete [`SearchBackend`](crate::web::search::SearchBackend) implementations.

mod brave;
mod tavily;

pub use brave::BraveBackend;
pub use tavily::TavilyBackend;

/// Map a `reqwest::Error` to a category-only message, never echoing the URL,
/// headers, or request body — so an API key (esp. Tavily's, sent in the body)
/// cannot leak into the model-visible tool result or traces (design-review M2).
pub(crate) fn sanitize_err(backend: &str, e: &reqwest::Error) -> anyhow::Error {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connection error"
    } else if e.is_body() {
        "response body error"
    } else if e.is_decode() {
        "invalid response body"
    } else {
        "request error"
    };
    anyhow::anyhow!("{backend} request failed: {kind}")
}
