//! Codegen for `#[tool]` and `tools!`.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{parse2, Error, ItemFn, LitStr};

use crate::attr::ToolAttrArgs;
use crate::resolve::resolve_core_path;
use crate::signature::{description_from_docs, partition_attrs, ToolSignature};

pub(crate) fn tool(args: TokenStream, item: TokenStream) -> Result<TokenStream, Error> {
    let attr_args: ToolAttrArgs = if args.is_empty() {
        ToolAttrArgs::default()
    } else {
        parse2(args)?
    };
    let item_fn: ItemFn = parse2(item)?;

    let sig = ToolSignature::from_item(&item_fn)?;
    let partitioned = partition_attrs(&item_fn.attrs);

    let description = resolve_description(&attr_args, &partitioned.doc_attrs, &item_fn)?;
    let tool_name = resolve_tool_name(&attr_args, &item_fn)?;
    let core = resolve_core_path(attr_args.crate_path.as_ref(), item_fn.sig.fn_token.span)?;

    let vis = &item_fn.vis;
    let fn_ident = &item_fn.sig.ident;

    let ctx_ty = &sig.ctx_ty;
    let args_ty = &sig.args_ty;
    let out_ty = &sig.out_ty;

    let forward_attrs = &partitioned.forward_attrs;

    // Helper fn signature is forwarded *verbatim* from the user's syn::Signature
    // (no path normalization). The helper fn and the `impl Tool` block both live
    // inside `const _: () = { … }`, which gives them direct access to the caller's
    // ambient `use` imports and type definitions without relying on `use super::*`.
    // This approach works in all Rust compilation contexts, including doctests, where
    // child `mod` blocks cannot reach the anonymous doctest wrapper module's items
    // via a glob import.
    let helper_sig = &item_fn.sig;
    let helper_body = &item_fn.block;

    let expanded = quote! {
        #[allow(non_camel_case_types)]
        #vis struct #fn_ident;

        // `const _` gives the helper fn and impl block access to all ambient names
        // (use-imports, struct defs, etc.) in the caller's scope. It also avoids
        // polluting the caller's namespace with `__helikon_*` identifiers.
        const _: () = {
            static __HELIKON_INPUT_SCHEMA:
                ::std::sync::OnceLock<::serde_json::Value> =
                ::std::sync::OnceLock::new();
            static __HELIKON_OUTPUT_SCHEMA:
                ::std::sync::OnceLock<::std::option::Option<::serde_json::Value>> =
                ::std::sync::OnceLock::new();

            // Bring the specialization trait into scope so that the autoref
            // trick resolves to the trait method (Some) when Out: JsonSchema,
            // rather than falling through to the inherent fallback (None).
            use #core::__private::OutputSchemaProbeSpec as _;

            #(#forward_attrs)*
            #helper_sig #helper_body

            #[#core::__private::async_trait::async_trait]
            impl #core::Tool<#ctx_ty> for #fn_ident {
                fn name(&self) -> &str { #tool_name }
                fn description(&self) -> &str { #description }

                fn schema(&self) -> &::serde_json::Value {
                    __HELIKON_INPUT_SCHEMA.get_or_init(|| {
                        ::serde_json::to_value(::schemars::schema_for!(#args_ty))
                            .expect("schemars schema must serialize")
                    })
                }

                fn output_schema(&self) -> ::std::option::Option<&::serde_json::Value> {
                    __HELIKON_OUTPUT_SCHEMA
                        .get_or_init(|| {
                            // Autoref-specialization: trait impl on `&Probe<T: JsonSchema>`
                            // wins (one deref) when bound holds; otherwise inherent
                            // fallback returns None. See core::__private.
                            (&&#core::__private::OutputSchemaProbe::<#out_ty>::NEW)
                                .schema()
                        })
                        .as_ref()
                }

                async fn invoke(
                    &self,
                    ctx: &#core::ToolContext<#ctx_ty>,
                    args: ::serde_json::Value,
                ) -> ::std::result::Result<#core::ToolOutput, #core::ToolError> {
                    let parsed: #args_ty = ::serde_json::from_value(args)
                        .map_err(|e| #core::ToolError::InvalidArgs {
                            schema_errors: ::std::vec![e.to_string()],
                        })?;
                    let out = #fn_ident(ctx, parsed).await?;
                    let content = ::serde_json::to_value(&out)
                        .map_err(|e| #core::ToolError::Other(e.into()))?;
                    ::std::result::Result::Ok(#core::ToolOutput::new(content))
                }
            }
        };
    };

    Ok(expanded)
}

fn resolve_description(
    attr: &ToolAttrArgs,
    docs: &[syn::Attribute],
    item_fn: &ItemFn,
) -> Result<TokenStream, Error> {
    if let Some(lit) = &attr.description {
        let value = lit.value();
        if value.is_empty() {
            return Err(Error::new_spanned(
                lit,
                "empty `description`; provide a non-empty literal or remove \
                 the attr to fall back to doc comments",
            ));
        }
        return Ok(quote!(#lit));
    }
    if let Some(text) = description_from_docs(docs) {
        let lit = LitStr::new(&text, Span::call_site());
        return Ok(quote!(#lit));
    }
    Err(Error::new_spanned(
        &item_fn.sig.ident,
        format!(
            "tool `{}` requires a description: add `#[tool(description = \"…\")]` \
             or a `///` doc comment",
            item_fn.sig.ident,
        ),
    ))
}

fn resolve_tool_name(attr: &ToolAttrArgs, item_fn: &ItemFn) -> Result<TokenStream, Error> {
    let (name_str, span) = if let Some(lit) = &attr.name {
        (lit.value(), lit.span())
    } else {
        // Strip raw-ident prefix `r#` if present.
        let raw = item_fn.sig.ident.to_string();
        let stripped = raw.strip_prefix("r#").unwrap_or(&raw).to_owned();
        (stripped, item_fn.sig.ident.span())
    };

    if !is_valid_name(&name_str) {
        return Err(Error::new(
            span,
            format!("tool name must match `[A-Za-z_][A-Za-z0-9_-]*`; got \"{name_str}\""),
        ));
    }

    let lit = LitStr::new(&name_str, span);
    Ok(quote!(#lit))
}

fn is_valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub(crate) fn tools(input: TokenStream) -> Result<TokenStream, Error> {
    use syn::parse::Parser;

    // Propagate syn's error directly so the diagnostic points at the
    // offending token, not at the macro call site as a whole.
    let parsed = ToolsInput::parse.parse2(input)?;

    if parsed.tools.is_empty() {
        return Err(Error::new(
            Span::call_site(),
            "tools! expects at least one tool; use \
             `Vec::<Arc<dyn Tool<Ctx>>>::new()` for an empty registry",
        ));
    }

    let core = resolve_core_path(parsed.crate_path.as_ref(), Span::call_site())?;
    let tools = &parsed.tools;

    Ok(quote! {
        {
            let __r: ::std::vec::Vec<::std::sync::Arc<dyn #core::Tool<_>>> = ::std::vec![
                #(
                    ::std::sync::Arc::new(#tools)
                        as ::std::sync::Arc<dyn #core::Tool<_>>
                ),*
            ];
            __r
        }
    })
}

struct ToolsInput {
    crate_path: Option<syn::Path>,
    tools: Vec<syn::Expr>,
}

impl ToolsInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let crate_path = if input.peek(syn::Token![crate]) {
            let _: syn::Token![crate] = input.parse()?;
            let _: syn::Token![=] = input.parse()?;
            let path: syn::Path = input.parse()?;
            let _: syn::Token![;] = input.parse()?;
            Some(path)
        } else {
            None
        };

        let mut tools = Vec::new();
        while !input.is_empty() {
            tools.push(input.parse::<syn::Expr>()?);
            if input.is_empty() {
                break;
            }
            let _: syn::Token![,] = input.parse()?;
        }

        Ok(ToolsInput { crate_path, tools })
    }
}
