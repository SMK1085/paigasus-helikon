//! Live smoke tests against a real LiteLLM proxy. Ignored by default; run
//! with `-- --ignored`, matching the sibling live suites (e.g.
//! `paigasus-helikon-providers-gemini/tests/live.rs`).
//!
//! Env-gated: set `LITELLM_API_BASE` (and optionally `LITELLM_API_KEY`) to
//! run. Loud-skips otherwise so `cargo test -- --ignored` stays green without
//! a proxy. Set `HELIKON_REQUIRE_LITELLM=1` to turn that skip into a hard
//! failure — SMA-523's CI job for this suite will set it, since a skipped
//! test *passes* and `cargo test` captures a passing test's output, so
//! without the flag a job that never reached a proxy is indistinguishable
//! from a green one (mirrors `HELIKON_REQUIRE_TEMPORAL` in
//! `paigasus-helikon-runtime-temporal/tests/temporal_live.rs`).
//!
//! A keyless rig is enough — LiteLLM `mock_response` deployments serve real
//! streaming SSE with a fake upstream key. See the SMA-451 design Appendix B
//! for the config, and SMA-523 for the CI job that will run this.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_litellm::LiteLlmModel;

/// Returns the configured proxy base URL, or prints a loud skip message and
/// returns `None`.
///
/// Set `HELIKON_REQUIRE_LITELLM=1` to turn that skip into a hard failure. See
/// the module doc for why this is load-bearing, not belt-and-braces — mirrors
/// `gate()` in `paigasus-helikon-runtime-temporal/tests/temporal_live.rs`.
fn gate() -> Option<String> {
    match std::env::var("LITELLM_API_BASE") {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => {
            if std::env::var("HELIKON_REQUIRE_LITELLM").as_deref() == Ok("1") {
                panic!(
                    "HELIKON_REQUIRE_LITELLM=1 but LITELLM_API_BASE is unset or empty — \
                     the live LiteLLM suite would have skipped silently"
                );
            }
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
#[ignore]
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
