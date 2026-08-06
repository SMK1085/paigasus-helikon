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
//! - **`POST /invocations`** — the endpoint AgentCore calls to run the agent. Accepts
//!   [`InvocationRequest`]'s three body shapes (`{"messages": [...]}`, `{"prompt": "..."}`,
//!   `{"input": "..."}`). An optional
//!   `X-Amzn-Bedrock-AgentCore-Runtime-Session-Id` request header (33-256 characters)
//!   pins the invocation to a session via the configured
//!   [`SessionProvider`](paigasus_helikon_runtime_axum::SessionProvider); an absent
//!   header gets a fresh, unshared session (one microVM instance is, by AgentCore's
//!   execution model, already one session). `Accept: application/json` selects a
//!   buffered `200` response shaped `{"final_output": "...", "usage": {...}}`; any
//!   other (or absent) `Accept` selects the default Server-Sent-Events transport,
//!   emitting one `data: <AgentEvent JSON>` frame per event with a terminal
//!   `RunCompleted`/`RunFailed` frame.
//!
//! [`AgentCoreServer::serve`] binds `0.0.0.0:8080` — the fixed port AgentCore's runtime
//! contract expects — and logs the app-side cold-start latency (`"ready in {ms}ms"`)
//! immediately after the listener is bound; AgentCore's own microVM provisioning
//! latency is outside this crate's control and is not part of that measurement.
//!
//! Session and per-request context handling reuse `paigasus-helikon-runtime-axum`'s
//! provider traits (`SessionProvider`/`ContextProvider`), so a self-hosted deployment
//! and an AgentCore deployment of the same agent share one provider vocabulary.
//!
//! # Session keys carry no principal in this runtime
//!
//! The [`SessionKey`](paigasus_helikon_runtime_axum::SessionKey) handed to the
//! configured provider always has `principal: None`. This runtime exposes no
//! `AuthLayer` seam, and AgentCore's execution model already isolates each session in
//! its own microVM instance, so the validated session id is the whole identity here.
//!
//! A custom [`SessionProvider`](paigasus_helikon_runtime_axum::SessionProvider)
//! supplied through [`AgentCoreServerBuilder::session_provider`] must therefore not
//! expect principal-based separation on this runtime — including via
//! [`SessionKey::storage_key`](paigasus_helikon_runtime_axum::SessionKey::storage_key),
//! which reduces to a stable per-id key when the principal is absent. That is the
//! intended behaviour here, not an oversight; the separation the axum and actix
//! runtimes get from a `Principal` is provided by the microVM boundary instead.
//!
//! # MCP-protocol mode (feature `mcp`, default on)
//!
//! `AgentCoreServer::serve_mcp` serves the same configured agent as a single MCP
//! tool over rmcp's streamable-HTTP transport instead of the HTTP-protocol contract
//! above — for AgentCore's MCP runtime type rather than its default HTTP runtime
//! type. It binds a separate port (`0.0.0.0:8000`) and mounts the MCP endpoint at
//! `/mcp` plus a trivial `/ping` (not part of MCP; cheap insurance). See
//! `AgentCoreServer::mcp_router` for the stateless/allowed-hosts configuration this
//! requires and why.
#![forbid(unsafe_code)]

mod error;
pub use error::AgentCoreError;

mod invoke;
pub use invoke::InvocationRequest;

#[cfg(feature = "mcp")]
mod mcp;

mod ping;
pub use ping::PingState;

mod session;

mod server;
pub use server::{AgentCoreServer, AgentCoreServerBuilder};
