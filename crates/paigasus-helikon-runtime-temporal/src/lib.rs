//! Temporal-backed durable runtime for the Paigasus Helikon AI SDK.
//!
//! Placeholder skeleton (SMA-332 Task 4): wires the workspace to the
//! official `temporalio-*` Rust SDK (Public Preview) so later tasks can add
//! the durable driver, activities, worker, and [`paigasus_helikon_core::Runner`]
//! implementation without a fresh dependency/skeleton PR of their own. Full
//! crate docs land once the implementation is complete.

/// Error types for the Temporal-backed durable runtime.
pub mod error;
/// Wire-format payload types exchanged between the Temporal workflow and its
/// activities.
pub mod payloads;
