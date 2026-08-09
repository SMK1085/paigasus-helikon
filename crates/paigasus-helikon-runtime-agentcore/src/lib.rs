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
//!
//! # WebSocket on the HTTP protocol (feature `ws`, default on)
//!
//! `GET /ws` is an optional endpoint of AgentCore's HTTP-protocol contract, carrying the
//! same request vocabulary as `POST /invocations` over a persistent connection: each
//! inbound **text** frame is one `InvocationRequest`, and every `AgentEvent` of the
//! resulting run goes back as one JSON text frame. Binary frames are unsupported and
//! close the connection with code `1003`.
//!
//! One run at a time per connection. A request arriving mid-run cancels the in-flight
//! run and *waits for it to finish* before starting its successor — the run's session
//! write happens inside that task, so starting the next run first would let it load
//! history without the interrupted turn.
//!
//! # A2A-protocol mode (feature `a2a`, default on)
//!
//! `AgentCoreServer::serve_a2a` binds `0.0.0.0:9000` and speaks JSON-RPC 2.0 at the root
//! path, with an agent card at `/.well-known/agent-card.json` for discovery. Methods:
//! `message/send`, `message/stream`, `tasks/get`, `tasks/cancel`, and
//! `tasks/resubscribe`; the push-notification-config family answers `-32003` and the
//! authenticated-extended-card method `-32004`.
//!
//! **Error codes are A2A-*specification* codes, never AWS's platform table.** AWS
//! documents a `-32051`…`-32055` range for conditions its platform reports to a client
//! (throttling, runtime unavailable, and so on) in front of the container. Those are
//! never emitted from inside it; a container that returned one would be claiming a
//! platform condition that did not happen. What this crate emits is the specification's
//! own taxonomy (`-32001` TaskNotFound, `-32002` TaskNotCancelable, and the JSON-RPC
//! core codes), and, per the specification, every such error rides an HTTP `200`.
//!
//! **Tasks are lost when the container stops.** The default [`InMemoryTaskStore`] is
//! bounded and in-process, but `tasks/get` and `tasks/resubscribe` exist so a client can
//! come back to a task after a disconnect — which only means something across container
//! lifetimes if tasks outlive one, and AgentCore terminates containers abruptly.
//! [`TaskStore`] is the seam: implement it over a database and install it with
//! [`AgentCoreServerBuilder::task_store`] for any deployment whose clients rely on
//! resubscription. Relatedly, a task present in a durable store but with no live run in
//! *this* container cannot be cancelled from here, and `tasks/cancel` answers `-32002`
//! rather than pretending otherwise.
//!
//! A client disconnect does **not** cancel an A2A task — the opposite of
//! `/invocations`' behaviour, and deliberately so: resubscription exists precisely to
//! survive a dropped stream. Only `tasks/cancel` produces `canceled`.
//!
//! # AG-UI-protocol mode (feature `ag-ui`, default on)
//!
//! `AgentCoreServer::serve_agui` binds `0.0.0.0:8080` and serves AG-UI's event
//! vocabulary over SSE at `POST /invocations` plus a WebSocket at `GET /ws`. AG-UI and
//! the HTTP protocol are alternative `serverProtocol` settings for one container and
//! share both the port and the path, so a deployment runs one or the other.
//!
//! **AG-UI mode is stateless per request and cannot use a persistent session backend in
//! v0.** AG-UI clients resend the entire conversation in `messages` on every request,
//! while the runner seeds the model with `history ++ input.messages`; pairing a
//! persisted session with a full client history would double-count every prior turn. So
//! each request gets a fresh, unshared session and `messages` is treated as the whole
//! conversation. The session header is still validated, and used only as a fallback
//! source for the AG-UI `threadId`.
//!
//! **Concurrent agents map imperfectly.** AG-UI's text and tool-call events assume one
//! active span at a time, so an agent interleaving two tool calls has its spans
//! serialized onto the wire rather than genuinely nested.
//!
//! # WebSocket frame quotas
//!
//! Both WebSocket endpoints pace and split outbound frames to stay inside AgentCore's
//! documented limits (64 KB per frame, 250 frames/second), budgeting against the
//! conservative reading of each. Splitting keeps a frame a valid protocol event where it
//! can — a long text delta becomes several smaller deltas — and falls back to
//! `helikon.chunk` envelopes only for events whose payload cannot be split into several
//! valid events (currently AG-UI's `TOOL_CALL_RESULT` and the HTTP protocol's
//! `AgentEvent` frames). A client that never sends oversize payloads never sees an
//! envelope; one that might must reassemble `helikon.chunk` frames in `seq` order until
//! `final` is `true`.
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

#[cfg(feature = "a2a")]
mod a2a;
/// The A2A task-persistence seam and its bounded in-memory default. Implement
/// [`TaskStore`] over a database to let tasks survive AgentCore's abrupt container
/// termination.
#[cfg(feature = "a2a")]
pub use a2a::store::{InMemoryTaskStore, TaskStore, MAX_EVENTS_PER_TASK};
/// A2A wire types: the task lifecycle, its artifacts, and the agent card served for
/// discovery. Public because they appear in the
/// [`TaskStore`](crate::TaskStore) trait's signature and in
/// [`AgentCoreServerBuilder::agent_card`].
#[cfg(feature = "a2a")]
pub use a2a::types::{
    AgentCapabilities, AgentCard, AgentSkill, Artifact, Part, Task, TaskEvent, TaskKind, TaskState,
    TaskStatus,
};

#[cfg(feature = "ag-ui")]
mod agui;

#[cfg(any(feature = "ws", feature = "ag-ui"))]
mod frame;

#[cfg(feature = "ws")]
mod ws;
