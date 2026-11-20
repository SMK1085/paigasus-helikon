//! Trait surface and core types for the Paigasus Helikon AI SDK.
//!
//! This crate is the dependency root of the workspace; the facade crate
//! [`paigasus-helikon`] re-exports its surface unconditionally.
//!
//! The seven object-safe traits ([`Model`], [`Tool`], [`Session`],
//! [`Guardrail`], [`Hook`], [`Agent`], [`Runner`]) and their carrier
//! types form the contract every other Paigasus Helikon crate depends on.
//!
//! See the [project documentation site] for conceptual material; this
//! crate's rustdoc is the canonical reference for the trait signatures and
//! carrier types.
//!
//! # Deferred tool calls
//!
//! A tool call can wait for a host decision in a later run. A permission policy
//! returns `Defer`, or an [`ApprovalHandler`] returns `ApprovalOutcome::Defer`
//! at an ask point. The run then ends with `AgentEvent::RunDeferred` and
//! [`StopReason::Deferred`]. The session records one `tool_deferred` event for each
//! call. The host resolves every open call with [`AgentInput::with_resolution`]
//! and runs the agent again with a runner that supports resume.
//!
//! Only a runner that records the session saves a deferral, for example
//! `TokioRunner` in `paigasus-helikon-runtime-tokio`. A direct `LlmAgent::run`
//! keeps the raw call only in a run-local slot, and the session gets no
//! `tool_deferred` event. A resume against that session then fails with
//! `NoOpenDeferrals`. Thus the host must start the first run and the resume
//! through the same session-recording runner, against the same session.
//!
//! Deferral is opt-in. Use [`RunContext::with_deferred_tool_calls`]. The default is
//! off. With the flag off, a `Defer` is a deny.
//!
//! The session stores the raw effective arguments of a deferred call. A secret in
//! these arguments persists in the session store. The host must keep one writer for
//! each session.
//!
//! **Rollback note.** Once a host stores a `tool_deferred` event, it cannot roll back
//! below this version, because an older reader cannot parse the row. Enable deferral
//! only after every reader of the session store runs this version.
//!
//! [`paigasus-helikon`]: https://docs.rs/paigasus-helikon
//! [project documentation site]: https://smk1085.github.io/paigasus-helikon/

pub mod agent;
pub mod agent_as_tool;
pub mod agent_builder;
pub mod command_match;
mod compacting_session;
pub mod context;
pub mod control;
pub mod deferral;
pub mod graph;
pub mod guardrail;
pub mod handoff;
pub mod hook;
pub mod item;
pub mod loop_state;
pub mod model;
mod path_match;
pub mod permission;
pub mod redaction;
mod resume_exec;
pub mod runner;
pub mod schema;
pub mod session;
pub mod state;
pub mod swarm;
pub mod token_counter;
pub mod tool;
pub mod tool_exec;
pub mod workflow;

#[doc(hidden)]
pub mod __private;

pub use agent::*;
pub use agent_as_tool::*;
pub use agent_builder::*;
pub use compacting_session::{CompactingSession, CompactingSessionBuilder, CompactingSessionError};
pub use context::*;
pub use deferral::*;
pub use graph::{GraphAgent, GraphAgentBuilder, GraphBuildError};
pub use guardrail::*;
pub use handoff::*;
pub use hook::*;
pub use item::*;
pub use loop_state::*;
pub use model::*;
pub use permission::*;
pub use runner::*;
pub use session::*;
pub use state::*;
pub use swarm::*;
pub use token_counter::*;
pub use tool::*;
pub use tool_exec::*;
pub use workflow::*;
