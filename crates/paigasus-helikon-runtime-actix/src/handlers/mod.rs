//! HTTP handlers mounted by [`crate::server::AgentServer::configure`].

pub(crate) mod agents;
pub(crate) mod events;
#[cfg(feature = "openapi")]
pub(crate) mod openapi;
pub(crate) mod runs;
