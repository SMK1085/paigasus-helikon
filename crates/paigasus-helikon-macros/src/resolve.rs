//! Resolves the path stem for `paigasus-helikon-core` symbols
//! referenced by generated code.

use proc_macro2::{Span, TokenStream};
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{format_ident, quote};
use syn::{Error, Path};

/// Resolve the prefix to use for `<stem>::Tool`, `<stem>::ToolContext`,
/// `<stem>::__private::OutputSchemaProbe`, and friends.
///
/// Resolution order:
/// 1. If `override_path` is `Some`, return it verbatim.
/// 2. Look up `paigasus-helikon-core` in the consumer's Cargo.toml:
///    - `FoundCrate::Itself` → `::paigasus_helikon_core`
///    - `FoundCrate::Name(n)` → `::<n>` (covers renamed deps)
/// 3. Fall back to `paigasus-helikon` (facade):
///    - `FoundCrate::Itself` → `::paigasus_helikon::core`
///    - `FoundCrate::Name(n)` → `::<n>::core`
/// 4. Neither found → compile error.
pub(crate) fn resolve_core_path(
    override_path: Option<&Path>,
    error_span: Span,
) -> Result<TokenStream, Error> {
    if let Some(p) = override_path {
        return Ok(quote!(#p));
    }

    match crate_name("paigasus-helikon-core") {
        Ok(FoundCrate::Itself) => Ok(quote!(::paigasus_helikon_core)),
        Ok(FoundCrate::Name(name)) => {
            let id = format_ident!("{}", name);
            Ok(quote!(::#id))
        }
        Err(_) => match crate_name("paigasus-helikon") {
            Ok(FoundCrate::Itself) => Ok(quote!(::paigasus_helikon::core)),
            Ok(FoundCrate::Name(name)) => {
                let id = format_ident!("{}", name);
                Ok(quote!(::#id::core))
            }
            Err(_) => Err(Error::new(
                error_span,
                "#[tool] / tools! requires either `paigasus-helikon-core` or \
                 `paigasus-helikon` (features=[\"macros\"]) as a direct \
                 dependency; or set `#[tool(crate = ::path)]`",
            )),
        },
    }
}
