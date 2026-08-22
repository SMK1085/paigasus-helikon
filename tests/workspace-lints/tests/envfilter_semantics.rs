//! `EnvFilter` matches a target by raw string prefix, not by `::` segment.
//! SMA-568 D2 and D3 both depend on this; SMA-557 could only verify it by hand.

use std::sync::{Arc, Mutex};

use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::EnvFilter;

const T_CORE_AGENT: &str = "paigasus::core::agent";
const T_CORE_WORKFLOW: &str = "paigasus::core::workflow";
const T_OPENAI_CHAT: &str = "paigasus::openai::chat";
const T_AXUM_REGISTRY: &str = "paigasus::runtime_axum::registry";
const T_TOKIO_RETRY: &str = "paigasus::runtime_tokio::retry";
const T_ACTIX_REGISTRY: &str = "paigasus::runtime_actix::registry";
const T_AGENTCORE_SERVER: &str = "paigasus::runtime_agentcore::server";
const T_TEMPORAL_WORKER: &str = "paigasus::runtime_temporal::worker";
const T_MODULE_PATH: &str = "paigasus_helikon_core::session";
const T_FOREIGN: &str = "hyper::client";

/// Records the target of every event that reaches it.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

impl<S> Layer<S> for Capture
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        self.0
            .lock()
            .expect("capture mutex")
            .push(event.metadata().target().to_owned());
    }
}

/// Emit one DEBUG event per probe target under `$directive`, and evaluate to
/// the targets that survived filtering.
///
/// **This is a `macro_rules!`, not a function, and that is load-bearing.** A
/// `tracing` callsite caches its `Interest` globally, while `with_default`
/// installs a subscriber only for the current thread. A shared helper
/// *function* would give all four tests the same ten callsites, so whichever
/// test ran first would prime the interest cache for the rest — the classic
/// interest-caching flake, and a green-when-wrong one. A macro expands at each
/// invocation, so every test gets its own callsites and the tests stay
/// independent even running in parallel.
macro_rules! reaching {
    ($directive:expr) => {{
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new($directive))
            .with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            tracing::event!(target: T_CORE_AGENT, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_CORE_WORKFLOW, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_OPENAI_CHAT, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_AXUM_REGISTRY, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_TOKIO_RETRY, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_ACTIX_REGISTRY, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_AGENTCORE_SERVER, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_TEMPORAL_WORKER, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_MODULE_PATH, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_FOREIGN, tracing::Level::DEBUG, "p");
        });
        let out = capture.0.lock().expect("capture mutex").clone();
        out
    }};
}

#[test]
fn two_segment_component_selects_only_that_component() {
    let got = reaching!("paigasus::core=debug");
    assert!(got.contains(&T_CORE_AGENT.to_owned()));
    assert!(got.contains(&T_CORE_WORKFLOW.to_owned()));
    assert!(!got.contains(&T_OPENAI_CHAT.to_owned()));
    assert!(!got.contains(&T_AXUM_REGISTRY.to_owned()));
}

#[test]
fn runtime_group_selector_reaches_every_adapter() {
    let got = reaching!("paigasus::runtime=debug");
    assert!(got.contains(&T_AXUM_REGISTRY.to_owned()));
    assert!(got.contains(&T_TOKIO_RETRY.to_owned()));
    assert!(got.contains(&T_ACTIX_REGISTRY.to_owned()));
    assert!(got.contains(&T_AGENTCORE_SERVER.to_owned()));
    assert!(got.contains(&T_TEMPORAL_WORKER.to_owned()));
    assert!(!got.contains(&T_CORE_AGENT.to_owned()));
    assert!(!got.contains(&T_OPENAI_CHAT.to_owned()));
}

#[test]
fn trailing_colons_select_the_namespace_and_exclude_module_paths() {
    let got = reaching!("paigasus::=debug");
    assert!(got.contains(&T_CORE_AGENT.to_owned()));
    assert!(got.contains(&T_OPENAI_CHAT.to_owned()));
    assert!(!got.contains(&T_MODULE_PATH.to_owned()));
    assert!(!got.contains(&T_FOREIGN.to_owned()));
}

// The load-bearing one: a bare `paigasus` is a raw prefix, so it ALSO matches
// `paigasus_helikon_core::session`. Everything in the book's "Filtering by
// target" section follows from this.
#[test]
fn bare_prefix_also_matches_module_paths() {
    let got = reaching!("paigasus=debug");
    assert!(got.contains(&T_CORE_AGENT.to_owned()));
    assert!(
        got.contains(&T_MODULE_PATH.to_owned()),
        "a bare `paigasus` directive must match `paigasus_helikon_*` too"
    );
    assert!(!got.contains(&T_FOREIGN.to_owned()));
}
