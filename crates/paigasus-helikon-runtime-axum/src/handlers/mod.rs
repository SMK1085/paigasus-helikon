//! Axum route handlers for the agent server.
//!
//! Each sub-module corresponds to one resource group (e.g. `agents`, `runs`).
//! Handlers are `pub(crate)` and wired into the router by [`crate::server`].

pub(crate) mod agents;
pub(crate) mod events;
#[cfg(feature = "openapi")]
pub(crate) mod openapi;
pub(crate) mod runs;
