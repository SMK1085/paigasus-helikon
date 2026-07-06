//! Temporal-backed durable runtime for the Paigasus Helikon AI SDK.
//!
//! Placeholder skeleton (SMA-332 Task 4): wires the workspace to the
//! official `temporalio-*` Rust SDK (Public Preview) so later tasks can add
//! the durable driver, activities, worker, and [`paigasus_helikon_core::Runner`]
//! implementation without a fresh dependency/skeleton PR of their own. Full
//! crate docs land once the implementation is complete.

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
