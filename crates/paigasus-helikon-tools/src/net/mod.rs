//! Shared networking policy + egress enforcement for the `web` and `microvm`
//! tools: the host allow/deny + SSRF IP classifier (promoted from SMA-412) and
//! the CONNECT egress proxy (SMA-437).

pub mod policy;
#[cfg(feature = "microvm")]
pub mod proxy;

pub use policy::{ip_blocked, GuardedResolver};
