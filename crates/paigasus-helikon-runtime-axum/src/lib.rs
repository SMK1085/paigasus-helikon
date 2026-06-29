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

// `registry` types are consumed by route handlers added in subsequent tasks.
// Until those callers land, suppress the dead_code lint on this module.
#[allow(dead_code)]
mod registry;

mod session;
pub use session::{InMemorySessionProvider, SessionProvider};

mod context;
pub use context::{ContextProvider, DefaultContextProvider};

mod auth;
pub use auth::AuthLayer;

mod dto;
pub use dto::{AgentInfo, AsyncAccepted, RunRequest, RunResponse, RunStatus};
