//! Network tools — `WebFetchTool` and `WebSearchTool`. Enabled via the `web` feature.
//!
//! `WebFetchTool` fetches an HTTP(S) URL, extracts the main article via
//! Readability, and returns Markdown. It enforces an optional host allow/deny
//! list **and** a default-on SSRF guard (blocks private/loopback/link-local/
//! CGNAT/ULA addresses, including the cloud-metadata IP). `WebSearchTool` runs a
//! query through a swappable `SearchBackend`.

mod fetch;
pub(crate) mod http;

pub use fetch::{WebFetchTool, WebFetchToolBuilder};
