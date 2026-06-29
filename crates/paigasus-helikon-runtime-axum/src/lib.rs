//! Self-hosted HTTP server runtime for Paigasus Helikon agents.
//!
//! Mounts one or more [`Agent`](paigasus_helikon_core::Agent)s on an [`axum`] router and serves
//! them over REST (one-shot), Server-Sent Events, and WebSocket, with replayable runs.
//!
//! See the crate `README.md` for a runnable example.
#![forbid(unsafe_code)]

mod error;
pub use error::{AuthRejection, ServerError};

// `event_log` types are consumed by transport modules added in subsequent tasks (SSE, WebSocket,
// one-shot handler). Until those callers land, suppress the dead_code lint on this module.
#[allow(dead_code)]
mod event_log;
