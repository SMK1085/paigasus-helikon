//! SSE -> ModelEvent translation tests via a mock server.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelError, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_gemini::GeminiModel;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn run(sse: &'static str) -> Vec<Result<ModelEvent, ModelError>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(sse, "text/event-stream"),
        )
        .mount(&server)
        .await;
    let model = GeminiModel::developer("gemini-2.5-flash")
        .api_key("k")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut r = ModelRequest::new();
    r.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text { text: "hi".into() }],
    }];
    let mut s = model.invoke(r, CancellationToken::new()).await.unwrap();
    let mut out = Vec::new();
    while let Some(ev) = s.next().await {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn text_then_finish() {
    let evs = run("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"a\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}\n\n").await;
    assert!(matches!(evs.first().unwrap(), Ok(ModelEvent::TokenDelta { text }) if text == "a"));
    assert!(matches!(
        evs.last().unwrap(),
        Ok(ModelEvent::Finish {
            reason: FinishReason::Stop
        })
    ));
}

#[tokio::test]
async fn blocked_prompt_refused() {
    let evs = run("data: {\"promptFeedback\":{\"blockReason\":\"SAFETY\"}}\n\n").await;
    assert!(matches!(
        evs.first().unwrap(),
        Err(ModelError::Refused { .. })
    ));
}

#[tokio::test]
async fn truncated_stream_no_finish() {
    let evs =
        run("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}\n\n")
            .await;
    assert!(evs
        .iter()
        .all(|e| !matches!(e, Ok(ModelEvent::Finish { .. }))));
}

/// Documents the actual per-chunk `Usage` emission behavior.
///
/// The `StreamTranslator` emits one `ModelEvent::Usage` per SSE chunk that
/// carries `usageMetadata`. This test exercises a two-chunk stream where both
/// chunks include usage data, confirming that the stream yields exactly two
/// `Usage` events (one per chunk). Consumers tracking cumulative token counts
/// should use the **last** `Usage` seen within a turn, per the last-wins
/// contract documented in `ModelEvent::Usage`.
#[tokio::test]
async fn multi_chunk_usage_emits_usage_per_chunk() {
    // Two SSE events, both carrying usageMetadata.
    // Chunk 1: text "a" + usage (promptTokens=1, candidateTokens=1).
    // Chunk 2: text "b" + STOP finishReason + usage (promptTokens=1, candidateTokens=2).
    let evs = run(concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"a\"}]}}],",
        "\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}\n\n",
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"b\"}]},\"finishReason\":\"STOP\"}],",
        "\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":2}}\n\n",
    ))
    .await;

    let usage_count = evs
        .iter()
        .filter(|e| matches!(e, Ok(ModelEvent::Usage { .. })))
        .count();

    // StreamTranslator emits one Usage per SSE chunk that includes usageMetadata,
    // so two chunks with usageMetadata produce two Usage events.
    assert_eq!(
        usage_count, 2,
        "expected one Usage event per SSE chunk carrying usageMetadata; got {usage_count}"
    );
}
