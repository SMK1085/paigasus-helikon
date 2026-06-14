//! [`WebFetchTool`] — HTTP(S) fetch → Readability → Markdown, with a host
//! allow/deny list and a default-on SSRF guard.

use std::marker::PhantomData;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

use crate::web::http::{build_client, host_allowed, ssrf_check};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_BODY: usize = 5 << 20; // 5 MiB
const MAX_REDIRECTS: usize = 5;
/// Run-scoped key under which the per-run WebFetch use counter is stored.
const USES_KEY: &str = "paigasus_helikon_tools::web_fetch::uses";
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
    max_uses: Option<usize>,
}

impl WebFetchToolBuilder {
    /// Per-request timeout, applied to each redirect hop individually (not the
    /// whole redirect chain). Default 30s.
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

    /// Restrict fetches to these hosts (and their sub-domains). When unset — or
    /// set to an empty list — any host is allowed (subject to `deny_domains` and
    /// the SSRF guard). Normalizing empty → "no restriction" avoids the footgun
    /// where optional/env-derived config silently blocks every fetch.
    pub fn allow_domains<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let hosts: Vec<String> = hosts.into_iter().map(Into::into).collect();
        self.allow_domains = (!hosts.is_empty()).then_some(hosts);
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

    /// Override the `User-Agent` header. The value must be a valid HTTP header
    /// value (visible ASCII, no control characters or newlines); an invalid
    /// value makes [`build`](Self::build) panic.
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }

    /// Cap the number of fetches this tool may perform within a single agent
    /// run. The `(n+1)`th fetch in a run is refused with [`ToolError::Denied`].
    /// Default: unlimited.
    ///
    /// The cap is **exact**: the run-scoped counter is bumped with an atomic
    /// check-and-increment on the shared `SessionState`, so even simultaneous
    /// invocations (parallel sub-agents or parallel tool calls sharing the run)
    /// admit **at most** `n` fetches in total. "Per run" means one shared
    /// `SessionState` — the agent run plus its handoff chain and parallel
    /// sub-agents; an `AgentAsTool` sub-run uses a fresh `SessionState` and so
    /// gets its own independent budget.
    ///
    /// Every *attempt* counts: the use is consumed up front, before the SSRF
    /// vet and the network request, so a failed, non-2xx, or SSRF-blocked fetch
    /// still spends one of the `n`. The security boundary is the SSRF guard, not
    /// this counter.
    pub fn max_uses(mut self, n: usize) -> Self {
        self.max_uses = Some(n);
        self
    }

    /// Finish building. Panics if the `reqwest::Client` cannot be built — with
    /// the pinned TLS backend the only reachable cause is a `user_agent` set to
    /// a string containing invalid HTTP header bytes (control chars/newlines);
    /// the default user-agent always builds. `User-Agent` is trusted integrator
    /// config (not request/model input), so this is treated as a configuration
    /// error rather than a recoverable runtime failure.
    pub fn build<Ctx>(self) -> WebFetchTool<Ctx> {
        let client = build_client(
            &self.user_agent,
            self.timeout,
            false,
            Some(self.allow_private_ips),
        )
        .expect("reqwest client builds (user_agent must be a valid HTTP header value)");
        WebFetchTool {
            client,
            max_body_bytes: self.max_body_bytes,
            allow_domains: self.allow_domains,
            deny_domains: self.deny_domains,
            allow_private_ips: self.allow_private_ips,
            max_uses: self.max_uses,
            schema: serde_json::to_value(schemars::schema_for!(WebFetchArgs))
                .expect("WebFetchArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

/// Fetches an HTTP(S) URL, extracts the main article via Readability, and
/// returns Markdown. Enforces a host allow/deny list and a default-on SSRF
/// guard. `effect() = SideEffect` (network).
pub struct WebFetchTool<Ctx = ()> {
    client: reqwest::Client,
    max_body_bytes: usize,
    allow_domains: Option<Vec<String>>,
    deny_domains: Vec<String>,
    allow_private_ips: bool,
    max_uses: Option<usize>,
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
            max_uses: None,
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
        let (content, format) = if lower.contains("text/html")
            || lower.contains("application/xhtml")
        {
            let html = String::from_utf8_lossy(&body);
            let md = html_to_markdown(&html, Some(final_url.as_str())).map_err(ToolError::Other)?;
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

    async fn invoke(&self, ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: WebFetchArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
                schema_errors: vec![e.to_string()],
            })?;

        // Per-run use cap (run-scoped, atomic via the shared SessionState).
        if let Some(max) = self.max_uses {
            if !ctx.state().increment_u64_if_below(USES_KEY, max as u64) {
                return Err(ToolError::Denied {
                    reason: format!("WebFetch use limit reached ({max} fetches per run)"),
                });
            }
        }

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
                    .ok_or_else(|| {
                        ToolError::Other(anyhow::anyhow!("redirect without Location"))
                    })?;
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

/// Whether a (lowercased) content-type should be returned as plain text.
fn is_textual(lower: &str) -> bool {
    lower.is_empty()
        || lower.starts_with("text/")
        || lower.contains("application/json")
        || lower.contains("application/xml")
        || lower.contains("+json")
        || lower.contains("+xml")
}

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
