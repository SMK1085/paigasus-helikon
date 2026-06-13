# WebFetch + WebSearch Network Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `WebFetchTool` (HTTP fetch → Readability → Markdown, with a default-on SSRF guard) and `WebSearchTool` (a swappable `SearchBackend` trait with Brave + Tavily backends) to `paigasus-helikon-tools`, behind an off-by-default `web` Cargo feature.

**Architecture:** New gated `web/` module. `WebFetchTool` and `WebSearchTool` are independent `Tool<Ctx>` impls (SMA-328 conventions: `PhantomData<fn() -> Ctx>`, builder structs, schema cached as `serde_json::Value`). A private `web::http` module holds the shared `reqwest::Client` builder, host allow/deny matching, and the SSRF IP classifier. No public shared primitive (Approach A). No `paigasus-helikon-core` change — `ToolError::Denied` already exists.

**Tech Stack:** Rust, `reqwest` (rustls/aws-lc-rs, matching the providers), `url`, `dom_smoothie` (Readability), `htmd` (HTML→Markdown), `tokio` (`net` for DNS), `async-trait`, `serde`/`schemars`. Tests use `wiremock` (already a workspace dep) + `#[tokio::test]`.

**Spec:** `docs/superpowers/specs/2026-06-13-tools-web-network-design.md`

**Plan deviations from the spec (both strictly better, called out here):**
1. **Tests use `wiremock`, not a hand-rolled `TcpListener`.** The spec's hand-rolled note assumed wiremock would be a *new* dep; it is already in `[workspace.dependencies]` (root `Cargo.toml:60`), so using it adds nothing and is cleaner.
2. **Redirects are a manual loop with `reqwest::redirect::Policy::none()`**, not a `Policy::custom` closure. The per-hop SSRF check does async DNS, and reqwest's redirect closure is synchronous — so the client disables auto-redirects and `invoke` follows them itself, re-running the scheme/domain/SSRF checks on each hop.

---

## File Structure

**Created:**
- `crates/paigasus-helikon-tools/src/web/mod.rs` — gated module root + re-exports
- `crates/paigasus-helikon-tools/src/web/http.rs` — `build_client`, `host_allowed`, `ip_blocked`, `ssrf_check` (private)
- `crates/paigasus-helikon-tools/src/web/fetch.rs` — `WebFetchTool` + `WebFetchToolBuilder` + extraction
- `crates/paigasus-helikon-tools/src/web/search.rs` — `SearchBackend`, `SearchResult`, `WebSearchTool` + builder
- `crates/paigasus-helikon-tools/src/web/backends/mod.rs` — re-exports
- `crates/paigasus-helikon-tools/src/web/backends/brave.rs` — `BraveBackend` + `parse_brave`
- `crates/paigasus-helikon-tools/src/web/backends/tavily.rs` — `TavilyBackend` + `parse_tavily` + key-leak test
- `crates/paigasus-helikon-tools/tests/web_fetch.rs` — WebFetch integration tests
- `crates/paigasus-helikon-tools/tests/web_search.rs` — WebSearch integration test
- `crates/paigasus-helikon-tools/tests/fixtures/article.html` — extraction fixture
- `crates/paigasus-helikon-tools/tests/fixtures/brave_search.json` — Brave wire fixture
- `crates/paigasus-helikon-tools/tests/fixtures/tavily_search.json` — Tavily wire fixture
- `crates/paigasus-helikon-tools/examples/web_research.rs` — manual real-model demo

**Modified:**
- `Cargo.toml` (root) — add `url`, `dom_smoothie`, `htmd` to `[workspace.dependencies]`
- `crates/paigasus-helikon-tools/Cargo.toml` — optional web deps, `[features] web`, dev-dep `wiremock`, `[[example]]` entry
- `crates/paigasus-helikon-tools/src/lib.rs` — gated `mod web` + re-exports
- `crates/paigasus-helikon/Cargo.toml` — `tools-web` feature
- `crates/paigasus-helikon/src/lib.rs` — doc note on the `tools` re-export

---

## Task 1: Wire dependencies and the `web` feature skeleton

**Files:**
- Modify: `Cargo.toml` (root `[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon-tools/Cargo.toml`
- Create: `crates/paigasus-helikon-tools/src/web/mod.rs`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs`

- [ ] **Step 1: Add the three new third-party deps to the workspace**

In root `Cargo.toml`, inside `[workspace.dependencies]` (after the existing entries, keeping the column alignment loose), add:

```toml
url                   = "2"
dom_smoothie          = "0.18"
htmd                  = "0.5"
```

- [ ] **Step 2: Add optional deps + the `web` feature to the tools crate**

In `crates/paigasus-helikon-tools/Cargo.toml`, in `[dependencies]` add (after the existing `tokio` line):

```toml
reqwest      = { workspace = true, optional = true, features = ["json", "stream", "rustls"] }
url          = { workspace = true, optional = true }
dom_smoothie = { workspace = true, optional = true }
htmd         = { workspace = true, optional = true }
```

Add a `[features]` table (place it just above `[lints]`):

```toml
[features]
# Network tools (WebFetch + WebSearch). Off by default so FS/Bash-only
# consumers never pull reqwest. `tokio/net` is needed for the SSRF DNS check.
web = ["dep:reqwest", "dep:url", "dep:dom_smoothie", "dep:htmd", "tokio/net"]
```

In `[dev-dependencies]` add:

```toml
wiremock = { workspace = true }
```

- [ ] **Step 3: Create the gated module root**

Create `crates/paigasus-helikon-tools/src/web/mod.rs`:

```rust
//! Network tools — [`WebFetchTool`](fetch::WebFetchTool) and
//! [`WebSearchTool`](search::WebSearchTool). Enabled via the `web` feature.
//!
//! `WebFetchTool` fetches an HTTP(S) URL, extracts the main article via
//! Readability, and returns Markdown. It enforces an optional host allow/deny
//! list **and** a default-on SSRF guard (blocks private/loopback/link-local/
//! CGNAT/ULA addresses, including the cloud-metadata IP). `WebSearchTool` runs a
//! query through a swappable [`SearchBackend`](search::SearchBackend).
```

(Submodules are added in later tasks; an empty doc-only `mod.rs` compiles.)

- [ ] **Step 4: Declare the module from the crate root**

In `crates/paigasus-helikon-tools/src/lib.rs`, after the existing `mod write;` line, add:

```rust
#[cfg(feature = "web")]
mod web;
```

- [ ] **Step 5: Verify both feature states compile**

Run: `cargo build -p paigasus-helikon-tools`
Expected: builds, and `reqwest` is NOT pulled (default features only).

Run: `cargo build -p paigasus-helikon-tools --features web`
Expected: builds; downloads `reqwest`, `url`, `dom_smoothie`, `htmd`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/paigasus-helikon-tools/Cargo.toml \
  crates/paigasus-helikon-tools/src/web/mod.rs crates/paigasus-helikon-tools/src/lib.rs
git commit -m "feat(tools): SMA-412 add web feature skeleton + deps"
```

---

## Task 2: `web::http` — host matching, SSRF classifier, client builder

**Files:**
- Create: `crates/paigasus-helikon-tools/src/web/http.rs`
- Modify: `crates/paigasus-helikon-tools/src/web/mod.rs`

- [ ] **Step 1: Declare the module**

In `crates/paigasus-helikon-tools/src/web/mod.rs`, add at the top of the file body (after the `//!` docs):

```rust
pub(crate) mod http;
```

- [ ] **Step 2: Write the failing unit tests**

Create `crates/paigasus-helikon-tools/src/web/http.rs` with ONLY the tests first:

```rust
//! Private HTTP helpers shared by the web tools: the `reqwest::Client` builder,
//! host allow/deny matching, and the SSRF IP classifier.

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ip_blocked_rejects_private_and_special_ranges() {
        for s in [
            "127.0.0.1",        // loopback
            "10.0.0.1",         // RFC1918
            "172.16.0.1",       // RFC1918
            "192.168.1.1",      // RFC1918
            "169.254.169.254",  // link-local / cloud metadata
            "100.64.0.1",       // CGNAT
            "0.0.0.0",          // unspecified
            "::1",              // v6 loopback
            "fc00::1",          // v6 ULA
            "fe80::1",          // v6 link-local
        ] {
            assert!(ip_blocked(ip(s)), "{s} should be blocked");
        }
    }

    #[test]
    fn ip_blocked_allows_public_addresses() {
        for s in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!ip_blocked(ip(s)), "{s} should be allowed");
        }
    }

    #[test]
    fn ip_blocked_unwraps_v4_mapped_v6() {
        let mapped = IpAddr::V6(Ipv4Addr::new(169, 254, 169, 254).to_ipv6_mapped());
        assert!(ip_blocked(mapped));
        let _ = Ipv6Addr::LOCALHOST; // keep the import used on all platforms
    }

    #[test]
    fn host_allowed_deny_beats_allow_and_matches_subdomains() {
        let deny = vec!["evil.test".to_string()];
        // deny wins
        assert!(!host_allowed("evil.test", None, &deny));
        assert!(!host_allowed("api.evil.test", None, &deny)); // subdomain
        // unrelated host with no allow-list passes
        assert!(host_allowed("good.test", None, &deny));
        // allow-list restricts
        let allow = Some(vec!["docs.rs".to_string()]);
        assert!(host_allowed("docs.rs", allow.as_deref(), &[]));
        assert!(host_allowed("api.docs.rs", allow.as_deref(), &[])); // subdomain
        assert!(!host_allowed("crates.io", allow.as_deref(), &[]));
        // case-insensitive
        assert!(!host_allowed("EVIL.test", None, &deny));
        // partial-label is NOT a match
        assert!(host_allowed("notevil.test", None, &deny));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail to compile**

Run: `cargo test -p paigasus-helikon-tools --features web --lib http`
Expected: FAIL — `cannot find function ip_blocked` / `host_allowed`.

- [ ] **Step 4: Implement the helpers**

Prepend to `crates/paigasus-helikon-tools/src/web/http.rs` (above the `#[cfg(test)]` module):

```rust
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use paigasus_helikon_core::ToolError;

/// Build a `reqwest::Client` with a fixed user-agent and timeout. When
/// `follow_redirects` is false the client never auto-redirects (WebFetch drives
/// redirects itself so it can re-run the SSRF check on every hop).
pub(crate) fn build_client(
    user_agent: &str,
    timeout: Duration,
    follow_redirects: bool,
) -> reqwest::Result<reqwest::Client> {
    let redirect = if follow_redirects {
        reqwest::redirect::Policy::default()
    } else {
        reqwest::redirect::Policy::none()
    };
    reqwest::Client::builder()
        .user_agent(user_agent.to_owned())
        .timeout(timeout)
        .redirect(redirect)
        .build()
}

/// `true` if `host` is permitted by the allow/deny lists. A list entry matches
/// when `host` equals it or is a sub-domain of it (case-insensitive). A deny
/// match always refuses; with an allow-list set, only matching hosts pass.
pub(crate) fn host_allowed(host: &str, allow: Option<&[String]>, deny: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    let matches = |entry: &String| {
        let e = entry.to_ascii_lowercase();
        host == e || host.ends_with(&format!(".{e}"))
    };
    if deny.iter().any(matches) {
        return false;
    }
    match allow {
        Some(list) => list.iter().any(matches),
        None => true,
    }
}

/// `true` if `ip` is in a range the SSRF guard refuses: loopback, RFC1918
/// private, link-local (incl. `169.254.169.254`), CGNAT, IPv6 ULA, or
/// unspecified. v4-mapped v6 addresses are unwrapped and re-checked as v4.
pub(crate) fn ip_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_blocked(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return v4_blocked(mapped);
            }
            v6.is_loopback() || v6.is_unspecified() || is_ula(v6) || is_v6_link_local(v6)
        }
    }
}

fn v4_blocked(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || is_cgnat(ip)
}

/// `100.64.0.0/10` (RFC 6598 carrier-grade NAT). `std`'s predicate is unstable.
fn is_cgnat(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

/// `fc00::/7` (RFC 4193 unique-local). `std`'s predicate is unstable.
fn is_ula(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

/// `fe80::/10` (link-local). `std`'s predicate is unstable.
fn is_v6_link_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

/// SSRF guard: refuse a URL whose host is, or resolves to, a blocked IP. A
/// no-op when `allow_private` is true. Resolution failure is operational
/// (`Other`); a blocked address is a deliberate refusal (`Denied`).
pub(crate) async fn ssrf_check(url: &url::Url, allow_private: bool) -> Result<(), ToolError> {
    if allow_private {
        return Ok(());
    }
    let denied = |host: &str| ToolError::Denied {
        reason: format!("host `{host}` resolves to a blocked (private/loopback/link-local) address"),
    };
    match url.host() {
        Some(url::Host::Ipv4(ip)) => {
            if ip_blocked(IpAddr::V4(ip)) {
                return Err(denied(&ip.to_string()));
            }
        }
        Some(url::Host::Ipv6(ip)) => {
            if ip_blocked(IpAddr::V6(ip)) {
                return Err(denied(&ip.to_string()));
            }
        }
        Some(url::Host::Domain(host)) => {
            let port = url.port_or_known_default().unwrap_or(80);
            let addrs = tokio::net::lookup_host((host, port)).await.map_err(|e| {
                ToolError::Other(anyhow::anyhow!("DNS resolution failed for `{host}`: {e}"))
            })?;
            let mut any = false;
            for addr in addrs {
                any = true;
                if ip_blocked(addr.ip()) {
                    return Err(denied(host));
                }
            }
            if !any {
                return Err(ToolError::Other(anyhow::anyhow!(
                    "no addresses resolved for `{host}`"
                )));
            }
        }
        None => {
            return Err(ToolError::Denied {
                reason: "URL has no host".to_owned(),
            });
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools --features web --lib http`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-tools/src/web/http.rs crates/paigasus-helikon-tools/src/web/mod.rs
git commit -m "feat(tools): SMA-412 add web::http host matching + SSRF classifier"
```

---

## Task 3: WebFetch HTML→Markdown extraction (pure function)

**Files:**
- Create: `crates/paigasus-helikon-tools/src/web/fetch.rs`
- Create: `crates/paigasus-helikon-tools/tests/fixtures/article.html`
- Modify: `crates/paigasus-helikon-tools/src/web/mod.rs`

- [ ] **Step 1: Create the article fixture**

Create `crates/paigasus-helikon-tools/tests/fixtures/article.html`:

```html
<!doctype html>
<html>
  <head><title>The Hippocrene Spring</title><style>.x{color:red}</style></head>
  <body>
    <nav><a href="/">HOME NAV LINK</a><a href="/about">ABOUT NAV LINK</a></nav>
    <script>window.TRACKER = "should not appear";</script>
    <article>
      <h1>The Hippocrene Spring</h1>
      <p>On Mount Helicon, the winged horse Pegasus struck the ground with his
      hoof, and from that spot the Hippocrene spring began to flow. The Muses
      were said to gather by its waters.</p>
      <p>Poets later treated the spring as a source of inspiration, so that to
      drink from the Hippocrene meant to receive the gift of verse. The image
      recurs throughout classical and renaissance literature.</p>
      <p>This second paragraph exists so the document has enough real content for
      the Readability extractor to treat it as an article rather than chrome.</p>
    </article>
    <footer>FOOTER BOILERPLATE TEXT</footer>
  </body>
</html>
```

- [ ] **Step 2: Declare the module**

In `crates/paigasus-helikon-tools/src/web/mod.rs`, add after the `pub(crate) mod http;` line:

```rust
mod fetch;

pub use fetch::{WebFetchTool, WebFetchToolBuilder};
```

- [ ] **Step 3: Write the failing extraction test**

Create `crates/paigasus-helikon-tools/src/web/fetch.rs` with ONLY the extraction function's test (the rest of the file lands in Task 4):

```rust
//! [`WebFetchTool`] — HTTP(S) fetch → Readability → Markdown, with a host
//! allow/deny list and a default-on SSRF guard.

#[cfg(test)]
mod extract_tests {
    use super::*;

    #[test]
    fn extracts_article_and_drops_chrome() {
        let html = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/article.html"
        ));
        let md = html_to_markdown(html, Some("https://example.com/article")).unwrap();
        assert!(md.contains("Hippocrene"), "article body present:\n{md}");
        assert!(!md.contains("NAV LINK"), "nav stripped:\n{md}");
        assert!(!md.contains("should not appear"), "script stripped:\n{md}");
        assert!(!md.contains("FOOTER BOILERPLATE"), "footer stripped:\n{md}");
    }
}
```

- [ ] **Step 4: Run the test to verify it fails to compile**

Run: `cargo test -p paigasus-helikon-tools --features web --lib fetch`
Expected: FAIL — `cannot find function html_to_markdown`.

- [ ] **Step 5: Implement the extraction function**

Prepend to `crates/paigasus-helikon-tools/src/web/fetch.rs` (above the `#[cfg(test)]` module):

```rust
/// Extract the main article from `html` via Readability, then convert it to
/// Markdown. `base_url` improves Readability's relative-link handling. Errors
/// are operational (the page was fetched, but could not be parsed).
fn html_to_markdown(html: &str, base_url: Option<&str>) -> Result<String, anyhow::Error> {
    let mut readability = dom_smoothie::Readability::new(html, base_url, None)
        .map_err(|e| anyhow::anyhow!("readability init failed: {e}"))?;
    let article = readability
        .parse()
        .map_err(|e| anyhow::anyhow!("readability parse failed: {e}"))?;
    let content_html: &str = &article.content; // StrTendril derefs to str
    htmd::convert(content_html).map_err(|e| anyhow::anyhow!("html→markdown failed: {e}"))
}
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p paigasus-helikon-tools --features web --lib fetch`
Expected: PASS. (If Readability extracts differently, widen the fixture's article paragraphs — it needs enough prose to score as content — and keep the same assertions.)

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-tools/src/web/fetch.rs \
  crates/paigasus-helikon-tools/src/web/mod.rs \
  crates/paigasus-helikon-tools/tests/fixtures/article.html
git commit -m "feat(tools): SMA-412 add WebFetch HTML→Markdown extraction"
```

---

## Task 4: `WebFetchTool` — builder, invoke, redirect loop

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/web/fetch.rs`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs`
- Create: `crates/paigasus-helikon-tools/tests/web_fetch.rs`

- [ ] **Step 1: Implement the builder, tool, and invoke**

In `crates/paigasus-helikon-tools/src/web/fetch.rs`, insert this ABOVE the `html_to_markdown` function (keep the existing `//!` header at the very top):

```rust
use std::marker::PhantomData;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

use crate::web::http::{build_client, host_allowed, ssrf_check};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_BODY: usize = 5 << 20; // 5 MiB
const MAX_REDIRECTS: usize = 10;
const DEFAULT_UA: &str = concat!("paigasus-helikon-tools/", env!("CARGO_PKG_VERSION"));

/// Arguments for [`WebFetchTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WebFetchArgs {
    /// The absolute http(s) URL to fetch.
    url: String,
}

/// Builder for [`WebFetchTool`]. Start from [`WebFetchTool::builder`].
pub struct WebFetchToolBuilder {
    timeout: Duration,
    max_body_bytes: usize,
    allow_domains: Option<Vec<String>>,
    deny_domains: Vec<String>,
    allow_private_ips: bool,
    user_agent: String,
}

impl WebFetchToolBuilder {
    /// Whole-request timeout. Default 30s.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Cap on the downloaded body; past it the body is truncated and the output
    /// `truncated` flag is set. Default 5 MiB.
    pub fn max_body_bytes(mut self, n: usize) -> Self {
        self.max_body_bytes = n;
        self
    }

    /// Restrict fetches to these hosts (and their sub-domains). When unset, any
    /// host is allowed (subject to `deny_domains` and the SSRF guard).
    pub fn allow_domains<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow_domains = Some(hosts.into_iter().map(Into::into).collect());
        self
    }

    /// Refuse these hosts (and their sub-domains). Always wins over the
    /// allow-list.
    pub fn deny_domains<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.deny_domains = hosts.into_iter().map(Into::into).collect();
        self
    }

    /// Opt out of the default-on SSRF guard, allowing fetches that resolve to
    /// private/loopback addresses. Default `false` (guard on).
    pub fn allow_private_ips(mut self, allow: bool) -> Self {
        self.allow_private_ips = allow;
        self
    }

    /// Override the `User-Agent` header.
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }

    /// Finish building. Panics only if the underlying `reqwest::Client` cannot
    /// be constructed (a misconfigured TLS backend — not reachable with the
    /// pinned features).
    pub fn build<Ctx>(self) -> WebFetchTool<Ctx> {
        let client = build_client(&self.user_agent, self.timeout, false)
            .expect("reqwest client builds with the pinned TLS features");
        WebFetchTool {
            client,
            max_body_bytes: self.max_body_bytes,
            allow_domains: self.allow_domains,
            deny_domains: self.deny_domains,
            allow_private_ips: self.allow_private_ips,
            schema: serde_json::to_value(schemars::schema_for!(WebFetchArgs))
                .expect("WebFetchArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

/// Fetches an HTTP(S) URL, extracts the main article via Readability, and
/// returns Markdown. Enforces a host allow/deny list and a default-on SSRF
/// guard (§6.2 of the design). `effect() = SideEffect` (network).
pub struct WebFetchTool<Ctx = ()> {
    client: reqwest::Client,
    max_body_bytes: usize,
    allow_domains: Option<Vec<String>>,
    deny_domains: Vec<String>,
    allow_private_ips: bool,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl WebFetchTool<()> {
    /// Start building a `WebFetchTool` (30s timeout, 5 MiB body cap, SSRF guard
    /// on, no domain restrictions).
    pub fn builder() -> WebFetchToolBuilder {
        WebFetchToolBuilder {
            timeout: DEFAULT_TIMEOUT,
            max_body_bytes: DEFAULT_MAX_BODY,
            allow_domains: None,
            deny_domains: Vec::new(),
            allow_private_ips: false,
            user_agent: DEFAULT_UA.to_owned(),
        }
    }
}

impl<Ctx> WebFetchTool<Ctx> {
    /// Scheme + host allow/deny + SSRF guard. Returns `Denied` on any refusal.
    async fn vet(&self, url: &url::Url) -> Result<(), ToolError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ToolError::Denied {
                reason: "only http/https URLs are supported".to_owned(),
            });
        }
        let host = url.host_str().ok_or_else(|| ToolError::Denied {
            reason: "URL has no host".to_owned(),
        })?;
        if !host_allowed(host, self.allow_domains.as_deref(), &self.deny_domains) {
            return Err(ToolError::Denied {
                reason: format!("host `{host}` is blocked by the domain allow/deny list"),
            });
        }
        ssrf_check(url, self.allow_private_ips).await
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for WebFetchTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "WebFetch"
    }

    fn description(&self) -> &str {
        "Fetch an http(s) URL and return its main content as Markdown. Refuses \
         non-http(s) schemes, hosts blocked by the configured allow/deny list, \
         and (by default) hosts that resolve to private/loopback/link-local \
         addresses (SSRF guard). Non-2xx status and body truncation are reported \
         in the result, not raised as errors."
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::SideEffect
    }

    async fn invoke(&self, _ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: WebFetchArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
                schema_errors: vec![e.to_string()],
            })?;
        let mut current = url::Url::parse(&args.url).map_err(|e| ToolError::Denied {
            reason: format!("invalid URL: {e}"),
        })?;

        // Manual redirect loop so the SSRF check runs on every hop.
        for _ in 0..=MAX_REDIRECTS {
            self.vet(&current).await?;
            let resp = self
                .client
                .get(current.clone())
                .send()
                .await
                .map_err(|e| ToolError::Other(anyhow::anyhow!("request failed: {e}")))?;

            if resp.status().is_redirection() {
                let location = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| ToolError::Other(anyhow::anyhow!("redirect without Location")))?;
                current = current.join(location).map_err(|e| ToolError::Denied {
                    reason: format!("invalid redirect target: {e}"),
                })?;
                continue;
            }

            return self.finish(resp, current).await;
        }
        Err(ToolError::Denied {
            reason: format!("too many redirects (> {MAX_REDIRECTS})"),
        })
    }
}

impl<Ctx> WebFetchTool<Ctx> {
    /// Read a terminal (non-redirect) response: cap the body, branch on
    /// content-type, build the output JSON.
    async fn finish(
        &self,
        mut resp: reqwest::Response,
        final_url: url::Url,
    ) -> Result<ToolOutput, ToolError> {
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        let mut body: Vec<u8> = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| ToolError::Other(anyhow::anyhow!("body read failed: {e}")))?
        {
            if body.len() + chunk.len() > self.max_body_bytes {
                let remaining = self.max_body_bytes - body.len();
                body.extend_from_slice(&chunk[..remaining]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }

        let lower = content_type.to_ascii_lowercase();
        let (content, format) = if lower.contains("text/html") || lower.contains("application/xhtml")
        {
            let html = String::from_utf8_lossy(&body);
            let md = html_to_markdown(&html, Some(final_url.as_str()))
                .map_err(ToolError::Other)?;
            (md, "markdown")
        } else if is_textual(&lower) {
            (String::from_utf8_lossy(&body).into_owned(), "text")
        } else {
            return Err(ToolError::Denied {
                reason: format!("unsupported non-text content type: {content_type}"),
            });
        };

        Ok(ToolOutput::new(serde_json::json!({
            "url": final_url.as_str(),
            "status": status,
            "content_type": content_type,
            "content": content,
            "format": format,
            "truncated": truncated,
        })))
    }
}

/// Whether a (lowercased) content-type should be returned as plain text.
fn is_textual(lower: &str) -> bool {
    lower.is_empty()
        || lower.starts_with("text/")
        || lower.contains("application/json")
        || lower.contains("application/xml")
        || lower.contains("+json")
        || lower.contains("+xml")
}
```

- [ ] **Step 2: Re-export from the crate root**

In `crates/paigasus-helikon-tools/src/lib.rs`, after the existing `pub use write::WriteTool;` line, add:

```rust
#[cfg(feature = "web")]
pub use web::{WebFetchTool, WebFetchToolBuilder};
```

- [ ] **Step 3: Verify the crate still builds**

Run: `cargo build -p paigasus-helikon-tools --features web`
Expected: builds clean.

- [ ] **Step 4: Write the failing integration tests**

Create `crates/paigasus-helikon-tools/tests/web_fetch.rs`:

```rust
#![cfg(feature = "web")]

use std::sync::Arc;

use paigasus_helikon_core::{
    CancellationToken, Tool, ToolContext, ToolError, TracerHandle,
};
use paigasus_helikon_tools::WebFetchTool;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ctx() -> ToolContext<()> {
    ToolContext::new(
        Arc::new(()),
        TracerHandle::default(),
        CancellationToken::new(),
        0,
        8,
    )
}

#[tokio::test]
async fn denies_blocked_domain_without_network() {
    let tool = WebFetchTool::builder().deny_domains(["example.com"]).build::<()>();
    let err = tool
        .invoke(&ctx(), serde_json::json!({ "url": "https://example.com/page" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }), "got {err:?}");
}

#[tokio::test]
async fn denies_non_http_scheme() {
    let tool = WebFetchTool::builder().build::<()>();
    let err = tool
        .invoke(&ctx(), serde_json::json!({ "url": "file:///etc/passwd" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }), "got {err:?}");
}

#[tokio::test]
async fn ssrf_guard_denies_metadata_ip_by_default() {
    let tool = WebFetchTool::builder().build::<()>();
    let err = tool
        .invoke(
            &ctx(),
            serde_json::json!({ "url": "http://169.254.169.254/latest/meta-data/" }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }), "got {err:?}");
}

#[tokio::test]
async fn fetches_text_when_private_ips_allowed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_string("hello world"),
        )
        .mount(&server)
        .await;

    // server.uri() is on 127.0.0.1, so the SSRF guard must be opted out.
    let tool = WebFetchTool::builder().allow_private_ips(true).build::<()>();
    let out = tool
        .invoke(&ctx(), serde_json::json!({ "url": server.uri() }))
        .await
        .unwrap();
    assert_eq!(out.content["status"], 200);
    assert_eq!(out.content["format"], "text");
    assert_eq!(out.content["content"], "hello world");
    assert_eq!(out.content["truncated"], false);
}

#[tokio::test]
async fn truncates_body_at_cap() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/plain")
                .set_body_string("0123456789ABCDEF"),
        )
        .mount(&server)
        .await;

    let tool = WebFetchTool::builder()
        .allow_private_ips(true)
        .max_body_bytes(10)
        .build::<()>();
    let out = tool
        .invoke(&ctx(), serde_json::json!({ "url": server.uri() }))
        .await
        .unwrap();
    assert_eq!(out.content["truncated"], true);
    assert_eq!(out.content["content"].as_str().unwrap().len(), 10);
}
```

- [ ] **Step 5: Run the integration tests**

Run: `cargo test -p paigasus-helikon-tools --features web --test web_fetch`
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-tools/src/web/fetch.rs \
  crates/paigasus-helikon-tools/src/lib.rs \
  crates/paigasus-helikon-tools/tests/web_fetch.rs
git commit -m "feat(tools): SMA-412 add WebFetchTool with SSRF guard + redirect loop"
```

---

## Task 5: `SearchBackend` trait + `SearchResult` + `WebSearchTool`

**Files:**
- Create: `crates/paigasus-helikon-tools/src/web/search.rs`
- Modify: `crates/paigasus-helikon-tools/src/web/mod.rs`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs`
- Create: `crates/paigasus-helikon-tools/tests/web_search.rs`

- [ ] **Step 1: Implement the trait, result type, and tool**

Create `crates/paigasus-helikon-tools/src/web/search.rs`:

```rust
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
    /// Run `query`, returning at most `limit` normalized results.
    async fn search(&self, query: &str, limit: usize)
        -> Result<Vec<SearchResult>, anyhow::Error>;
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
    /// Default and ceiling for the per-call result count. Default 5.
    pub fn max_results(mut self, n: usize) -> Self {
        self.max_results = n;
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
            .min(HARD_MAX_RESULTS)
            .max(1);
        let results = self
            .backend
            .search(&args.query, limit)
            .await
            .map_err(ToolError::Other)?;
        Ok(ToolOutput::new(serde_json::json!({ "results": results })))
    }
}
```

- [ ] **Step 2: Declare the module and re-export**

In `crates/paigasus-helikon-tools/src/web/mod.rs`, after the `pub use fetch::...` line add:

```rust
mod search;

pub use search::{SearchBackend, SearchResult, WebSearchTool, WebSearchToolBuilder};
```

In `crates/paigasus-helikon-tools/src/lib.rs`, extend the gated re-export:

```rust
#[cfg(feature = "web")]
pub use web::{
    SearchBackend, SearchResult, WebFetchTool, WebFetchToolBuilder, WebSearchTool,
    WebSearchToolBuilder,
};
```

(Replace the Task-4 `pub use web::{WebFetchTool, WebFetchToolBuilder};` line with this combined one.)

- [ ] **Step 3: Write the failing swappability test**

Create `crates/paigasus-helikon-tools/tests/web_search.rs`:

```rust
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
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p paigasus-helikon-tools --features web --test web_search`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/src/web/search.rs \
  crates/paigasus-helikon-tools/src/web/mod.rs \
  crates/paigasus-helikon-tools/src/lib.rs \
  crates/paigasus-helikon-tools/tests/web_search.rs
git commit -m "feat(tools): SMA-412 add WebSearchTool + SearchBackend trait"
```

---

## Task 6: `BraveBackend`

**Files:**
- Create: `crates/paigasus-helikon-tools/src/web/backends/mod.rs`
- Create: `crates/paigasus-helikon-tools/src/web/backends/brave.rs`
- Create: `crates/paigasus-helikon-tools/tests/fixtures/brave_search.json`
- Modify: `crates/paigasus-helikon-tools/src/web/mod.rs`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs`

- [ ] **Step 1: Create the Brave wire fixture**

Create `crates/paigasus-helikon-tools/tests/fixtures/brave_search.json`:

```json
{
  "web": {
    "results": [
      {
        "title": "Mount Helicon - Wikipedia",
        "url": "https://en.wikipedia.org/wiki/Mount_Helicon",
        "description": "Mount Helicon is a mountain in Boeotia, Greece."
      },
      {
        "title": "Hippocrene",
        "url": "https://en.wikipedia.org/wiki/Hippocrene",
        "description": "A spring sacred to the Muses."
      }
    ]
  }
}
```

- [ ] **Step 2: Create the backends module root**

Create `crates/paigasus-helikon-tools/src/web/backends/mod.rs`:

```rust
//! Concrete [`SearchBackend`](crate::web::search::SearchBackend) implementations.

mod brave;
mod tavily;

pub use brave::BraveBackend;
pub use tavily::TavilyBackend;
```

(`tavily` lands in Task 7; declaring it now would not compile, so add only `mod brave;` / `pub use brave::BraveBackend;` here for this task and add the tavily lines in Task 7.)

- [ ] **Step 3: Implement `BraveBackend` with a fixture parse test**

Create `crates/paigasus-helikon-tools/src/web/backends/brave.rs`:

```rust
//! [`BraveBackend`] — the Brave Search API behind [`SearchBackend`].

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use crate::web::http::build_client;
use crate::web::search::{SearchBackend, SearchResult};

const DEFAULT_UA: &str = concat!("paigasus-helikon-tools/", env!("CARGO_PKG_VERSION"));
const DEFAULT_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

/// Brave Search API backend.
pub struct BraveBackend {
    client: reqwest::Client,
    api_key: String,
    endpoint: String,
}

impl BraveBackend {
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

    /// Build a backend, reading the key from `BRAVE_SEARCH_API_KEY`.
    pub fn from_env() -> Result<Self, anyhow::Error> {
        let key = std::env::var("BRAVE_SEARCH_API_KEY")
            .map_err(|_| anyhow::anyhow!("BRAVE_SEARCH_API_KEY is not set"))?;
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
impl SearchBackend for BraveBackend {
    fn name(&self) -> &str {
        "brave"
    }

    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, anyhow::Error> {
        let resp = self
            .client
            .get(&self.endpoint)
            .header("X-Subscription-Token", &self.api_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .query(&[("q", query), ("count", &limit.to_string())])
            .send()
            .await
            .map_err(|e| super::sanitize_err("brave", &e))?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!(
                "brave request failed: HTTP {}",
                resp.status().as_u16()
            ));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| super::sanitize_err("brave", &e))?;
        Ok(parse_brave(&body, limit))
    }
}

/// Map a Brave response body to normalized results.
fn parse_brave(body: &Value, limit: usize) -> Vec<SearchResult> {
    body.get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .take(limit)
                .map(|item| {
                    SearchResult::new(
                        str_field(item, "title"),
                        str_field(item, "url"),
                        str_field(item, "description"),
                        None,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn str_field(item: &Value, key: &str) -> String {
    item.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_brave_fixture() {
        let body: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/brave_search.json"
        )))
        .unwrap();
        let results = parse_brave(&body, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Mount Helicon - Wikipedia");
        assert_eq!(results[0].url, "https://en.wikipedia.org/wiki/Mount_Helicon");
        assert_eq!(results[0].snippet, "Mount Helicon is a mountain in Boeotia, Greece.");
        assert!(results[0].content.is_none());
    }

    #[test]
    fn parse_brave_respects_limit() {
        let body: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/brave_search.json"
        )))
        .unwrap();
        assert_eq!(parse_brave(&body, 1).len(), 1);
    }
}
```

- [ ] **Step 4: Add the shared `sanitize_err` helper**

In `crates/paigasus-helikon-tools/src/web/backends/mod.rs`, add below the `pub use` lines:

```rust
/// Map a `reqwest::Error` to a category-only message, never echoing the URL,
/// headers, or request body — so an API key (esp. Tavily's, sent in the body)
/// cannot leak into the model-visible tool result or traces (design-review M2).
pub(crate) fn sanitize_err(backend: &str, e: &reqwest::Error) -> anyhow::Error {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connection error"
    } else if e.is_decode() {
        "invalid response body"
    } else {
        "request error"
    };
    anyhow::anyhow!("{backend} request failed: {kind}")
}
```

- [ ] **Step 5: Declare the backends module and re-export**

In `crates/paigasus-helikon-tools/src/web/mod.rs`, after the `pub use search::...` line add:

```rust
mod backends;

pub use backends::BraveBackend;
```

In `crates/paigasus-helikon-tools/src/lib.rs`, add `BraveBackend` to the gated re-export list:

```rust
#[cfg(feature = "web")]
pub use web::{
    BraveBackend, SearchBackend, SearchResult, WebFetchTool, WebFetchToolBuilder, WebSearchTool,
    WebSearchToolBuilder,
};
```

- [ ] **Step 6: Run the parse tests**

Run: `cargo test -p paigasus-helikon-tools --features web --lib brave`
Expected: PASS (2 tests).

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-tools/src/web/backends/ \
  crates/paigasus-helikon-tools/src/web/mod.rs \
  crates/paigasus-helikon-tools/src/lib.rs \
  crates/paigasus-helikon-tools/tests/fixtures/brave_search.json
git commit -m "feat(tools): SMA-412 add BraveBackend + error sanitizer"
```

---

## Task 7: `TavilyBackend` + key-leak guard

**Files:**
- Create: `crates/paigasus-helikon-tools/src/web/backends/tavily.rs`
- Create: `crates/paigasus-helikon-tools/tests/fixtures/tavily_search.json`
- Modify: `crates/paigasus-helikon-tools/src/web/backends/mod.rs`
- Modify: `crates/paigasus-helikon-tools/src/web/mod.rs`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs`

- [ ] **Step 1: Create the Tavily wire fixture**

Create `crates/paigasus-helikon-tools/tests/fixtures/tavily_search.json`:

```json
{
  "query": "mount helicon",
  "results": [
    {
      "title": "Mount Helicon",
      "url": "https://example.com/helicon",
      "content": "Mount Helicon is a mountain celebrated in Greek mythology as sacred to the Muses, home to the springs Aganippe and Hippocrene."
    },
    {
      "title": "Hippocrene Spring",
      "url": "https://example.com/hippocrene",
      "content": "The Hippocrene was a spring on Mount Helicon, formed by the hoof of Pegasus."
    }
  ]
}
```

- [ ] **Step 2: Implement `TavilyBackend` with parse + key-leak tests**

Create `crates/paigasus-helikon-tools/src/web/backends/tavily.rs`:

```rust
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

    async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, anyhow::Error> {
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
                        item.get("title").and_then(|v| v.as_str()).unwrap_or_default(),
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
        assert!(!msg.contains("api_key"), "request body leaked in error: {msg}");
    }
}
```

- [ ] **Step 3: Wire up the module + re-exports**

In `crates/paigasus-helikon-tools/src/web/backends/mod.rs`, add `mod tavily;` next to `mod brave;` and `pub use tavily::TavilyBackend;` next to the Brave re-export.

In `crates/paigasus-helikon-tools/src/web/mod.rs`, change the backends re-export to:

```rust
pub use backends::{BraveBackend, TavilyBackend};
```

In `crates/paigasus-helikon-tools/src/lib.rs`, add `TavilyBackend` to the gated list:

```rust
#[cfg(feature = "web")]
pub use web::{
    BraveBackend, SearchBackend, SearchResult, TavilyBackend, WebFetchTool, WebFetchToolBuilder,
    WebSearchTool, WebSearchToolBuilder,
};
```

- [ ] **Step 4: Run the Tavily tests**

Run: `cargo test -p paigasus-helikon-tools --features web --lib tavily`
Expected: PASS (2 tests, incl. the key-leak guard).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/src/web/backends/ \
  crates/paigasus-helikon-tools/src/web/mod.rs \
  crates/paigasus-helikon-tools/src/lib.rs \
  crates/paigasus-helikon-tools/tests/fixtures/tavily_search.json
git commit -m "feat(tools): SMA-412 add TavilyBackend + key-leak guard"
```

---

## Task 8: Facade `tools-web` feature + cross-cutting verification

**Files:**
- Modify: `crates/paigasus-helikon/Cargo.toml`
- Modify: `crates/paigasus-helikon/src/lib.rs`

- [ ] **Step 1: Add the facade feature**

In `crates/paigasus-helikon/Cargo.toml`, in `[features]`, add after the `tools = [...]` line:

```toml
tools-web          = ["tools", "paigasus-helikon-tools/web"]
```

- [ ] **Step 2: Note it on the re-export doc**

In `crates/paigasus-helikon/src/lib.rs`, update the doc comment above the `tools` re-export to mention the web tools:

```rust
/// Sandboxed Read/Write/Edit/Bash tools. Enabled via the `tools` feature; the
/// `WebFetch`/`WebSearch` network tools additionally require `tools-web`.
#[cfg(feature = "tools")]
pub use paigasus_helikon_tools as tools;
```

- [ ] **Step 3: Verify the facade feature resolves**

Run: `cargo build -p paigasus-helikon --features tools-web`
Expected: builds; `paigasus_helikon::tools::WebFetchTool` is reachable.

- [ ] **Step 4: Verify the default tools crate pulls no reqwest**

Run: `cargo tree -p paigasus-helikon-tools -i reqwest 2>&1 | head`
Expected: `package ID specification ... did not match any packages` (reqwest absent without `--features web`).

Run: `cargo tree -p paigasus-helikon-tools --features web -i reqwest | head -1`
Expected: prints `reqwest v0.13...` (present with the feature).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon/Cargo.toml crates/paigasus-helikon/src/lib.rs
git commit -m "feat(facade): SMA-412 add tools-web feature for network tools"
```

---

## Task 9: Manual real-model example

**Files:**
- Create: `crates/paigasus-helikon-tools/examples/web_research.rs`
- Modify: `crates/paigasus-helikon-tools/Cargo.toml`

- [ ] **Step 1: Register the example with its required feature**

In `crates/paigasus-helikon-tools/Cargo.toml`, after the `[dev-dependencies]` block add:

```toml
[[example]]
name = "web_research"
required-features = ["web"]
```

- [ ] **Step 2: Write the example**

Create `crates/paigasus-helikon-tools/examples/web_research.rs`:

```rust
//! Real-model demo: an agent researches a question with `WebSearch` + `WebFetch`,
//! with both network tools gated by a `PermissionPolicy`.
//!
//! Run with keys:
//! `OPENAI_API_KEY=... BRAVE_SEARCH_API_KEY=... \
//!   cargo run -p paigasus-helikon-tools --features web --example web_research`
//!
//! This example is the canonical reference for gating network tools with a
//! `PermissionPolicy`. The policy below allows `WebSearch` and `WebFetch`; swap
//! an `Allow` for an `AskUser`/`Deny` to see a tool blocked.

use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, HookRegistry, LlmAgent, MemorySession,
    PermissionDecision, PermissionPolicy, RunContext, TracerHandle,
};
use paigasus_helikon_providers_openai::OpenAiModel;
use paigasus_helikon_tools::{BraveBackend, WebFetchTool, WebSearchTool};

/// Allow the network tools explicitly; deny everything else.
struct AllowWebTools;

#[async_trait]
impl PermissionPolicy<()> for AllowWebTools {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        tool: &str,
        _args: &serde_json::Value,
    ) -> PermissionDecision {
        match tool {
            "WebSearch" | "WebFetch" => PermissionDecision::Allow,
            _ => PermissionDecision::AskUser {
                prompt: format!("Allow `{tool}`?"),
            },
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let backend = Arc::new(BraveBackend::from_env()?);
    let model = OpenAiModel::chat("gpt-5-mini").build()?;

    let agent = LlmAgent::builder::<()>()
        .name("web-researcher")
        .model(model)
        .instructions(
            "Research the user's question. Use WebSearch to find sources, then \
             WebFetch a result URL to read it. Cite the URLs you used.",
        )
        .tool(WebSearchTool::builder(backend).build())
        // SSRF guard on by default; allow_domains/deny_domains could narrow it.
        .tool(WebFetchTool::builder().build())
        .build();

    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::<()>::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
    .with_permission_policy(Arc::new(AllowWebTools));

    let input = AgentInput::from_user_text(
        "What is the Hippocrene spring and how does it relate to Mount Helicon?",
    );
    let mut stream = agent.run(ctx, input).await?;
    let mut stdout = std::io::stdout();
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::TokenDelta { text } => {
                print!("{text}");
                stdout.flush()?;
            }
            AgentEvent::RunFailed { error } => anyhow::bail!("run failed: {error}"),
            _ => {}
        }
    }
    println!();
    Ok(())
}
```

- [ ] **Step 3: Verify the example compiles**

Run: `cargo build -p paigasus-helikon-tools --features web --example web_research`
Expected: builds clean (does not run; needs API keys).

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-tools/examples/web_research.rs \
  crates/paigasus-helikon-tools/Cargo.toml
git commit -m "feat(tools): SMA-412 add web_research example gating network tools"
```

---

## Task 10: Full local CI gate run + deny.toml review

**Files:**
- Modify (if needed): `deny.toml`, `Cargo.lock`, any files flagged by fmt/clippy

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then: `cargo fmt --all -- --check`
Expected: no diff.

- [ ] **Step 2: Clippy (all features, all targets)**

Run: `cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: no warnings. Fix any (common: `needless_borrow`, `redundant_clone`).

- [ ] **Step 3: Full test suite (all features)**

Run: `cargo test --workspace --all-features`
Expected: all pass, including the new lib unit tests and `web_fetch` / `web_search` integration tests.

- [ ] **Step 4: Docs (warnings = errors)**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
Expected: builds with no missing-docs or broken-intra-doc-link warnings. (Every `pub` item added has a `///` doc.)

- [ ] **Step 5: cargo-deny (license + advisory review for the new deps)**

Run: `cargo deny check`
Expected: PASS. `dom_smoothie` (MIT) and `htmd` (Apache-2.0) are already on the allowlist. If a *transitive* dep fails on license, add a narrowly-scoped allow entry to `deny.toml` with a comment naming the crate and SMA-412; if it fails on an advisory, evaluate before suppressing. Commit any `deny.toml` change.

- [ ] **Step 6: Doc coverage**

Run: `DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh`
Expected: PASS (requires the pinned nightly toolchain installed).

- [ ] **Step 7: Commit any fixes + the lockfile**

```bash
git add -- Cargo.lock deny.toml crates/
git commit -m "chore(tools): SMA-412 satisfy fmt/clippy/deny gates for web tools"
```

(Use explicit paths — never `git add -A` in this repo: `.env`/`.claude` are untracked-but-not-ignored. Skip this commit if Steps 1–6 produced no changes.)

---

## Self-Review (completed during plan authoring)

**Spec coverage:** WebFetch (Task 4) + extraction (Task 3) + SSRF guard (Task 2/4, AC) + domain allow/deny (Task 2/4, AC) ✓; WebSearch + trait + Arc swappability (Task 5, AC) ✓; Brave + Tavily backends parse-tested (Tasks 6/7, AC) ✓; M2 key sanitization (Tasks 6/7) ✓; `web` feature gating + `tools-web` facade + no-reqwest-by-default (Tasks 1/8, AC) ✓; example gating network tools (Task 9) ✓; release mechanics need no manual steps (spec §12) so no task is required — release-plz auto-bumps on merge.

**Placeholder scan:** No TBD/TODO; every code step shows complete code; commands have expected output.

**Type consistency:** `SearchResult::new` (4 args) used identically in backends and tests; `WebFetchTool::builder()` (no args) / `WebSearchTool::builder(Arc<dyn SearchBackend>)` consistent; `build_client(ua, timeout, follow_redirects)`, `host_allowed(host, Option<&[String]>, &[String])`, `ip_blocked(IpAddr)`, `ssrf_check(&Url, bool)`, `sanitize_err(&str, &reqwest::Error)` signatures match every call site; lib.rs re-export list grows monotonically and ends with all 8 public items.

**Note on `with_endpoint`:** the backends' test-only `#[cfg(test)] fn with_endpoint` is exercised by unit tests inside the same file (so private access is fine); it is not part of the public API.
