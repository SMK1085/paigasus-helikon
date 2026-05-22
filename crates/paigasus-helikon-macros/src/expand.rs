//! Codegen for `#[tool]` and `tools!`.

use proc_macro2::TokenStream;
use syn::Error;

/// `#[tool]` codegen entry point. Populated by Phase C.
pub(crate) fn tool(_args: TokenStream, item: TokenStream) -> Result<TokenStream, Error> {
    Err(Error::new_spanned(
        item,
        "#[tool] not implemented yet — placeholder from SMA-315 Phase A",
    ))
}

/// `tools!` codegen entry point. Populated by Phase C.
pub(crate) fn tools(input: TokenStream) -> Result<TokenStream, Error> {
    Err(Error::new_spanned(
        input,
        "tools! not implemented yet — placeholder from SMA-315 Phase A",
    ))
}
