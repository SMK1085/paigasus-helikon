//! Endpoint URL construction.
//!
//! LiteLLM serves both `/chat/completions` and `/v1/chat/completions`, and
//! operators write base URLs both ways, so [`normalise_endpoint`] accepts
//! either. See the SMA-451 design §6.

/// Default request path appended to the configured base URL.
pub(crate) const DEFAULT_CHAT_PATH: &str = "/v1/chat/completions";

/// A `base_url` that could not be turned into a request endpoint.
#[derive(Debug)]
pub(crate) struct UrlError;

/// Resolve `base_url` + `path` into an absolute endpoint URL.
///
/// Operates on parsed URL path segments rather than raw strings: string
/// concatenation would mangle inputs carrying a query, and the `Url` API
/// makes the trailing-segment rules total.
///
/// Rejects any scheme other than `http`/`https`, and any URL carrying a
/// query or fragment. The scheme check is load-bearing —
/// `Url::parse("localhost:4000")` *succeeds*, parsing `localhost` as the
/// scheme and `4000` as the path, so without it the single most likely
/// operator typo would sail through.
///
/// A trailing `v1` segment on `base_url` is dropped before `path` is
/// appended, but **only when `path` itself starts with a `v1` segment** — the
/// default `path`, `/v1/chat/completions`, does — so a base URL of either
/// `http://host` or `http://host/v1` combined with the default path lands on
/// the same endpoint rather than doubling the `v1` segment.
///
/// A custom `path` (via
/// [`LiteLlmModelBuilder::chat_completions_path`](crate::LiteLlmModelBuilder::chat_completions_path))
/// that does *not* start with `v1` leaves a trailing `v1` on `base_url`
/// untouched, so operators mounted at a gateway path like
/// `/openai/deployments/x/chat/completions` under a base ending in `/v1` get
/// exactly that base preserved — this is the escape hatch the builder method
/// and the crate README document; popping unconditionally would silently
/// delete an operator's explicit `v1` segment regardless of what `path` asked
/// for.
pub(crate) fn normalise_endpoint(base_url: &str, path: &str) -> Result<String, UrlError> {
    let mut url = reqwest::Url::parse(base_url).map_err(|_| UrlError)?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlError);
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(UrlError);
    }

    let mut segments: Vec<String> = url
        .path_segments()
        .map(|s| s.filter(|seg| !seg.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default();
    let path_segments: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if segments.last().map(String::as_str) == Some("v1")
        && path_segments.first().map(String::as_str) == Some("v1")
    {
        segments.pop();
    }
    segments.extend(path_segments);

    {
        let mut segs = url.path_segments_mut().map_err(|_| UrlError)?;
        segs.clear();
        for seg in &segments {
            segs.push(seg);
        }
    }

    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(u: &str) -> Result<String, UrlError> {
        normalise_endpoint(u, DEFAULT_CHAT_PATH)
    }

    #[test]
    fn appends_v1_chat_completions_to_a_bare_host() {
        assert_eq!(
            norm("http://localhost:4000").unwrap(),
            "http://localhost:4000/v1/chat/completions"
        );
    }

    #[test]
    fn tolerates_a_trailing_slash() {
        assert_eq!(
            norm("http://localhost:4000/").unwrap(),
            "http://localhost:4000/v1/chat/completions"
        );
    }

    #[test]
    fn does_not_double_the_v1_segment() {
        assert_eq!(
            norm("http://localhost:4000/v1").unwrap(),
            "http://localhost:4000/v1/chat/completions"
        );
        assert_eq!(
            norm("http://localhost:4000/v1/").unwrap(),
            "http://localhost:4000/v1/chat/completions"
        );
    }

    #[test]
    fn preserves_a_mount_path() {
        assert_eq!(
            norm("https://gw.example.com/litellm").unwrap(),
            "https://gw.example.com/litellm/v1/chat/completions"
        );
    }

    #[test]
    fn rejects_a_scheme_less_host() {
        // `Url::parse("localhost:4000")` SUCCEEDS with scheme `localhost`,
        // so this case is only caught by the explicit scheme check.
        assert!(norm("localhost:4000").is_err());
    }

    #[test]
    fn rejects_a_non_http_scheme() {
        assert!(norm("ftp://gw.example.com").is_err());
        assert!(norm("file:///etc/passwd").is_err());
    }

    #[test]
    fn rejects_a_query_or_fragment() {
        assert!(norm("http://gw/litellm?key=x").is_err());
        assert!(norm("http://gw/litellm#frag").is_err());
    }

    #[test]
    fn rejects_unparseable_input() {
        assert!(norm("not a url").is_err());
    }

    #[test]
    fn honours_a_custom_path() {
        assert_eq!(
            normalise_endpoint("http://gw:4000", "/openai/deployments/x/chat/completions").unwrap(),
            "http://gw:4000/openai/deployments/x/chat/completions"
        );
    }

    /// A custom `.chat_completions_path()` that does NOT start with `v1` must
    /// preserve a `/v1` already present on `base_url` — this is the escape
    /// hatch the builder method and the crate README document
    /// ("override the whole path"). Regression for the unconditional-pop
    /// bug: `honours_a_custom_path` above uses a base with no `/v1` at all,
    /// so it cannot see a base's `v1` being silently deleted.
    #[test]
    fn a_custom_path_preserves_an_explicit_v1_on_the_base() {
        assert_eq!(
            normalise_endpoint(
                "https://gw.example.com/v1",
                "/openai/deployments/x/chat/completions"
            )
            .unwrap(),
            "https://gw.example.com/v1/openai/deployments/x/chat/completions"
        );
    }
}
