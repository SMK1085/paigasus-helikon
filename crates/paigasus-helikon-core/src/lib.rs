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

pub mod context;

pub use context::*;
