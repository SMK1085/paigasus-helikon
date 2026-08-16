//! Wire-format / transport tests for the LiteLLM provider.

use std::sync::{Mutex, OnceLock};

use futures_util::StreamExt;
use paigasus_helikon_core::{CancellationToken, ContentPart, Item, Model, ModelRequest};
use paigasus_helikon_providers_litellm::LiteLlmModel;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Serializes tests in this binary that mutate the LiteLLM auth env vars,
/// matching the pattern in `src/builder.rs`'s test module.
static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
fn env_lock() -> &'static Mutex<()> {
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

/// Env vars that can supply an auth key, checked/cleared/restored by
/// [`no_authorization_header_when_no_key_is_configured`].
const AUTH_ENV_KEYS: [&str; 2] = ["LITELLM_API_KEY", "LITELLM_PROXY_API_KEY"];

fn sse_ok() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n\
             data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
            "text/event-stream",
        )
}

fn user(s: &str) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text { text: s.into() }],
    }];
    r
}

async fn drain(model: &LiteLlmModel) {
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();
    while s.next().await.is_some() {}
}

#[tokio::test]
async fn posts_to_v1_chat_completions_with_sse_accept() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("accept", "text/event-stream"))
        .and(header("content-type", "application/json"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .api_key("sk-test")
        .build()
        .unwrap();
    drain(&model).await;
}

#[tokio::test]
async fn base_url_already_ending_in_v1_does_not_double_the_segment() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(format!("{}/v1", server.uri()))
        .build()
        .unwrap();
    drain(&model).await;
}

#[tokio::test]
async fn authorization_header_is_sent_when_a_key_is_configured() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .api_key("sk-test")
        .build()
        .unwrap();
    drain(&model).await;
}

/// The security-relevant assertion for optional auth.
///
/// **This must inspect `received_requests()`.** Wiremock has no negative
/// header matcher, so a `Mock::given(method("POST"))` with no header condition
/// matches whether or not the header was sent — an implementation that always
/// sent auth would pass such a test.
#[tokio::test]
async fn no_authorization_header_when_no_key_is_configured() {
    // Ensure ambient env vars cannot supply a key, guarded against other
    // tests in this binary that might read/set them concurrently, and
    // restore whatever was there afterward. A std::sync::Mutex guard must
    // not be held across an .await, so the lock only brackets the env
    // mutation and its restoration, not the async body in between.
    let saved: Vec<(&str, Option<String>)> = {
        let _g = env_lock().lock().unwrap();
        let saved = AUTH_ENV_KEYS
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        for k in AUTH_ENV_KEYS {
            std::env::remove_var(k);
        }
        saved
    };

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();
    drain(&model).await;

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].headers.get("authorization").is_none(),
        "no Authorization header must be sent when no key is configured"
    );

    let _g = env_lock().lock().unwrap();
    for (k, v) in saved {
        match v {
            Some(val) => std::env::set_var(k, val),
            None => std::env::remove_var(k),
        }
    }
}

#[tokio::test]
async fn num_retries_is_sent_in_both_body_and_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .num_retries(3)
        .build()
        .unwrap();
    drain(&model).await;

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["num_retries"], 3);
    assert_eq!(
        requests[0].headers.get("x-litellm-num-retries").unwrap(),
        "3"
    );
}

#[tokio::test]
async fn custom_headers_are_passed_through() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("x-litellm-tags", "free"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .header("x-litellm-tags", "free")
        .build()
        .unwrap();
    drain(&model).await;
}

/// A failing request returns non-2xx JSON, not an SSE stream.
#[tokio::test]
async fn non_sse_error_response_yields_a_single_classified_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(500)
                .insert_header("content-type", "application/json")
                .set_body_raw(
                    r#"{"error":{"message":"litellm.InternalServerError: mock","type":null,"param":null,"code":"500"}}"#,
                    "application/json",
                ),
        )
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();

    let first = s.next().await.expect("one event");
    assert!(matches!(
        first,
        Err(paigasus_helikon_core::ModelError::Unavailable)
    ));
    assert!(s.next().await.is_none(), "error must terminate the stream");
}

/// A 200 with `content-type: application/json` — the realistic shape for a
/// gateway that silently doesn't stream, or a corporate proxy returning a
/// JSON interstitial — must be rejected by the `is_sse` half of the response
/// gate, not treated as an empty SSE stream.
///
/// Without `|| !is_sse` in `model.rs`'s response gate, `eventsource()` would
/// find no `data:` frames in this body, hit EOF immediately, and
/// `ChatTranslator::finish()` would yield an empty stream — no error, no
/// `Finish` event, silent data loss that every other test in this file
/// cannot detect (they all exercise the gate via a non-2xx status, the one
/// case where gated and un-gated behaviour coincide).
#[tokio::test]
async fn success_status_with_non_sse_content_type_yields_a_single_classified_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_raw(
                    r#"{"id":"chatcmpl-1","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#,
                    "application/json",
                ),
        )
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();

    let first = s.next().await.expect("one event");
    match first {
        Err(paigasus_helikon_core::ModelError::Other(e)) => {
            let msg = e.to_string();
            assert!(
                msg.contains("application/json"),
                "the error must report the content-type actually received so an \
                 operator can diagnose a misconfigured gateway: {msg}"
            );
        }
        other => panic!("expected a classified Other error, got {other:?}"),
    }
    assert!(s.next().await.is_none(), "error must terminate the stream");
}

/// A non-`{"error":...}` failure body containing incidental prose like
/// "context window" (e.g. an echoed prompt on an unrelated 4xx) must not be
/// misclassified as `ContextLengthExceeded` — the prose match only applies
/// to a genuinely parsed `error.message` field, never to the whole-body
/// fallback used when the response carries no `error` key at all.
#[tokio::test]
async fn unrelated_error_body_mentioning_context_window_is_not_misclassified() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("content-type", "application/json")
                .set_body_raw(
                    r#"{"prompt_echo":"please discuss expanding the context window of the office"}"#,
                    "application/json",
                ),
        )
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();

    let first = s.next().await.expect("one event");
    assert!(
        !matches!(
            first,
            Err(paigasus_helikon_core::ModelError::ContextLengthExceeded)
        ),
        "an unrelated error body must not be misclassified as \
         ContextLengthExceeded: {first:?}"
    );
}

#[tokio::test]
async fn rate_limit_carries_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("content-type", "application/json")
                .insert_header("retry-after", "2")
                .set_body_raw(
                    r#"{"error":{"message":"litellm.RateLimitError: mock","type":"throttling_error","code":"429"}}"#,
                    "application/json",
                ),
        )
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();

    match s.next().await.unwrap() {
        Err(paigasus_helikon_core::ModelError::RateLimited { retry_after_ms }) => {
            assert_eq!(retry_after_ms, Some(2000));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}
