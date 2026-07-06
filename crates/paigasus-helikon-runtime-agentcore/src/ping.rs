//! `GET /ping` — the AgentCore health-check endpoint.
//!
//! [`PingState`] is the single source of truth for the server's health-check status. The
//! handler in this module is mounted on a *substate* extracted from the server's shared
//! `Arc<PingState>` (via [`axum::extract::FromRef`]), so it never touches the runner, the
//! agent, the session provider, or any in-flight invocation — a stuck or slow invocation
//! can never delay or starve a health check.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::Json;
use serde::Serialize;

/// The two health-check status values the AgentCore contract recognises.
///
/// Serializes to the bare string `"Healthy"` / `"HealthyBusy"` — serde's default
/// externally-tagged representation of a unit-only enum variant is just the variant
/// name as a JSON string, which happens to match the contract's exact required casing
/// with no `#[serde(rename)]` needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
enum PingStatusKind {
    Healthy,
    HealthyBusy,
}

/// Response body for `GET /ping`.
///
/// `time_of_last_update` is omitted from the JSON body entirely (not emitted as `null`)
/// until the first genuine status transition, per AWS's AgentCore contract guidance.
#[derive(Debug, Serialize)]
pub(crate) struct PingResponse {
    status: PingStatusKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_of_last_update: Option<u64>,
}

/// Shared health-check state backing the AgentCore `/ping` contract.
///
/// Starts `Healthy` with no `time_of_last_update`. [`set_busy`](PingState::set_busy)
/// flips the status and stamps `time_of_last_update` (Unix seconds) *only* on a genuine
/// transition; repeating the current value is a no-op with respect to the stamp.
/// AgentCore uses an advancing `time_of_last_update` to judge whether the agent is still
/// making progress, so re-stamping it on every call — even without a real transition —
/// would defeat its idle-timeout logic.
///
/// v0 of this crate never flips this itself (no background work happens between
/// `/invocations` calls), but the state and a public setter ship so that an [`Agent`]'s
/// tools can flag long-running asynchronous work via
/// [`AgentCoreServer::ping_state`](crate::AgentCoreServer::ping_state).
///
/// [`Agent`]: paigasus_helikon_core::Agent
#[derive(Debug, Default)]
pub struct PingState {
    busy: AtomicBool,
    last_update: Mutex<Option<u64>>,
}

impl PingState {
    /// Set the busy flag.
    ///
    /// Only a *change* from the current value stamps `time_of_last_update` with the
    /// current Unix time, in seconds; calling this again with the value already in
    /// effect is a no-op with respect to the stamp (the status itself is unaffected
    /// either way, since it was already at `busy`).
    pub fn set_busy(&self, busy: bool) {
        let previous = self.busy.swap(busy, Ordering::AcqRel);
        if previous != busy {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            *self
                .last_update
                .lock()
                .expect("PingState::last_update mutex poisoned") = Some(now);
        }
    }

    /// Snapshot the current status into the wire response body.
    fn snapshot(&self) -> PingResponse {
        let status = if self.busy.load(Ordering::Acquire) {
            PingStatusKind::HealthyBusy
        } else {
            PingStatusKind::Healthy
        };
        let time_of_last_update = *self
            .last_update
            .lock()
            .expect("PingState::last_update mutex poisoned");
        PingResponse {
            status,
            time_of_last_update,
        }
    }
}

/// `GET /ping` handler.
///
/// Resolves synchronously from the shared [`PingState`] substate and nothing else.
pub(crate) async fn ping(State(state): State<Arc<PingState>>) -> Json<PingResponse> {
    Json(state.snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use tower::ServiceExt as _;

    fn ping_router(state: Arc<PingState>) -> Router {
        Router::new().route("/ping", get(ping)).with_state(state)
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn initial_ping_is_healthy_with_no_stamp() {
        let state = Arc::new(PingState::default());
        let resp = ping_router(state)
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json, serde_json::json!({"status": "Healthy"}));
        assert!(
            json.get("time_of_last_update").is_none(),
            "time_of_last_update must be absent before any transition, got {json:?}"
        );
    }

    #[tokio::test]
    async fn set_busy_true_transitions_to_healthy_busy_with_stamp() {
        let state = Arc::new(PingState::default());
        state.set_busy(true);
        let resp = ping_router(state)
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let json = body_json(resp).await;
        assert_eq!(json["status"], "HealthyBusy");
        assert!(json["time_of_last_update"].as_u64().is_some());
    }

    #[tokio::test]
    async fn set_busy_true_twice_does_not_advance_the_stamp() {
        let state = PingState::default();
        state.set_busy(true);
        let first = state.snapshot().time_of_last_update;

        // Cross a real second boundary so a buggy implementation that re-stamps on
        // every call (rather than only on a genuine transition) would observably
        // change the value.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        state.set_busy(true); // same value: must be a no-op w.r.t. the stamp
        let second = state.snapshot().time_of_last_update;
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn set_busy_false_transitions_back_with_its_own_stamp() {
        let state = PingState::default();
        state.set_busy(true);
        assert!(state.snapshot().time_of_last_update.is_some());

        state.set_busy(false);
        let snapshot = state.snapshot();
        assert_eq!(snapshot.status, PingStatusKind::Healthy);
        assert!(snapshot.time_of_last_update.is_some());
    }
}
