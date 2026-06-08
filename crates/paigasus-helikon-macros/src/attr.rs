//! Attribute parsing for `#[tool(...)]`.

use syn::{
    parse::{Parse, ParseStream},
    Error, Ident, LitStr, Path, Token,
};

/// Parsed `effect = read_only | write | side_effect`.
#[derive(Clone, Copy)]
pub(crate) enum ToolEffectArg {
    ReadOnly,
    Write,
    SideEffect,
}

/// Parsed form of `#[tool(description = "...", name = "...", effect = ..., crate = ::path)]`.
#[derive(Default)]
pub(crate) struct ToolAttrArgs {
    pub description: Option<LitStr>,
    pub name: Option<LitStr>,
    pub effect: Option<ToolEffectArg>,
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
            // Handle it explicitly before falling back to regular identifiers.
            if input.peek(Token![crate]) {
                let _: Token![crate] = input.parse()?;
                let _: Token![=] = input.parse()?;
                let path: Path = input.parse()?;
                out.crate_path = Some(path);
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
                    "effect" => {
                        let val: Ident = input.parse()?;
                        out.effect = Some(match val.to_string().as_str() {
                            "read_only" => ToolEffectArg::ReadOnly,
                            "write" => ToolEffectArg::Write,
                            "side_effect" => ToolEffectArg::SideEffect,
                            other => {
                                return Err(Error::new(
                                    val.span(),
                                    format!(
                                        "invalid `effect` value `{other}`; expected \
                                         `read_only`, `write`, or `side_effect`"
                                    ),
                                ));
                            }
                        });
                    }
                    other => {
                        return Err(Error::new(
                            key.span(),
                            format!(
                                "unknown #[tool] attribute `{other}`; expected one of \
                                 `description`, `name`, `effect`, `crate`",
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
