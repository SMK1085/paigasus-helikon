//! Codegen for `#[tool]` and `tools!`.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
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
    let helper_mod = format_ident!("__helikon_tool_{}", fn_ident);

    let ctx_ty = &sig.ctx_ty;
    let args_ty = &sig.args_ty;
    let out_ty = &sig.out_ty;

    let forward_attrs = &partitioned.forward_attrs;

    // Helper fn signature is forwarded *verbatim* from the user's syn::Signature
    // (no path normalization). Inside `mod __helikon_tool_<ident> { use super::*; … }`
    // the user's `&ToolContext<Ctx>` and `Result<Out, anyhow::Error>` resolve via the glob.
    let helper_sig = &item_fn.sig;
    let helper_body = &item_fn.block;

    let expanded = quote! {
        #[allow(non_camel_case_types)]
        #vis struct #fn_ident;

        // `non_snake_case` allow is required because the module name embeds
        // the user's identifier — e.g. `__helikon_tool_legacyAdd` for
        // `async fn legacyAdd`. Without this, a project-wide
        // `#![deny(non_snake_case)]` would fail inside macro-expanded code.
        #[allow(non_snake_case)]
        mod #helper_mod {
            use super::*;

            pub(super) static INPUT_SCHEMA:
                ::std::sync::OnceLock<::serde_json::Value> =
                ::std::sync::OnceLock::new();
            pub(super) static OUTPUT_SCHEMA:
                ::std::sync::OnceLock<::std::option::Option<::serde_json::Value>> =
                ::std::sync::OnceLock::new();

            #(#forward_attrs)*
            pub(super) #helper_sig #helper_body
        }

        #[#core::__private::async_trait::async_trait]
        impl #core::Tool<#ctx_ty> for #fn_ident {
            fn name(&self) -> &str { #tool_name }
            fn description(&self) -> &str { #description }

            fn schema(&self) -> &::serde_json::Value {
                #helper_mod::INPUT_SCHEMA.get_or_init(|| {
                    ::serde_json::to_value(::schemars::schema_for!(#args_ty))
                        .expect("schemars schema must serialize")
                })
            }

            fn output_schema(&self) -> ::std::option::Option<&::serde_json::Value> {
                #helper_mod::OUTPUT_SCHEMA
                    .get_or_init(|| {
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
                let out = #helper_mod::#fn_ident(ctx, parsed).await?;
                let content = ::serde_json::to_value(&out)
                    .map_err(|e| #core::ToolError::Other(e.into()))?;
                ::std::result::Result::Ok(#core::ToolOutput::new(content))
            }
        }
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
            format!(
                "tool name must match `[A-Za-z_][A-Za-z0-9_-]*`; got \"{name_str}\""
            ),
        ));
    }

    let lit = LitStr::new(&name_str, span);
    Ok(quote!(#lit))
}

fn is_valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return false; };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub(crate) fn tools(_input: TokenStream) -> Result<TokenStream, Error> {
    Err(Error::new(
        Span::call_site(),
        "tools! not implemented yet — placeholder from Phase C1",
    ))
}
