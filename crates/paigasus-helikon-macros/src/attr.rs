//! Attribute parsing for `#[tool(...)]`.

use syn::{
    parse::{Parse, ParseStream},
    Error, Ident, LitStr, Path, Token,
};

/// Parsed form of `#[tool(description = "...", name = "...", crate = ::path)]`.
#[derive(Default)]
pub(crate) struct ToolAttrArgs {
    pub description: Option<LitStr>,
    pub name: Option<LitStr>,
    pub crate_path: Option<Path>,
}

impl Parse for ToolAttrArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut out = ToolAttrArgs::default();
        if input.is_empty() {
            return Ok(out);
        }

        loop {
            // `crate` is a keyword; syn's `Ident` parser rejects keywords by default.
            // We must handle it explicitly before falling back to regular identifiers.
            if input.peek(Token![crate]) {
                let kw: Token![crate] = input.parse()?;
                let _: Token![=] = input.parse()?;
                let path: Path = input.parse()?;
                out.crate_path = Some(path);
                let _ = kw; // span consumed above; suppress unused-variable lint
            } else {
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
