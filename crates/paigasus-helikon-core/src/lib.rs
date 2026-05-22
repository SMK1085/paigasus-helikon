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
//! [`paigasus-helikon`]: https://docs.rs/paigasus-helikon
//! [project documentation site]: https://smk1085.github.io/paigasus-helikon/

pub mod agent;
pub mod context;
pub mod guardrail;
pub mod hook;
pub mod item;
pub mod loop_state;
pub mod model;
pub mod runner;
pub mod session;
pub mod tool;

#[doc(hidden)]
pub mod __private;

pub use agent::*;
pub use context::*;
pub use guardrail::*;
pub use hook::*;
pub use item::*;
pub use loop_state::*;
pub use model::*;
pub use runner::*;
pub use session::*;
pub use tool::*;
