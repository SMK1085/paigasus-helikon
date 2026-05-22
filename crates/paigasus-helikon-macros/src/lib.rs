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
/// # Example
///
/// ```
/// use paigasus_helikon_core::{Tool, ToolContext, ToolError};
/// use paigasus_helikon_macros::tool;
/// use schemars::JsonSchema;
/// use serde::{Deserialize, Serialize};
///
/// struct MyCtx;
///
/// #[derive(Deserialize, JsonSchema)]
/// struct AddArgs { a: i64, b: i64 }
///
/// #[derive(Serialize, JsonSchema)]
/// struct AddOut { sum: i64 }
///
/// /// Adds two numbers.
/// #[tool]
/// async fn add(
///     _ctx: &ToolContext<MyCtx>,
///     args: AddArgs,
/// ) -> Result<AddOut, ToolError> {
///     Ok(AddOut { sum: args.a + args.b })
/// }
///
/// assert_eq!(add.name(), "add");
/// assert_eq!(add.description(), "Adds two numbers.");
/// ```
///
/// See the SMA-315 design doc for the full attribute surface and
/// edge-case behavior.
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
/// directly. Do not pre-wrap with `Arc` — `tools![Arc::new(t)]`
/// generates `Arc::new(Arc::new(t)) as Arc<dyn Tool<_>>` and fails
/// the cast.
///
/// An optional `crate = ::path;` prefix overrides the auto-resolved
/// support-crate path; use only for renamed deps or unusual setups.
///
/// **Ctx-mismatch diagnostics:** every tool in a single `tools!`
/// invocation must implement `Tool<Ctx>` for the *same* `Ctx`. The
/// shared `Ctx` is inferred from the first tool or from the LHS type
/// annotation. When rustc reports something like
/// ``the trait `Tool<…>` is not implemented for <tool-name>``,
/// the cause is a `Ctx` mismatch inside `tools!`.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use paigasus_helikon_core::{Tool, ToolContext, ToolError};
/// use paigasus_helikon_macros::{tool, tools};
/// use schemars::JsonSchema;
/// use serde::{Deserialize, Serialize};
///
/// struct MyCtx;
///
/// #[derive(Deserialize, JsonSchema)]
/// struct AddArgs { a: i64, b: i64 }
/// #[derive(Serialize, JsonSchema)]
/// struct AddOut { sum: i64 }
///
/// /// Adds two numbers.
/// #[tool]
/// async fn add(_ctx: &ToolContext<MyCtx>, args: AddArgs)
///     -> Result<AddOut, ToolError>
/// {
///     Ok(AddOut { sum: args.a + args.b })
/// }
///
/// let registry: Vec<Arc<dyn Tool<MyCtx>>> = tools![add];
/// assert_eq!(registry.len(), 1);
/// ```
#[proc_macro]
pub fn tools(input: TokenStream) -> TokenStream {
    match expand::tools(input.into()) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
