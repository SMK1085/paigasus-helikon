//! `LiteLlmModel` — the public [`paigasus_helikon_core::Model`] implementation.

use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::builder::{BuildError, Config, LiteLlmModelBuilder};
use crate::error::{classify, parse_retry_after_ms};
use crate::sse::StreamChunk;
use crate::stream::ChatTranslator;

/// LiteLLM proxy provider.
///
/// Construct via [`Self::chat`] or [`Self::from_env`]. Always streams.
#[derive(Debug, Clone)]
pub struct LiteLlmModel(pub(crate) Arc<Config>);

impl LiteLlmModel {
    /// Chat Completions builder for a proxy model alias.
    ///
    /// The alias is whatever the proxy operator configured — it need not be an
    /// OpenAI model id.
    pub fn chat(model_id: impl Into<String>) -> LiteLlmModelBuilder {
        LiteLlmModelBuilder::new(model_id)
    }

    /// Build from the ambient environment.
    ///
    /// Reads `LITELLM_API_BASE` (falling back to `LITELLM_PROXY_API_BASE`) and
    /// `LITELLM_API_KEY` (falling back to `LITELLM_PROXY_API_KEY`). The key is
    /// optional — an unset key yields an unauthenticated model, not an error —
    /// so the only failures are [`BuildError::MissingBaseUrl`] and
    /// [`BuildError::InvalidBaseUrl`].
    pub fn from_env(model_id: impl Into<String>) -> Result<Self, BuildError> {
        Self::chat(model_id).build()
    }

    pub(crate) fn from_config(cfg: Config) -> Self {
        Self(Arc::new(cfg))
    }

    #[cfg(test)]
    pub(crate) fn endpoint(&self) -> &str {
        &self.0.endpoint
    }

    #[cfg(test)]
    pub(crate) fn auth(&self) -> Option<&str> {
        self.0.auth.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn config_for_test(&self) -> Config {
        Config {
            http: self.0.http.clone(),
            endpoint: self.0.endpoint.clone(),
            model_id: self.0.model_id.clone(),
            auth: self.0.auth.clone(),
            headers: self.0.headers.clone(),
            capabilities: self.0.capabilities,
            extras: self.0.extras.clone(),
        }
    }
}

#[async_trait]
impl Model for LiteLlmModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let cfg = self.0.clone();
        let body = crate::translate::build_request(&cfg, &request);
        // Hoisted above the stream so a malformed configured header (e.g. an
        // API key with a trailing newline that survived `build()`, which only
        // checks non-empty) fails `invoke` eagerly with a real `ModelError`
        // instead of silently sending the request unauthenticated — see the
        // SMA-451 Task 9 review, second fix round.
        let headers = build_headers(&cfg)?;

        let s = stream! {
            let send_fut = cfg.http
                .post(&cfg.endpoint)
                .headers(headers)
                .json(&body)
                .send();

            let response = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                r = send_fut => match r {
                    Ok(r) => r,
                    Err(e) => { yield Err(ModelError::Transport(e.to_string())); return; }
                },
            };

            let status = response.status();
            let headers = response.headers().clone();

            // Correlation ids, logged on every response including streaming
            // ones. A routing chokepoint with no call id is not debuggable.
            let call_id = headers
                .get("x-litellm-call-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            tracing::debug!(
                target: "paigasus::litellm::http",
                status = status.as_u16(),
                call_id = %call_id,
                model_id = headers.get("x-litellm-model-id").and_then(|v| v.to_str().ok()).unwrap_or(""),
                attempted_retries = headers.get("x-litellm-attempted-retries").and_then(|v| v.to_str().ok()).unwrap_or(""),
                attempted_fallbacks = headers.get("x-litellm-attempted-fallbacks").and_then(|v| v.to_str().ok()).unwrap_or(""),
                "litellm response"
            );

            // A failing request returns non-2xx JSON, NOT an SSE stream —
            // check before entering the framing loop.
            let content_type = headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let is_sse = content_type.starts_with("text/event-stream");

            if !status.is_success() || !is_sse {
                let retry_after_ms = parse_retry_after_ms(&headers);
                let bytes = response.bytes().await.unwrap_or_default();
                let err = error_from_body(
                    status.as_u16(), &bytes, retry_after_ms, &call_id, &content_type,
                );
                yield Err(err);
                return;
            }

            let mut events = response.bytes_stream().eventsource();
            let mut translator = ChatTranslator::new();

            loop {
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    n = events.next() => n,
                };
                match next {
                    None => {
                        for ev in translator.finish() { yield Ok(ev); }
                        return;
                    }
                    Some(Err(e)) => {
                        yield Err(ModelError::Transport(e.to_string()));
                        return;
                    }
                    Some(Ok(event)) => {
                        if event.data == "[DONE]" {
                            for ev in translator.finish() { yield Ok(ev); }
                            return;
                        }
                        let chunk: StreamChunk = match serde_json::from_str(&event.data) {
                            Ok(c) => c,
                            Err(parse_err) => {
                                tracing::warn!(
                                    target: "paigasus::litellm::sse",
                                    %parse_err,
                                    event_len = event.data.len(),
                                    "unparseable SSE event payload; skipping"
                                );
                                continue;
                            }
                        };
                        // Defensive: a backend failing mid-generation can emit
                        // an error frame. Unverified — every reproducible
                        // failure returns non-2xx JSON before the stream
                        // opens. A JSON-null `error` — a shape several
                        // OpenAI-compatible backends emit on an otherwise
                        // healthy chunk — is NOT an error: serde already maps
                        // `null` to `None` for `Option<T>`, and the explicit
                        // `is_null()` guard here is defense in depth against
                        // that invariant ever drifting.
                        if chunk.error.as_ref().is_some_and(|e| !e.is_null()) {
                            yield Err(error_from_body(
                                500, event.data.as_bytes(), None, &call_id, &content_type,
                            ));
                            return;
                        }
                        for ev in translator.consume(chunk) {
                            yield Ok(ev);
                        }
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.0.capabilities
    }

    fn provider(&self) -> &str {
        "litellm"
    }

    fn model(&self) -> &str {
        &self.0.model_id
    }
}

/// Build the request header set: provider-computed headers first, caller
/// headers (`cfg.headers`) inserted last so they can *override* — not
/// duplicate — a same-named provider header.
///
/// Assembled as a [`reqwest::header::HeaderMap`] and applied to the request
/// via `.headers()`, which has replace semantics
/// (`http::HeaderMap::Entry::Occupied::insert` collapses to the newly
/// inserted single value). Chaining `.header()` calls on the
/// `RequestBuilder` instead would use append semantics
/// (`RequestBuilder::header_sensitive` always calls `HeaderMap::append`), so
/// a caller `.header("authorization", …)` would add a *second*
/// `Authorization` header rather than replacing the provider's. LiteLLM (a
/// Starlette app) reads only the first occurrence of a header, so that
/// second header would be silently ignored — defeating the escape hatch.
///
/// Returns `Err` if the configured API key cannot become a valid header
/// value (e.g. it carries a trailing newline from a pasted env value or a
/// `fs::read_to_string`d file — `build()` only rejects an empty/whitespace
/// key, it does not trim, so such a value survives construction). This must
/// be a hard failure, not a skip-and-warn: silently omitting `Authorization`
/// would send the request *unauthenticated* rather than with the intended
/// key, which is indistinguishable from a correct config against a keyless
/// or default-keyed proxy — wrong spend attribution and wrong routing with
/// no error anywhere. Caller headers (`cfg.headers`), by contrast, are
/// already validated at `build()` time (`builder.rs`), so a conversion
/// failure there is unreachable and stays skip-and-warn as defense in depth.
fn build_headers(cfg: &Config) -> Result<reqwest::header::HeaderMap, ModelError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("text/event-stream"),
    );

    if let Some(key) = &cfg.auth {
        let value =
            reqwest::header::HeaderValue::try_from(format!("Bearer {key}")).map_err(|e| {
                // Never interpolate `key` itself into the message — this
                // must not leak credential material into logs/error chains.
                ModelError::Other(anyhow::anyhow!(
                    "the configured api key could not be converted into a valid \
                     Authorization header value ({e}); refusing to send the \
                     request unauthenticated"
                ))
            })?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    if let Some(n) = cfg.extras.num_retries {
        // Also sent in the body. Upstream documents the header as
        // outranking the body, so the two cannot disagree. A `u8`'s
        // `Display` is always ASCII digits, so this cannot fail.
        headers.insert(
            reqwest::header::HeaderName::from_static("x-litellm-num-retries"),
            reqwest::header::HeaderValue::try_from(n.to_string())
                .expect("u8 Display is always a valid header value"),
        );
    }
    // Caller headers last: `.insert()` replaces a provider-set header of the
    // same name instead of duplicating it. `build()` already validated
    // every caller header name/value (builder.rs), so this conversion
    // should never fail — but don't unwrap blindly; skip and warn rather
    // than panic if it somehow does.
    for (name, value) in &cfg.headers {
        match (
            reqwest::header::HeaderName::try_from(name.as_str()),
            reqwest::header::HeaderValue::try_from(value.as_str()),
        ) {
            (Ok(name), Ok(value)) => {
                headers.insert(name, value);
            }
            _ => tracing::warn!(
                target: "paigasus::litellm::http",
                header = %name,
                "header is not a valid HTTP header at request time; skipping \
                 (should have been rejected at build())"
            ),
        }
    }

    Ok(headers)
}

/// Cap on how much of a response body survives into the whole-body fallback
/// message (see [`truncated_body`]).
const MAX_FALLBACK_BODY_BYTES: usize = 512;

/// Render `bytes` as lossy UTF-8, truncated to [`MAX_FALLBACK_BODY_BYTES`]
/// with an explicit elision marker.
///
/// Used only on `error_from_body`'s whole-body fallback path (no parseable
/// `error` key). A gateway returning an HTML interstitial or another large
/// payload must not turn a routine error into an unbounded `ModelError`
/// message that gets logged in full — but the truncation must be
/// unmistakable, so a caller can never read a cut-off body as a genuinely
/// short one.
fn truncated_body(bytes: &[u8]) -> String {
    if bytes.len() <= MAX_FALLBACK_BODY_BYTES {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    format!(
        "{}... [truncated, {} bytes total]",
        String::from_utf8_lossy(&bytes[..MAX_FALLBACK_BODY_BYTES]),
        bytes.len()
    )
}

/// Extract LiteLLM's error envelope and classify it.
///
/// `content_type` is the response's actual `content-type` header. It is only
/// folded into the rendered message on the whole-body fallback path (no
/// `error` key found) — the single realistic case this matters for is a 200
/// with `content-type: application/json` that never enters the SSE framing
/// loop (a gateway that silently doesn't stream, or a corporate proxy
/// returning an interstitial), where the content-type actually received is
/// the one fact an operator needs to diagnose a misconfigured gateway.
fn error_from_body(
    status: u16,
    bytes: &[u8],
    retry_after_ms: Option<u64>,
    call_id: &str,
    content_type: &str,
) -> ModelError {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(bytes).ok();
    let error_obj = parsed.as_ref().and_then(|v| v.get("error"));
    // Whether `message` below came from a genuine parsed `error.message`
    // field, as opposed to the whole-body fallback. Threaded through to
    // `classify` so its context-overflow prose match — inherently unanchored
    // substring matching — is never run against an arbitrary response body
    // (see the C1 doc comment on `classify`).
    let is_parsed_error_message = error_obj.is_some();
    let (code, err_type, message) = error_obj
        .map(|e| {
            let as_str = |k: &str| e.get(k).and_then(|x| x.as_str()).map(str::to_owned);
            (
                as_str("code"),
                as_str("type"),
                as_str("message").unwrap_or_default(),
            )
        })
        .unwrap_or_else(|| {
            let ct = if content_type.is_empty() {
                "none"
            } else {
                content_type
            };
            (
                None,
                None,
                format!("{} (content-type: {ct})", truncated_body(bytes)),
            )
        });

    let message = if call_id.is_empty() {
        message
    } else {
        format!("{message} (x-litellm-call-id: {call_id})")
    };

    classify(
        status,
        code.as_deref(),
        err_type.as_deref(),
        &message,
        is_parsed_error_message,
        retry_after_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_and_model_getters() {
        let m = LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .api_key("sk-x")
            .build()
            .unwrap();
        assert_eq!(m.provider(), "litellm");
        assert_eq!(m.model(), "prod-fast");
    }

    #[test]
    fn capabilities_default_to_streaming_and_tools() {
        let m = LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .build()
            .unwrap();
        assert!(m.capabilities().streaming);
        assert!(m.capabilities().tools);
        assert!(!m.capabilities().vision);
    }

    /// A caller `.header("authorization", …)` must replace the provider's
    /// resolved auth header, not sit alongside it as a second
    /// `Authorization` header.
    ///
    /// Regression for the pre-fix behavior: chaining `.header()` calls on
    /// `RequestBuilder` appends (`HeaderMap::append`), so the override
    /// produced two `Authorization` headers on the wire. A Starlette-based
    /// proxy like LiteLLM reads only the first occurrence — the provider's
    /// own — so the caller's override was silently ignored. `build_headers`
    /// now assembles a `HeaderMap` with caller headers inserted last
    /// (`HeaderMap::insert` replaces), which this test asserts directly on
    /// the constructed map rather than on wire bytes a mock server would
    /// receive, since header multiplicity collapses before serialization on
    /// the wire and isn't reliably observable server-side.
    #[test]
    fn caller_authorization_header_overrides_not_duplicates() {
        let m = LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .api_key("sk-provider")
            .header("authorization", "Bearer caller-override")
            .build()
            .unwrap();
        let headers = build_headers(&m.config_for_test()).unwrap();

        let values: Vec<_> = headers
            .get_all(reqwest::header::AUTHORIZATION)
            .iter()
            .collect();
        assert_eq!(
            values.len(),
            1,
            "expected exactly one Authorization header, got {values:?}"
        );
        assert_eq!(values[0], "Bearer caller-override");
    }

    /// Same override behavior for a non-auth provider-computed header.
    #[test]
    fn caller_content_type_header_overrides_not_duplicates() {
        let m = LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .header("content-type", "application/json; charset=utf-8")
            .build()
            .unwrap();
        let headers = build_headers(&m.config_for_test()).unwrap();

        let values: Vec<_> = headers
            .get_all(reqwest::header::CONTENT_TYPE)
            .iter()
            .collect();
        assert_eq!(
            values.len(),
            1,
            "expected exactly one Content-Type header, got {values:?}"
        );
        assert_eq!(values[0], "application/json; charset=utf-8");
    }

    /// A configured API key that cannot become a valid `Authorization`
    /// header value must fail `invoke` loudly — not silently send the
    /// request unauthenticated.
    ///
    /// `build()` only rejects an empty/whitespace key (`builder.rs`); it
    /// does not trim, so a key with a trailing newline (e.g. from
    /// `fs::read_to_string` on a file with a trailing newline, or a pasted
    /// env value) survives construction and only fails here, at
    /// `HeaderValue::try_from` time. Regression for the second fix round:
    /// the intermediate fix (warn-and-skip on this specific conversion)
    /// converted this into a silent credential drop — the request would
    /// still be sent, just unauthenticated, which is indistinguishable from
    /// a correct config against a keyless/default-keyed proxy.
    ///
    /// Also pins the error *variant*: `ModelError::Transport` reads as
    /// retryable to a retry decorator, which would retry a malformed key
    /// forever. This must not regress back to `Transport` (or to a silent
    /// `Ok`) if the header-construction path is touched again.
    #[test]
    fn invalid_api_key_header_value_fails_invoke_not_silently() {
        let m = LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .api_key("sk-trailing-newline\n")
            .build()
            .unwrap();
        let err = build_headers(&m.config_for_test()).unwrap_err();
        assert!(
            !matches!(err, ModelError::Transport(_)),
            "must not be classified as a retryable transport error, got {err:?}"
        );
        assert!(
            matches!(err, ModelError::Other(_)),
            "expected ModelError::Other, got {err:?}"
        );
    }

    /// A gateway returning an HTML interstitial or another large,
    /// non-`error`-shaped body must not become an unbounded `ModelError`
    /// message — `error_from_body`'s whole-body fallback must truncate it
    /// with an unmistakable elision marker, while still reporting the
    /// content-type and (when present) the call id.
    #[test]
    fn whole_body_fallback_is_truncated_with_an_elision_marker() {
        let huge_body = "z".repeat(4096);
        let err = error_from_body(400, huge_body.as_bytes(), None, "call-123", "text/html");
        match err {
            ModelError::Other(e) => {
                let msg = e.to_string();
                assert!(
                    msg.len() < huge_body.len(),
                    "message must be truncated well below the body size, got {} bytes: {msg}",
                    msg.len()
                );
                assert!(
                    msg.contains("truncated"),
                    "truncated message must carry an explicit elision marker: {msg}"
                );
                assert!(
                    msg.contains("text/html"),
                    "content-type must still be reported: {msg}"
                );
                assert!(
                    msg.contains("call-123"),
                    "x-litellm-call-id must still be appended: {msg}"
                );
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    /// A body at or under the truncation bound must be passed through
    /// verbatim — no elision marker on a genuinely short body.
    #[test]
    fn short_whole_body_fallback_is_not_truncated() {
        let body = "not json";
        let err = error_from_body(400, body.as_bytes(), None, "", "text/plain");
        match err {
            ModelError::Other(e) => {
                let msg = e.to_string();
                assert!(msg.contains(body));
                assert!(
                    !msg.contains("truncated"),
                    "a short body must not carry an elision marker: {msg}"
                );
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }
}
