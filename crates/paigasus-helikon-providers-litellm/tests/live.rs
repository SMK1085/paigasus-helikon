//! Live tests against a real LiteLLM proxy.
//!
//! Env-gated: set `LITELLM_API_BASE` (and optionally `LITELLM_API_KEY`) to
//! run. Loud-skips otherwise so `cargo test` stays green without a proxy.
//!
//! A keyless rig is enough — LiteLLM `mock_response` deployments serve real
//! streaming SSE with a fake upstream key. See the SMA-451 design Appendix B
//! for the config, and SMA-523 for the CI job that will run this.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_litellm::LiteLlmModel;

fn gate() -> Option<String> {
    match std::env::var("LITELLM_API_BASE") {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => {
            eprintln!(
                "SKIP: LITELLM_API_BASE not set — skipping live LiteLLM test. \
                 See docs/superpowers/specs/2026-08-16-sma-451-litellm-provider-design.md Appendix B."
            );
            None
        }
    }
}

fn model_id() -> String {
    std::env::var("LITELLM_TEST_MODEL").unwrap_or_else(|_| "mock-fast".to_owned())
}

#[tokio::test]
async fn live_streaming_turn_ends_with_finish_after_usage() {
    let Some(base) = gate() else { return };

    let model = LiteLlmModel::chat(model_id())
        .base_url(base)
        .build()
        .expect("build against live proxy");

    let mut req = ModelRequest::new();
    req.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text {
            text: "say hi".into(),
        }],
    }];

    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let mut evs = Vec::new();
    while let Some(ev) = s.next().await {
        evs.push(ev.expect("live stream must not error"));
    }

    assert!(
        matches!(evs.last(), Some(ModelEvent::Finish { .. })),
        "Finish must be the terminal event against a real proxy"
    );
    assert!(
        evs.iter().any(|e| matches!(e, ModelEvent::Usage { .. })),
        "include_usage should produce a Usage event"
    );
}
