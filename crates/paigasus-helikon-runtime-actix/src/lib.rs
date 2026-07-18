//! Self-hosted actix-web server runtime for Paigasus Helikon agents.
//!
//! Mounts one or more [`Agent`](paigasus_helikon_core::Agent)s on an [`actix_web`] app and
//! serves them over REST (one-shot), Server-Sent Events, and WebSocket, with replayable runs.
//! Public-surface-compatible with `paigasus-helikon-runtime-axum`.
//!
//! See the crate `README.md` for a runnable example.
#![forbid(unsafe_code)]

mod event_log;
mod registry;

mod dto;
pub use dto::{AgentInfo, AsyncAccepted, RunRequest, RunResponse, RunStatus};

mod error;
pub use error::{AuthRejection, ServerError};
mod session;
pub use session::{InMemorySessionProvider, SessionProvider};
