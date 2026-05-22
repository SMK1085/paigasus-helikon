//! Attribute parsing for `#[tool(...)]`.

use proc_macro2::Span;
use syn::{
    parse::{Parse, ParseStream},
    Error, Ident, LitStr, Path, Token,
};

/// Parsed form of `#[tool(description = "...", name = "...", crate = ::path)]`.
#[derive(Default)]
#[allow(dead_code)]
pub(crate) struct ToolAttrArgs {
    pub description: Option<LitStr>,
    pub name: Option<LitStr>,
    pub crate_path: Option<Path>,
    pub span: Option<Span>,
}

impl Parse for ToolAttrArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let span = Some(input.span());
        let mut out = ToolAttrArgs {
            span,
            ..Default::default()
        };
        if input.is_empty() {
            return Ok(out);
        }

        loop {
            let key: Ident = input.parse()?;
            let _: Token![=] = input.parse()?;

            match key.to_string().as_str() {
                "description" => {
                    let lit: LitStr = input.parse()?;
                    out.description = Some(lit);
                }
                "name" => {
                    let lit: LitStr = input.parse()?;
                    out.name = Some(lit);
                }
                "crate" => {
                    let path: Path = input.parse()?;
                    out.crate_path = Some(path);
                }
                other => {
                    return Err(Error::new(
                        key.span(),
                        format!(
                            "unknown #[tool] attribute `{other}`; expected one of \
                             `description`, `name`, `crate`",
                        ),
                    ));
                }
            }

            if input.is_empty() {
                break;
            }
            let _: Token![,] = input.parse()?;
            if input.is_empty() {
                break;
            }
        }

        Ok(out)
    }
}
