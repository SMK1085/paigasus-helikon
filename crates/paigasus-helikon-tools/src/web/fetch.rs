//! [`WebFetchTool`] — HTTP(S) fetch → Readability → Markdown, with a host
//! allow/deny list and a default-on SSRF guard.

/// Extract the main article from `html` via Readability, then convert it to
/// Markdown. `base_url` improves Readability's relative-link handling. Errors
/// are operational (the page was fetched, but could not be parsed).
#[allow(dead_code)] // consumed by WebFetchTool::finish in Task 4 (SMA-412)
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
