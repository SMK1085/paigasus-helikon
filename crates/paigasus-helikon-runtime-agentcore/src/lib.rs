//! AWS Bedrock AgentCore runtime for Paigasus Helikon agents.
//!
//! [`AgentCoreServer`] mounts a single [`Agent`](paigasus_helikon_core::Agent) on an
//! [`axum`] router that implements AWS Bedrock AgentCore's HTTP-protocol container
//! contract:
//!
//! - **`GET /ping`** — a dedicated, always-responsive health-check handler that never
//!   shares state with the runner, the agent, or any in-flight invocation, so a slow or
//!   stuck invocation can never delay a health check. Returns HTTP 200 with a JSON body
//!   of exactly `{"status":"Healthy"}` or `{"status":"HealthyBusy"}` — this casing is
//!   part of the contract and is not configurable. An optional `time_of_last_update`
//!   field (Unix seconds) is included *only* once a genuine status transition has
//!   occurred: never on the initial steady state, and never re-stamped by a repeated
//!   call reporting the same status, since AgentCore uses an advancing timestamp to
//!   judge whether the agent is still making progress. See [`PingState`].
//! - **`POST /invocations`** — the endpoint AgentCore calls to run the agent. This
//!   revision of the crate mounts a placeholder that always returns HTTP 501; the full
//!   JSON/SSE request-response contract lands in a follow-up revision.
//!
//! [`AgentCoreServer::serve`] binds `0.0.0.0:8080` — the fixed port AgentCore's runtime
//! contract expects — and logs the app-side cold-start latency (`"ready in {ms}ms"`)
//! immediately after the listener is bound; AgentCore's own microVM provisioning
//! latency is outside this crate's control and is not part of that measurement.
//!
//! Session and per-request context handling reuse `paigasus-helikon-runtime-axum`'s
//! provider traits (`SessionProvider`/`ContextProvider`), so a self-hosted deployment
//! and an AgentCore deployment of the same agent share one provider vocabulary.
#![forbid(unsafe_code)]

mod error;
pub use error::AgentCoreError;

mod ping;
pub use ping::PingState;

mod server;
pub use server::{AgentCoreServer, AgentCoreServerBuilder};
