//! AG-UI `GET /ws` — bidirectional AG-UI event exchange.
//!
//! Stub for SMA-461 Task 6, so [`crate::agui`]'s router compiles and mounts a `/ws`
//! route ahead of the real handler. Task 7 (SMA-461) owns the actual implementation —
//! a WebSocket upgrade carrying the same [`crate::agui::types::RunAgentInput`] request
//! vocabulary as `POST /invocations`, paced through [`crate::frame::FrameBudget`].

use axum::http::StatusCode;

/// Placeholder `GET /ws` handler: always `501 Not Implemented`. Task 7 replaces this.
///
/// Takes no extractors — including no `State<AppState<Ctx>>` — so it satisfies axum's
/// `Handler` bound for a router of any `Ctx`, with no generic parameter of its own
/// needed at this call site. Task 7's real implementation will need one.
pub(crate) async fn ws_upgrade() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
