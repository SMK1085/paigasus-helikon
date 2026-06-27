//! Wire-format / transport tests for the Gemini provider.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelError, ModelRequest,
};
use paigasus_helikon_providers_gemini::GeminiModel;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse_ok() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}\n\n",
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

#[tokio::test]
async fn developer_url_and_api_key_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent",
        ))
        .and(query_param("alt", "sse"))
        .and(header("x-goog-api-key", "sk-test"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = GeminiModel::developer("gemini-2.5-flash")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();
    let mut texts = Vec::new();
    while let Some(ev) = s.next().await {
        if let Ok(paigasus_helikon_core::ModelEvent::TokenDelta { text }) = ev {
            texts.push(text);
        }
    }
    assert_eq!(texts, vec!["hi"]);
}

#[tokio::test]
async fn vertex_url_and_bearer_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/projects/proj/locations/us-central1/publishers/google/models/gemini-2.5-pro:streamGenerateContent"))
        .and(query_param("alt", "sse"))
        .and(header("authorization", "Bearer ya29.token"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = GeminiModel::vertex("gemini-2.5-pro", "proj", "us-central1")
        .bearer_token("ya29.token")
        .base_url(server.uri())
        .build()
        .unwrap();
    let s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();
    let evs: Vec<_> = s.collect().await;
    assert!(!evs.is_empty());
}

#[tokio::test]
async fn http_429_maps_to_rate_limited() {
    let server = MockServer::start().await;
    let body = serde_json::json!({"error": {"status": "RESOURCE_EXHAUSTED", "message": "quota"}});
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "5")
                .set_body_json(body),
        )
        .mount(&server)
        .await;

    let model = GeminiModel::developer("gemini-2.5-flash")
        .api_key("k")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();
    let first = s.next().await.unwrap();
    assert!(matches!(
        first,
        Err(ModelError::RateLimited {
            retry_after_ms: Some(5000)
        })
    ));
}
