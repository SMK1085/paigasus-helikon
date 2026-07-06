//! Temporal-backed durable runtime for the Paigasus Helikon AI SDK.
//!
//! Placeholder skeleton (SMA-332 Task 4): wires the workspace to the
//! official `temporalio-*` Rust SDK (Public Preview) so later tasks can add
//! the durable driver, activities, worker, and [`paigasus_helikon_core::Runner`]
//! implementation without a fresh dependency/skeleton PR of their own. Full
//! crate docs land once the implementation is complete.

/// Thin Temporal activity layer over the SDK-free driver-facing inner
/// functions, plus the process-local per-agent registry a durable worker
/// resolves by name (never serialized — see [`driver::AgentPlan`]'s docs on
/// why). Private: every externally-relevant type it defines is re-exported
/// or consumed through [`worker`].
mod activities;
/// The pure durable-loop step machine.
///
/// [`driver::DurableDriver`] wraps [`paigasus_helikon_core::transition`] with
/// the bookkeeping (conversation, accumulated events, cumulative usage) a
/// Temporal workflow needs to drive an agent run one activity result at a
/// time, without any Temporal SDK dependency of its own.
pub mod driver;
/// Error types for the Temporal-backed durable runtime.
pub mod error;
/// Wire-format payload types exchanged between the Temporal workflow and its
/// activities.
pub mod payloads;
/// The client-side [`paigasus_helikon_core::Runner`] implementation:
/// [`runner::TemporalRunner`] starts the durable workflow, awaits its outcome
/// (with cooperative cancellation), and mirrors `TokioRunner`'s session
/// semantics at the run boundary.
pub mod runner;
/// Temporal worker construction: builds a [`worker::TemporalAgentWorker`]
/// that serves one or more registered [`paigasus_helikon_core::LlmAgent`]s'
/// activities on a task queue.
pub mod worker;
/// The durable agent-loop workflow driven by a
/// [`crate::worker::TemporalAgentWorker`]. Internal: the public entry points
/// are [`worker::TemporalAgentWorker`] (worker side) and
/// [`runner::TemporalRunner`] (client side).
mod workflow;
