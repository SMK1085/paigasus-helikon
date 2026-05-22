//! Validation and decomposition of the `async fn` a `#[tool]` attribute targets.

// TODO(SMA-315): drop these dead_code allows once expand::tool consumes ToolSignature + PartitionedAttrs.

use syn::{
    Attribute, Error, FnArg, GenericArgument, ItemFn, Pat, PatType, PathArguments,
    Result, ReturnType, Type, TypePath, TypeReference,
};

/// Decomposed view of the user's `async fn`.
#[allow(dead_code)]
pub(crate) struct ToolSignature<'a> {
    pub item: &'a ItemFn,
    /// `Ctx` extracted from `&ToolContext<Ctx>`.
    pub ctx_ty: Type,
    /// Type of the args struct (second positional argument).
    pub args_ty: Type,
    /// `Out` from `Result<Out, _>` in the return type.
    pub out_ty: Type,
}

impl<'a> ToolSignature<'a> {
    /// Parse + validate. Errors describe what the macro looked for,
    /// not what the user did.
    #[allow(dead_code)]
    pub(crate) fn from_item(item: &'a ItemFn) -> Result<Self> {
        let sig = &item.sig;

        if sig.asyncness.is_none() {
            return Err(Error::new_spanned(
                sig.fn_token,
                "#[tool] requires an `async fn`",
            ));
        }
        if let Some(unsafe_tok) = &sig.unsafety {
            return Err(Error::new_spanned(
                unsafe_tok,
                "#[tool] cannot wrap an `unsafe fn`; `Tool::invoke` is safe — \
                 drop the `unsafe` qualifier or inline the unsafe block inside the body",
            ));
        }
        if let Some(const_tok) = &sig.constness {
            return Err(Error::new_spanned(
                const_tok,
                "#[tool] cannot wrap a `const fn`; `Tool::invoke` is not const",
            ));
        }
        if let Some(abi) = &sig.abi {
            return Err(Error::new_spanned(
                abi,
                "#[tool] cannot wrap a fn with an `extern` ABI; remove the ABI specifier",
            ));
        }
        if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
            return Err(Error::new_spanned(
                &sig.generics,
                "#[tool] does not support generic free fns or `where` clauses; \
                 instantiate the generic and apply #[tool] to the concrete fn",
            ));
        }

        // Must be a free fn — no `self`, no trait-method form.
        if sig.inputs.iter().any(|a| matches!(a, FnArg::Receiver(_))) {
            return Err(Error::new_spanned(
                sig.fn_token,
                "#[tool] applies to free `async fn` only",
            ));
        }

        if sig.inputs.len() != 2 {
            return Err(Error::new_spanned(
                &sig.inputs,
                "#[tool] expects two args: `&ToolContext<Ctx>` and an args struct",
            ));
        }

        let mut iter = sig.inputs.iter();
        let ctx_arg = iter.next().unwrap();
        let args_arg = iter.next().unwrap();

        let ctx_ty = extract_ctx_ty(ctx_arg)?;
        let args_ty = extract_arg_ty(args_arg)?;
        let out_ty = extract_out_ty(&sig.output)?;

        Ok(ToolSignature {
            item,
            ctx_ty,
            args_ty,
            out_ty,
        })
    }
}

/// `_ctx: &…::ToolContext<Ctx>` — match on the trailing path segment.
fn extract_ctx_ty(arg: &FnArg) -> Result<Type> {
    let PatType { ty, .. } = match arg {
        FnArg::Typed(pt) => pt,
        FnArg::Receiver(_) => unreachable!(),
    };

    let TypeReference { elem, .. } = match &**ty {
        Type::Reference(r) => r,
        other => {
            return Err(Error::new_spanned(other, ctx_match_diagnostic()));
        }
    };

    let TypePath { path, .. } = match &**elem {
        Type::Path(p) => p,
        other => return Err(Error::new_spanned(other, ctx_match_diagnostic())),
    };

    let last = path
        .segments
        .last()
        .ok_or_else(|| Error::new_spanned(path, ctx_match_diagnostic()))?;

    if last.ident != "ToolContext" {
        return Err(Error::new_spanned(path, ctx_match_diagnostic()));
    }

    let generics = match &last.arguments {
        PathArguments::AngleBracketed(g) => g,
        _ => return Err(Error::new_spanned(path, ctx_match_diagnostic())),
    };

    let mut tys = generics.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t.clone()),
        _ => None,
    });
    let ctx_ty = tys
        .next()
        .ok_or_else(|| Error::new_spanned(generics, ctx_match_diagnostic()))?;
    if tys.next().is_some() {
        return Err(Error::new_spanned(generics, ctx_match_diagnostic()));
    }

    Ok(ctx_ty)
}

fn ctx_match_diagnostic() -> &'static str {
    "#[tool] expects the first argument to be `&…::ToolContext<Ctx>` (matched on \
     the trailing path segment `ToolContext` with one type argument); aliases and \
     renames are not unwrapped — name the type directly"
}

fn extract_arg_ty(arg: &FnArg) -> Result<Type> {
    match arg {
        FnArg::Typed(PatType { ty, pat, .. }) => {
            if matches!(&**pat, Pat::Wild(_)) {
                return Err(Error::new_spanned(
                    pat,
                    "#[tool] requires the second argument to have a binding name (not `_`)",
                ));
            }
            Ok((**ty).clone())
        }
        FnArg::Receiver(_) => unreachable!(),
    }
}

fn extract_out_ty(output: &ReturnType) -> Result<Type> {
    let ty = match output {
        ReturnType::Type(_, t) => &**t,
        ReturnType::Default => {
            return Err(Error::new_spanned(
                output,
                "#[tool] expects a `Result<Out, E>` return type",
            ));
        }
    };

    let TypePath { path, .. } = match ty {
        Type::Path(p) => p,
        other => {
            return Err(Error::new_spanned(
                other,
                "#[tool] expects a `Result<Out, E>` return type",
            ));
        }
    };

    let last = path
        .segments
        .last()
        .ok_or_else(|| Error::new_spanned(path, "#[tool] expects a `Result<Out, E>` return type"))?;
    if last.ident != "Result" {
        return Err(Error::new_spanned(
            path,
            "#[tool] expects a `Result<Out, E>` return type",
        ));
    }

    let generics = match &last.arguments {
        PathArguments::AngleBracketed(g) => g,
        _ => {
            return Err(Error::new_spanned(
                path,
                "#[tool] expects a `Result<Out, E>` return type",
            ));
        }
    };

    let mut tys = generics.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t.clone()),
        _ => None,
    });
    tys.next()
        .ok_or_else(|| Error::new_spanned(generics, "#[tool] expects a `Result<Out, E>` return type"))
}

/// Separates `#[tool(...)]` and `#[doc = "..."]` from forwarded attrs.
#[allow(dead_code)]
pub(crate) struct PartitionedAttrs {
    pub tool_attrs: Vec<Attribute>,
    pub doc_attrs: Vec<Attribute>,
    pub forward_attrs: Vec<Attribute>,
}

#[allow(dead_code)]
pub(crate) fn partition_attrs(attrs: &[Attribute]) -> PartitionedAttrs {
    let mut tool_attrs = Vec::new();
    let mut doc_attrs = Vec::new();
    let mut forward_attrs = Vec::new();
    for a in attrs {
        if a.path().is_ident("tool") {
            tool_attrs.push(a.clone());
        } else if a.path().is_ident("doc") {
            doc_attrs.push(a.clone());
        } else {
            forward_attrs.push(a.clone());
        }
    }
    PartitionedAttrs {
        tool_attrs,
        doc_attrs,
        forward_attrs,
    }
}

/// First-paragraph description extracted from `#[doc = "…"]` attrs.
/// Returns `None` when no doc attrs are present.
#[allow(dead_code)]
pub(crate) fn description_from_docs(doc_attrs: &[Attribute]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for a in doc_attrs {
        if let syn::Meta::NameValue(nv) = &a.meta {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
            {
                let raw = s.value();
                lines.push(raw.strip_prefix(' ').unwrap_or(&raw).to_owned());
            }
        }
    }
    if lines.is_empty() {
        return None;
    }
    let joined = lines.join("\n");
    let trimmed = joined.trim_end().to_owned();
    let first_para = trimmed.split("\n\n").next().unwrap_or("").to_owned();
    if first_para.is_empty() {
        None
    } else {
        Some(first_para)
    }
}
