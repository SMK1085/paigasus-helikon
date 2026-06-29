//! Self-hosted HTTP server runtime for Paigasus Helikon agents.
//!
//! Mounts one or more [`Agent`](paigasus_helikon_core::Agent)s on an [`axum`] router and serves
//! them over REST (one-shot), Server-Sent Events, and WebSocket, with replayable runs.
//!
//! See the crate `README.md` for a runnable example.
#![forbid(unsafe_code)]

// Modules are added by subsequent tasks.
