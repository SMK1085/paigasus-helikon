//! `LiteLlmModelBuilder` — fluent constructor for [`crate::LiteLlmModel`].
//!
//! `base_url` is required (explicit, else `LITELLM_API_BASE`, else
//! `LITELLM_PROXY_API_BASE`); auth is optional. See the SMA-451 design §5–§7.

use paigasus_helikon_core::ModelCapabilities;
use serde_json::{Map, Value};

use crate::capabilities::{apply_override, conservative_defaults};
use crate::transport::{normalise_endpoint, DEFAULT_CHAT_PATH};

/// Request-body keys the provider computes per-request.
///
/// `.extra_body()` may not set these — letting a caller forge them would make
/// the translator's output unpredictable. The LiteLLM extras (`fallbacks`,
/// `num_retries`, `metadata`, `tags`) are deliberately **absent**: those are
/// the keys whose upstream wire shape is least certain, so `extra_body` must
/// stay a usable escape hatch for them.
pub(crate) const RESERVED_BODY_KEYS: &[&str] = &[
    "model",
    "messages",
    "stream",
    "stream_options",
    "tools",
    "tool_choice",
    "response_format",
    "temperature",
    "top_p",
    "max_tokens",
    "n",
];

/// LiteLLM-specific request fields.
#[derive(Debug, Clone, Default)]
pub(crate) struct Extras {
    pub(crate) fallbacks: Vec<String>,
    pub(crate) num_retries: Option<u8>,
    pub(crate) metadata: Map<String, Value>,
    pub(crate) tags: Vec<String>,
    pub(crate) extra_body: Map<String, Value>,
}

/// Resolved, immutable model configuration.
pub(crate) struct Config {
    pub(crate) http: reqwest::Client,
    pub(crate) endpoint: String,
    pub(crate) model_id: String,
    /// Bearer credential, or `None` for an unauthenticated proxy.
    pub(crate) auth: Option<String>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) capabilities: ModelCapabilities,
    pub(crate) extras: Extras,
}

/// Header names whose values must never appear in `Debug` output.
fn is_secret_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "authorization"
        || n == "proxy-authorization"
        || n == "api-key"
        || n == "x-api-key"
        || n == "x-litellm-api-key"
        || n.starts_with("x-litellm-key")
}

/// Render `endpoint` with any URL userinfo (`user:pass@…`) blanked.
///
/// A basic-auth-fronted LiteLLM proxy (`http://admin:s3cret@litellm:4000`)
/// is a supported deployment shape — `normalise_endpoint` doesn't reject
/// userinfo, and reqwest honours it — so the credential must be stripped
/// here rather than assumed absent. Falls back to the raw string if
/// `endpoint` doesn't reparse.
///
/// Two call sites, two different guarantees about that fallback: [`Config`]'s
/// `Debug` impl calls this on an already-`normalise_endpoint`d string, which
/// always reparses (it came from that function's own `Url::to_string()`).
/// `build()`'s `InvalidBaseUrl` error path calls this on the *raw,
/// unvalidated* `base_url` input — which may be genuinely unparseable (e.g.
/// `"not a url"`) — precisely because that input already failed
/// `normalise_endpoint`. The fallback exists for that second case: it cannot
/// panic on unparseable input (`Url::parse` returns `Result`, never panics),
/// and a string with no parseable userinfo has no credential to leak in the
/// first place, so returning it verbatim is safe.
fn redact_endpoint(endpoint: &str) -> String {
    match reqwest::Url::parse(endpoint) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.to_string()
        }
        Err(_) => endpoint.to_owned(),
    }
}

/// `Debug` view of [`Extras`] that shows key names/counts, not values.
///
/// `metadata` and `extra_body` are arbitrary caller-supplied JSON — LiteLLM's
/// own request passthrough accepts per-request provider credentials there
/// (`api_key`, `aws_*`, `vertex_credentials`, …), none of which are in
/// [`RESERVED_BODY_KEYS`], so a value-printing `Debug` would leak them.
struct RedactedExtras<'a>(&'a Extras);

impl std::fmt::Debug for RedactedExtras<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extras")
            .field("fallbacks", &self.0.fallbacks)
            .field("num_retries", &self.0.num_retries)
            .field("metadata_keys", &self.0.metadata.keys().collect::<Vec<_>>())
            .field("tags", &self.0.tags)
            .field(
                "extra_body_keys",
                &self.0.extra_body.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str(),
                    if is_secret_header(k) {
                        "<redacted>"
                    } else {
                        v.as_str()
                    },
                )
            })
            .collect();
        f.debug_struct("Config")
            .field("endpoint", &redact_endpoint(&self.endpoint))
            .field("model_id", &self.model_id)
            .field("auth", &self.auth.as_ref().map(|_| "<redacted>"))
            .field("headers", &headers)
            .field("capabilities", &self.capabilities)
            .field("extras", &RedactedExtras(&self.extras))
            .finish()
    }
}

/// Construction-time errors. Runtime errors flow through
/// [`paigasus_helikon_core::ModelError`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// Neither `.base_url()` nor `LITELLM_API_BASE`/`LITELLM_PROXY_API_BASE`
    /// supplied a proxy address.
    #[error("no base URL: set .base_url() or LITELLM_API_BASE")]
    MissingBaseUrl,
    /// `base_url` was unparseable, used a scheme other than http/https, or
    /// carried a query string or fragment.
    ///
    /// The carried string has any URL userinfo (`user:pass@…`) redacted —
    /// `redact_endpoint` is applied to the raw `base_url` input before it is
    /// stored here, so a basic-auth-fronted proxy URL that was rejected for
    /// an unrelated reason (e.g. a query string) does not leak its password
    /// into an error a caller will log.
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    /// `.extra_body()` set a key the provider computes per-request.
    #[error("extra_body may not set the reserved key `{0}`")]
    ReservedExtraBodyKey(String),
    /// `.extra_body()` was given a non-object JSON value.
    #[error("extra_body must be a JSON object")]
    ExtraBodyNotAnObject,
    /// `.metadata()` set a key the provider owns — use `.tags()` instead.
    #[error("metadata key `{0}` is reserved; use the dedicated builder method")]
    ReservedMetadataKey(String),
    /// `.header()` name or value is not a valid HTTP header.
    #[error("invalid header: {0}")]
    InvalidHeader(String),
}

#[derive(Debug, Clone)]
enum Auth {
    None,
    Key(String),
}

/// Fluent builder for [`crate::LiteLlmModel`].
pub struct LiteLlmModelBuilder {
    model_id: String,
    base_url: Option<String>,
    chat_path: String,
    auth: Auth,
    http_client: Option<reqwest::Client>,
    headers: Vec<(String, String)>,
    capabilities_override: Option<ModelCapabilities>,
    extras: Extras,
    extra_body_raw: Option<Value>,
    metadata_pairs: Vec<(String, Value)>,
}

impl std::fmt::Debug for LiteLlmModelBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiteLlmModelBuilder")
            .field("model_id", &self.model_id)
            .field("base_url", &self.base_url)
            .field("chat_path", &self.chat_path)
            .field(
                "auth",
                &match self.auth {
                    Auth::None => "None",
                    Auth::Key(_) => "<redacted>",
                },
            )
            .finish_non_exhaustive()
    }
}

impl LiteLlmModelBuilder {
    pub(crate) fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            base_url: None,
            chat_path: DEFAULT_CHAT_PATH.to_owned(),
            auth: Auth::None,
            http_client: None,
            headers: Vec::new(),
            capabilities_override: None,
            extras: Extras::default(),
            extra_body_raw: None,
            metadata_pairs: Vec::new(),
        }
    }

    /// Proxy base URL, e.g. `http://litellm:4000`. Required.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Virtual key / API key. Last-set auth wins. Empty values are ignored.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.auth = Auth::Key(key.into());
        self
    }

    /// Pre-minted bearer token. Last-set auth wins. Empty values are ignored.
    pub fn bearer(mut self, token: impl Into<String>) -> Self {
        self.auth = Auth::Key(token.into());
        self
    }

    /// Override the request path appended to `base_url`.
    ///
    /// Escape hatch for gateways the `/v1` normalisation heuristic gets wrong.
    pub fn chat_completions_path(mut self, path: impl Into<String>) -> Self {
        self.chat_path = path.into();
        self
    }

    /// Add an arbitrary request header, e.g. `x-litellm-tags`.
    ///
    /// Accumulates: each call adds another header rather than replacing an
    /// earlier one. A later call using the same name still wins on the wire,
    /// since headers are applied via a replacing `HeaderMap::insert` (see
    /// `model.rs::build_headers`), not appended.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Use a caller-provided `reqwest::Client`.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Declare what the proxied backend can do. Wins over the conservative
    /// defaults, except that `streaming` is always forced on.
    pub fn with_capabilities(mut self, caps: ModelCapabilities) -> Self {
        self.capabilities_override = Some(caps);
        self
    }

    /// LiteLLM router fallback model names, tried in order.
    ///
    /// Replaces: only the most recent call's list is used. Calling this
    /// twice discards the first call's list entirely rather than merging.
    pub fn fallbacks<I, S>(mut self, models: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extras.fallbacks = models.into_iter().map(Into::into).collect();
        self
    }

    /// LiteLLM server-side retry count.
    ///
    /// Note this composes multiplicatively with a client-side retry decorator;
    /// see the crate README.
    pub fn num_retries(mut self, n: u8) -> Self {
        self.extras.num_retries = Some(n);
        self
    }

    /// Add a LiteLLM `metadata` entry (spend logs, tracing correlation).
    ///
    /// The key `tags` is reserved — use [`Self::tags`].
    ///
    /// Accumulates: each call adds another metadata entry. A later call
    /// using the same key overwrites the earlier value for that key rather
    /// than being rejected.
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.metadata_pairs.push((key.into(), value.into()));
        self
    }

    /// LiteLLM routing/spend tags, emitted as `metadata.tags`.
    ///
    /// Replaces: only the most recent call's list is used. Calling this
    /// twice discards the first call's list entirely rather than merging.
    pub fn tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extras.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Arbitrary extra request-body fields, merged at the root.
    ///
    /// Rejected at build time if it sets a key the provider computes
    /// per-request. The LiteLLM extras are *not* reserved.
    ///
    /// Replaces: only the most recent call's JSON object is used. Calling
    /// this twice discards the first call's object entirely rather than
    /// merging the two.
    pub fn extra_body(mut self, value: Value) -> Self {
        self.extra_body_raw = Some(value);
        self
    }

    /// Validate everything and produce the model.
    pub fn build(self) -> Result<crate::LiteLlmModel, BuildError> {
        let base = self
            .base_url
            .or_else(|| non_empty_env("LITELLM_API_BASE"))
            .or_else(|| non_empty_env("LITELLM_PROXY_API_BASE"))
            .ok_or(BuildError::MissingBaseUrl)?;

        let endpoint = normalise_endpoint(&base, &self.chat_path)
            .map_err(|_| BuildError::InvalidBaseUrl(redact_endpoint(&base)))?;

        // Auth: explicit wins; empty/whitespace is treated as absent so we
        // never emit a malformed `Bearer ` header, and so an
        // unset-but-declared env var (e.g. `LITELLM_API_KEY=` from
        // docker-compose/.env) doesn't shadow a real key further down the
        // fallback chain.
        let auth = match self.auth {
            Auth::Key(k) => Some(k),
            Auth::None => {
                non_empty_env("LITELLM_API_KEY").or_else(|| non_empty_env("LITELLM_PROXY_API_KEY"))
            }
        }
        .filter(|k| !k.trim().is_empty());

        for (name, value) in &self.headers {
            if reqwest::header::HeaderName::try_from(name.as_str()).is_err() {
                return Err(BuildError::InvalidHeader(name.clone()));
            }
            if reqwest::header::HeaderValue::try_from(value.as_str()).is_err() {
                return Err(BuildError::InvalidHeader(format!("value for `{name}`")));
            }
        }

        let mut extras = self.extras;

        for (k, v) in self.metadata_pairs {
            if k == "tags" {
                return Err(BuildError::ReservedMetadataKey(k));
            }
            extras.metadata.insert(k, v);
        }

        if let Some(raw) = self.extra_body_raw {
            let obj = match raw {
                Value::Object(m) => m,
                _ => return Err(BuildError::ExtraBodyNotAnObject),
            };
            for key in obj.keys() {
                if RESERVED_BODY_KEYS.contains(&key.as_str()) {
                    return Err(BuildError::ReservedExtraBodyKey(key.clone()));
                }
            }
            extras.extra_body = obj;
        }

        let http = self.http_client.unwrap_or_else(default_http_client);
        let capabilities = apply_override(conservative_defaults(), self.capabilities_override);

        Ok(crate::LiteLlmModel::from_config(Config {
            http,
            endpoint,
            model_id: self.model_id,
            auth,
            headers: self.headers,
            capabilities,
            extras,
        }))
    }
}

/// Read an env var, treating unset *and* empty/whitespace-only as absent.
///
/// `std::env::var` returns `Ok("")` for a var declared but set to the empty
/// string (the shape docker-compose/`.env` produce for an unset-but-declared
/// variable) — filtering per-source here, before the fallback chain's
/// `or_else`, keeps an empty first var from shadowing a real value in a
/// later one.
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Default HTTP client: a connect timeout so a hung proxy cannot hang
/// `invoke` indefinitely, and redirects disabled so an authenticated POST is
/// never silently replayed elsewhere. No overall request timeout — a long
/// generation is not a hang.
fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest client with connect timeout and no redirects")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    // Serialize env-mutating tests, matching providers-openai/src/builder.rs.
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn clear_env() {
        for k in [
            "LITELLM_API_BASE",
            "LITELLM_PROXY_API_BASE",
            "LITELLM_API_KEY",
            "LITELLM_PROXY_API_KEY",
        ] {
            std::env::remove_var(k);
        }
    }

    fn b() -> LiteLlmModelBuilder {
        LiteLlmModelBuilder::new("prod-fast")
    }

    #[test]
    fn missing_base_url_is_an_error() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        assert!(matches!(b().build(), Err(BuildError::MissingBaseUrl)));
    }

    #[test]
    fn base_url_falls_back_to_litellm_api_base() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("LITELLM_API_BASE", "http://from-env:4000");
        let m = b().build().expect("env base_url should be used");
        assert_eq!(m.endpoint(), "http://from-env:4000/v1/chat/completions");
        clear_env();
    }

    #[test]
    fn base_url_falls_back_to_litellm_proxy_api_base() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("LITELLM_PROXY_API_BASE", "http://proxy-env:4000");
        let m = b().build().expect("proxy env base_url should be used");
        assert_eq!(m.endpoint(), "http://proxy-env:4000/v1/chat/completions");
        clear_env();
    }

    #[test]
    fn empty_api_base_env_falls_through_to_proxy_api_base_env() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        // The shape docker-compose/.env produce for an unset-but-declared
        // variable: `Ok("")`, not "unset".
        std::env::set_var("LITELLM_API_BASE", "");
        std::env::set_var("LITELLM_PROXY_API_BASE", "http://p:4000");
        let m = b()
            .build()
            .expect("empty LITELLM_API_BASE must fall through to the proxy env base");
        assert_eq!(m.endpoint(), "http://p:4000/v1/chat/completions");
        clear_env();
    }

    #[test]
    fn invalid_base_url_is_an_error() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        assert!(matches!(
            b().base_url("localhost:4000").build(),
            Err(BuildError::InvalidBaseUrl(_))
        ));
    }

    /// `BuildError::InvalidBaseUrl` must not leak a basic-auth password from
    /// a rejected `base_url` — the crate redacts endpoint credentials
    /// everywhere else (`Config`'s `Debug` impl), and a caller logging this
    /// error must get the same guarantee.
    #[test]
    fn invalid_base_url_error_redacts_userinfo_credentials() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        // Rejected for carrying a query string, not for the userinfo — the
        // password must still not survive into the rendered error.
        let err = b()
            .base_url("http://admin:s3cret@gw:4000/x?k=1")
            .build()
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains("s3cret"),
            "InvalidBaseUrl must not leak the endpoint's basic-auth password: {msg}"
        );
        assert!(
            matches!(err, BuildError::InvalidBaseUrl(_)),
            "expected InvalidBaseUrl, got {err:?}"
        );
    }

    /// `redact_endpoint`'s fallback branch (raw string, unparseable input)
    /// must not panic when reached via the `InvalidBaseUrl` path — unlike
    /// `Config`'s `Debug` impl, which only ever calls it on an
    /// already-`normalise_endpoint`d string, `build()` calls it on the raw,
    /// unvalidated `base_url`, which can be genuinely unparseable.
    #[test]
    fn invalid_base_url_error_does_not_panic_on_unparseable_input() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        let err = b().base_url("not a url").build().unwrap_err();
        assert!(matches!(err, BuildError::InvalidBaseUrl(_)));
    }

    #[test]
    fn auth_is_optional_and_absent_means_no_header() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        let m = b().base_url("http://p:4000").build().unwrap();
        assert!(m.auth().is_none(), "no key configured means no header");
    }

    #[test]
    fn empty_and_whitespace_keys_are_treated_as_absent() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        for key in ["", "   ", "\t"] {
            let m = b().base_url("http://p:4000").api_key(key).build().unwrap();
            assert!(
                m.auth().is_none(),
                "empty/whitespace key must not produce `Bearer `"
            );
        }
    }

    #[test]
    fn api_key_env_fallbacks_are_honoured() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("LITELLM_PROXY_API_KEY", "sk-proxy");
        let m = b().base_url("http://p:4000").build().unwrap();
        assert_eq!(m.auth(), Some("sk-proxy"));
        clear_env();
    }

    #[test]
    fn empty_api_key_env_falls_through_to_proxy_api_key_env() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("LITELLM_API_KEY", "");
        std::env::set_var("LITELLM_PROXY_API_KEY", "sk-real");
        let m = b().base_url("http://p:4000").build().unwrap();
        assert_eq!(
            m.auth(),
            Some("sk-real"),
            "an empty LITELLM_API_KEY must not shadow a real LITELLM_PROXY_API_KEY"
        );
        clear_env();
    }

    #[test]
    fn last_set_auth_wins() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        let m = b()
            .base_url("http://p:4000")
            .api_key("first")
            .bearer("second")
            .build()
            .unwrap();
        assert_eq!(m.auth(), Some("second"));
    }

    #[test]
    fn extra_body_must_be_an_object() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        let err = b()
            .base_url("http://p:4000")
            .extra_body(serde_json::json!([1, 2, 3]))
            .build()
            .unwrap_err();
        assert!(matches!(err, BuildError::ExtraBodyNotAnObject));
    }

    #[test]
    fn every_reserved_key_is_rejected_in_extra_body() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        for key in RESERVED_BODY_KEYS {
            let err = b()
                .base_url("http://p:4000")
                .extra_body(serde_json::json!({ *key: "x" }))
                .build()
                .unwrap_err();
            match err {
                BuildError::ReservedExtraBodyKey(k) => assert_eq!(&k, *key),
                other => panic!("expected ReservedExtraBodyKey for {key}, got {other:?}"),
            }
        }
    }

    #[test]
    fn litellm_extras_are_not_reserved_in_extra_body() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        // These are the keys whose upstream shape is least certain, so
        // extra_body must remain a usable escape hatch for them (§7.2).
        for key in ["fallbacks", "num_retries", "metadata", "tags"] {
            assert!(
                b().base_url("http://p:4000")
                    .extra_body(serde_json::json!({ key: "x" }))
                    .build()
                    .is_ok(),
                "{key} must NOT be reserved"
            );
        }
    }

    #[test]
    fn metadata_tags_key_is_rejected_with_its_own_variant() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        let err = b()
            .base_url("http://p:4000")
            .metadata("tags", "oops")
            .build()
            .unwrap_err();
        match err {
            BuildError::ReservedMetadataKey(k) => assert_eq!(k, "tags"),
            other => panic!("expected ReservedMetadataKey, got {other:?}"),
        }
    }

    #[test]
    fn invalid_header_name_or_value_is_rejected() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        assert!(matches!(
            b().base_url("http://p:4000")
                .header("bad header name", "v")
                .build(),
            Err(BuildError::InvalidHeader(_))
        ));
        assert!(matches!(
            b().base_url("http://p:4000")
                .header("x-ok", "bad\nvalue")
                .build(),
            Err(BuildError::InvalidHeader(_))
        ));
    }

    #[test]
    fn debug_output_redacts_the_api_key() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        let m = b()
            .base_url("http://p:4000")
            .api_key("sk-super-secret-value")
            .header("authorization", "Bearer another-secret")
            .build()
            .unwrap();
        let dbg = format!("{m:?}");
        assert!(
            !dbg.contains("sk-super-secret-value"),
            "api key leaked into Debug: {dbg}"
        );
        assert!(
            !dbg.contains("another-secret"),
            "auth header value leaked into Debug: {dbg}"
        );
    }

    #[test]
    fn debug_output_redacts_endpoint_userinfo_and_extra_body_values() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        // Basic-auth-fronted proxy: URL userinfo carries a credential too.
        // extra_body's `api_key` is LiteLLM passthrough shape, not in
        // RESERVED_BODY_KEYS, so it's a legal (and realistic) escape-hatch
        // value that must not be printed.
        let m = b()
            .base_url("http://admin:s3cret-pw@p:4000")
            .extra_body(serde_json::json!({ "api_key": "sk-passthrough-secret" }))
            .build()
            .unwrap();
        let dbg = format!("{m:?}");
        assert!(
            !dbg.contains("s3cret-pw"),
            "endpoint userinfo password leaked into Debug: {dbg}"
        );
        assert!(
            !dbg.contains("admin"),
            "endpoint userinfo username leaked into Debug: {dbg}"
        );
        assert!(
            !dbg.contains("sk-passthrough-secret"),
            "extra_body value leaked into Debug: {dbg}"
        );
        assert!(
            dbg.contains("api_key"),
            "extra_body key name should still be visible for debugging: {dbg}"
        );
    }

    /// Table-driven over every entry `is_secret_header` recognizes (plus a
    /// mixed-case spelling and a non-exact `x-litellm-key*` prefix match), so
    /// no entry can be silently deleted from the list without a test
    /// noticing. Regression for `proxy-authorization` and
    /// `x-litellm-api-key` — LiteLLM's documented alternate key header —
    /// which were previously missing while only `authorization` had test
    /// coverage.
    #[test]
    fn debug_output_redacts_every_secret_header_entry() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        const SECRET_HEADER_NAMES: &[&str] = &[
            "authorization",
            "Authorization",
            "proxy-authorization",
            "api-key",
            "x-api-key",
            "x-litellm-api-key",
            "x-litellm-key",
            "x-litellm-key-alt",
        ];
        for name in SECRET_HEADER_NAMES {
            let secret_value = format!(
                "secret-value-for-{}",
                name.to_ascii_lowercase().replace(['-', ' '], "_")
            );
            let m = b()
                .base_url("http://p:4000")
                .header(*name, secret_value.clone())
                .build()
                .unwrap();
            let dbg = format!("{m:?}");
            assert!(
                !dbg.contains(&secret_value),
                "header `{name}` was not redacted in Debug output: {dbg}"
            );
        }
    }
}
