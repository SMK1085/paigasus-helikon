//! Proc macros for the Paigasus Helikon SDK.
//!
//! Two macros:
//! - `#[tool]` — attribute macro on `async fn` that synthesizes an
//!   `impl Tool<Ctx>` against `paigasus-helikon-core`.
//! - `tools!` — function-like macro that boxes a heterogeneous list of
//!   tool values into `Vec<Arc<dyn Tool<Ctx>>>`.
//!
//! See the SMA-315 design at
//! `docs/superpowers/specs/2026-05-22-sma-315-tool-proc-macro-design.md`.

mod attr;
mod expand;
mod resolve;
mod signature;

use proc_macro::TokenStream;

/// Attribute macro that generates an `impl Tool<Ctx>` for an `async fn`.
///
/// See the crate-level documentation for the full design.
#[proc_macro_attribute]
pub fn tool(args: TokenStream, item: TokenStream) -> TokenStream {
    match expand::tool(args.into(), item.into()) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Function-like macro that boxes a comma-separated list of tool
/// expressions into `Vec<Arc<dyn Tool<Ctx>>>`.
///
/// Each argument must be a value of a type implementing `Tool<Ctx>`
/// directly. Do not pre-wrap with `Arc`. An optional `crate = ::path;`
/// prefix overrides the auto-resolved support-crate path.
#[proc_macro]
pub fn tools(input: TokenStream) -> TokenStream {
    match expand::tools(input.into()) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
