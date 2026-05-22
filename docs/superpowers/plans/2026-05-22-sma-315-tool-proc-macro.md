# SMA-315 — `#[tool]` proc-macro Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `#[tool]` and `tools![…]` in `paigasus-helikon-macros`, plus the supporting `paigasus-helikon-core::__private` module, satisfying SMA-315's three acceptance criteria.

**Architecture:** Two function-like proc-macros (`#[tool]` attribute + `tools!` function-like) in `paigasus-helikon-macros`. Both auto-resolve their reference to the support crate via `proc-macro-crate` (probes `paigasus-helikon-core` first, falls back to `paigasus-helikon`). A new `#[doc(hidden)] pub mod __private` in `paigasus-helikon-core` hosts the `OutputSchemaProbe` autoref-specialization helper and a re-export of `async_trait`, both referenced from macro-generated code. The user's fn body is moved verbatim into a sibling `mod __helikon_tool_<ident>` so a `use super::*;` glob carries any types they wrote.

**Tech Stack:** Rust 1.75 (workspace MSRV), `proc-macro2` / `quote` / `syn 2` for codegen, `proc-macro-crate 3` for support-crate path resolution, `schemars 1` for JSON Schema, `async-trait` for the object-safe `Tool` trait, `trybuild` + `insta` + `rustversion` for tests (UI suite gated to stable).

**Spec:** `docs/superpowers/specs/2026-05-22-sma-315-tool-proc-macro-design.md`

**Branch:** `feature/sma-315-tool-proc-macro-with-schemars-derived-json-schema` (already created on this worktree; spec commits already landed).

**Commit convention:** Every code commit uses `feat(macros): SMA-315 <message>` or `feat(core): SMA-315 <message>` depending on the crate. The local commit-msg hook (SMA-335) enforces Conventional Commits with this scope allowlist; the `pr-title.yml` workflow re-validates on PR. Cross-crate `Cargo.toml` edits use `chore(workspace): SMA-315 …`. Never use `--no-verify`. Plan-doc commit uses `docs(plans): SMA-315 …`.

---

## Phase A — Foundation: deps, core `__private`, macros scaffold

### Task A1: Pin new workspace dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root, `[workspace.dependencies]` block)

- [ ] **Step 1: Add the five new pins**

In `[workspace.dependencies]`, add (alphabetical placement; place `proc-macro-crate` and `proc-macro2` together, `quote` after `rmcp`, `syn` after `tokio-util`, `rustversion` after `insta`, `trybuild` after `tracing`):

```toml
proc-macro2      = "1"
proc-macro-crate = "3"
quote            = "1"
syn              = { version = "2", features = ["full"] }
trybuild         = "1"
rustversion      = "1"
```

`schemars`, `serde`, `serde_json`, `tokio`, `async-trait`, `insta` are already declared.

- [ ] **Step 2: Verify cargo metadata resolves**

Run: `cargo metadata --format-version 1 --no-deps > /dev/null`
Expected: exits 0 with no output.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "$(cat <<'EOF'
chore(workspace): SMA-315 pin proc-macro deps for #[tool] macro

Pins proc-macro2, quote, syn (features = ["full"], no extra-traits),
proc-macro-crate, trybuild, rustversion. All five are required for the
SMA-315 #[tool] / tools! macros and their test infrastructure. syn
omits `extra-traits` deliberately — it adds Debug/Eq/Hash derives
across the whole AST and measurably slows macro-crate compile time
without serving codegen.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

`chore(workspace)` is correct (not `feat`) because this commit only touches workspace metadata.

---

### Task A2: Add the `paigasus-helikon-core::__private` module

**Files:**
- Create: `crates/paigasus-helikon-core/src/__private.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs`

- [ ] **Step 1: Create the module file**

Create `crates/paigasus-helikon-core/src/__private.rs` with the following exact content. (Rust allows filenames with leading underscores and `mod __private;` resolves to `__private.rs` directly — no aliasing required.)

```rust
//! Implementation details exposed to macro-generated code.
//!
//! **Semver-exempt.** Items in this module are not part of the public API.
//! Only the `#[tool]` and `tools!` macros in `paigasus-helikon-macros`
//! are expected to reference them. Direct use by application code is
//! unsupported and may break without notice.

use std::marker::PhantomData;

// Re-export so generated code can name it absolutely via
// `::paigasus_helikon_core::__private::async_trait::async_trait`.
pub use async_trait;

/// Type-level probe used by `#[tool]` to decide whether `Out: JsonSchema`.
///
/// The macro emits `(&&OutputSchemaProbe::<Out>::NEW).schema()`. Method
/// resolution starts at `&&Probe<Out>`, auto-derefs once to `&Probe<Out>`,
/// and finds the `OutputSchemaProbeSpec::schema` impl iff
/// `Out: JsonSchema`. If the bound holds, the specialized arm wins
/// (fewer deref steps); otherwise resolution falls through to the
/// inherent `fn schema(&self) -> None` fallback.
pub struct OutputSchemaProbe<T>(PhantomData<T>);

impl<T> OutputSchemaProbe<T> {
    /// Construct the probe (used by macro-generated code).
    pub const NEW: Self = Self(PhantomData);
}

/// Trait that carries the specialized arm of the autoref-specialization
/// trick. `OutputSchemaProbeSpec for &OutputSchemaProbe<T>` is one
/// deref step closer than the inherent fallback, so method resolution
/// prefers it when `T: JsonSchema` holds.
pub trait OutputSchemaProbeSpec {
    /// Return the JSON Schema for `T`, or `None` if `T: JsonSchema` does not hold.
    fn schema(&self) -> Option<serde_json::Value>;
}

impl<T: schemars::JsonSchema> OutputSchemaProbeSpec for &OutputSchemaProbe<T> {
    fn schema(&self) -> Option<serde_json::Value> {
        serde_json::to_value(schemars::schema_for!(T)).ok()
    }
}

impl<T> OutputSchemaProbe<T> {
    /// Fallback arm — runs when `T: JsonSchema` does not hold. Returns `None`.
    pub fn schema(&self) -> Option<serde_json::Value> {
        None
    }
}
```

- [ ] **Step 2: Register the module in `lib.rs`**

In `crates/paigasus-helikon-core/src/lib.rs`, after the existing `pub mod tool;` line, add:

```rust
#[doc(hidden)]
pub mod __private;
```

The publicly-reachable path is `paigasus_helikon_core::__private`. No `pub use` alias is needed; rustc handles the underscore-prefixed module name directly.

- [ ] **Step 3: Verify the module compiles**

Run: `cargo build -p paigasus-helikon-core`
Expected: exits 0 with no warnings.

- [ ] **Step 4: Add a unit test for the autoref-specialization**

Create `crates/paigasus-helikon-core/tests/private_probe.rs`:

```rust
//! Locks the autoref-specialization behavior of OutputSchemaProbe.
//! If this test breaks, the #[tool] macro's output_schema() codegen
//! will silently regress.

use paigasus_helikon_core::__private::{OutputSchemaProbe, OutputSchemaProbeSpec as _};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Serialize, JsonSchema)]
struct HasSchema {
    x: i32,
}

struct NoSchema;

#[test]
fn jsonschema_type_picks_specialized_arm() {
    let v = (&&OutputSchemaProbe::<HasSchema>::NEW).schema();
    assert!(v.is_some(), "Out: JsonSchema must produce Some(schema)");
}

#[test]
fn non_jsonschema_type_picks_fallback_arm() {
    let v = (&&OutputSchemaProbe::<NoSchema>::NEW).schema();
    assert!(v.is_none(), "Out without JsonSchema must produce None");
}
```

- [ ] **Step 5: Run the test**

Run: `cargo test -p paigasus-helikon-core --test private_probe`
Expected: 2 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/__private.rs \
        crates/paigasus-helikon-core/src/lib.rs \
        crates/paigasus-helikon-core/tests/private_probe.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-315 add __private module for #[tool] codegen support

Hosts the OutputSchemaProbe autoref-specialization helper and a
re-export of async_trait, both referenced by paigasus-helikon-macros'
generated code. Proc-macro crates cannot publicly export support
types, so this module lives in core — same pattern as
serde_derive ↔ serde::__private.

The module is #[doc(hidden)] and the rustdoc declares it semver-exempt.
Direct application use is unsupported.

Locks the autoref behavior with two assertions: HasSchema (derives
JsonSchema) picks the specialized arm; NoSchema picks the fallback.
A future refactor that flips the deref-level arrangement would fail
the second assertion immediately.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task A3: Wire `paigasus-helikon-macros` Cargo.toml

**Files:**
- Modify: `crates/paigasus-helikon-macros/Cargo.toml`

- [ ] **Step 1: Replace the file**

Overwrite `crates/paigasus-helikon-macros/Cargo.toml` with:

```toml
[package]
name        = "paigasus-helikon-macros"
description = "Proc macros for the Paigasus Helikon AI SDK."
version                = "0.0.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[lib]
proc-macro = true

[dependencies]
proc-macro2      = { workspace = true }
proc-macro-crate = { workspace = true }
quote            = { workspace = true }
syn              = { workspace = true }

[dev-dependencies]
paigasus-helikon-core = { path = "../paigasus-helikon-core" }
paigasus-helikon      = { path = "../paigasus-helikon", features = ["macros"] }
async-trait  = { workspace = true }
schemars     = { workspace = true }
serde        = { workspace = true }
serde_json   = { workspace = true }
tokio        = { workspace = true, features = ["macros", "rt"] }
anyhow       = { workspace = true }
trybuild     = { workspace = true }
insta        = { workspace = true, features = ["json"] }
rustversion  = { workspace = true }

[lints]
workspace = true
```

`paigasus-helikon` as a dev-dep creates a workspace-internal cycle (macros's dev-deps → facade → macros). Cargo allows this because dev-deps don't propagate to library builds — same pattern serde_derive uses for its tests.

- [ ] **Step 2: Verify cargo can resolve the manifest**

Run: `cargo metadata --format-version 1 --no-deps > /dev/null`
Expected: exits 0.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-macros/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(macros): SMA-315 wire deps for #[tool] proc-macro

proc-macro2/quote/syn/proc-macro-crate as direct deps for codegen.
Dev-deps cover the schema_golden + end_to_end + trybuild test trio:
schemars, serde, serde_json, async-trait, tokio (macros+rt features),
anyhow, trybuild, insta, rustversion. paigasus-helikon-core (path)
for the macro under test; paigasus-helikon (path, features=["macros"])
to exercise the facade-only-consumer trybuild compile-pass case.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task A4: Scaffold the macros crate source layout

**Files:**
- Modify: `crates/paigasus-helikon-macros/src/lib.rs`
- Create: `crates/paigasus-helikon-macros/src/attr.rs`
- Create: `crates/paigasus-helikon-macros/src/signature.rs`
- Create: `crates/paigasus-helikon-macros/src/resolve.rs`
- Create: `crates/paigasus-helikon-macros/src/expand.rs`

- [ ] **Step 1: Replace `lib.rs` with the scaffold**

Overwrite `crates/paigasus-helikon-macros/src/lib.rs`:

```rust
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
```

- [ ] **Step 2: Create empty `attr.rs` stub**

Create `crates/paigasus-helikon-macros/src/attr.rs`:

```rust
//! Attribute parsing for `#[tool(...)]` and the `tools!` `crate = ...;` prefix.

// Populated by Task B1.
```

- [ ] **Step 3: Create empty `signature.rs` stub**

Create `crates/paigasus-helikon-macros/src/signature.rs`:

```rust
//! Validation and decomposition of the `async fn` a `#[tool]` attribute targets.

// Populated by Task B2.
```

- [ ] **Step 4: Create empty `resolve.rs` stub**

Create `crates/paigasus-helikon-macros/src/resolve.rs`:

```rust
//! Resolves the path stem for `paigasus-helikon-core` symbols
//! referenced by generated code.

// Populated by Task B3.
```

- [ ] **Step 5: Create `expand.rs` with the two entry points stubbed**

Create `crates/paigasus-helikon-macros/src/expand.rs`:

```rust
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
```

- [ ] **Step 6: Verify everything compiles**

Run: `cargo build -p paigasus-helikon-macros`
Expected: exits 0 with no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-macros/src/
git commit -m "$(cat <<'EOF'
feat(macros): SMA-315 scaffold #[tool] / tools! module layout

Five files: lib.rs declares the two #[proc_macro*] entry points and
delegates to expand.rs; attr.rs, signature.rs, resolve.rs, expand.rs
are empty stubs populated by subsequent tasks. Both entry points
return a compile_error placeholder so any premature use surfaces
immediately rather than silently expanding to nothing.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase B — Parsing: attributes, signatures, support-crate resolution

### Task B1: Parse `#[tool(description, name, crate)]`

**Files:**
- Modify: `crates/paigasus-helikon-macros/src/attr.rs`

- [ ] **Step 1: Replace `attr.rs`**

Overwrite `crates/paigasus-helikon-macros/src/attr.rs`:

```rust
//! Attribute parsing for `#[tool(...)]`.

use proc_macro2::Span;
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
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p paigasus-helikon-macros`
Expected: exits 0 with no warnings.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-macros/src/attr.rs
git commit -m "$(cat <<'EOF'
feat(macros): SMA-315 parse #[tool(description, name, crate)] attribute

ToolAttrArgs accepts zero or more comma-separated `key = value` pairs.
Unknown keys emit a spanned error pointing at the key. Validation of
the `name` regex and the empty-description rule lives in expand.rs
where it can be combined with doc-comment fallback.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B2: Validate and decompose the `async fn` signature

**Files:**
- Modify: `crates/paigasus-helikon-macros/src/signature.rs`

- [ ] **Step 1: Replace `signature.rs`**

Overwrite `crates/paigasus-helikon-macros/src/signature.rs`:

```rust
//! Validation and decomposition of the `async fn` a `#[tool]` attribute targets.

use syn::{
    Attribute, Error, FnArg, GenericArgument, ItemFn, Pat, PatType, PathArguments,
    Result, ReturnType, Type, TypePath, TypeReference,
};

/// Decomposed view of the user's `async fn`.
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
    pub(crate) fn from_item(item: &'a ItemFn) -> Result<Self> {
        let sig = &item.sig;

        if sig.asyncness.is_none() {
            return Err(Error::new_spanned(
                &sig.fn_token,
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
        if !sig.generics.params.is_empty() {
            return Err(Error::new_spanned(
                &sig.generics,
                "#[tool] does not support generic free fns; instantiate the generic \
                 and apply #[tool] to the concrete fn",
            ));
        }

        // Must be a free fn — no `self`, no trait-method form.
        if sig.inputs.iter().any(|a| matches!(a, FnArg::Receiver(_))) {
            return Err(Error::new_spanned(
                &sig.fn_token,
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
            return Err(Error::new_spanned(
                other,
                ctx_match_diagnostic(),
            ));
        }
    };

    let TypePath { path, .. } = match &**elem {
        Type::Path(p) => p,
        other => return Err(Error::new_spanned(other, ctx_match_diagnostic())),
    };

    let last = path.segments.last().ok_or_else(|| {
        Error::new_spanned(path, ctx_match_diagnostic())
    })?;

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
pub(crate) struct PartitionedAttrs {
    pub tool_attrs: Vec<Attribute>,
    pub doc_attrs: Vec<Attribute>,
    pub forward_attrs: Vec<Attribute>,
}

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

```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p paigasus-helikon-macros`
Expected: exits 0 with no warnings. `Error::new_spanned` is a free function — the `Spanned` trait does **not** need to be in scope for it to work.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-macros/src/signature.rs
git commit -m "$(cat <<'EOF'
feat(macros): SMA-315 parse + validate #[tool] async fn signature

ToolSignature carries the user's ItemFn plus extracted Ctx, Args, and
Out types. Validation rejects:
- non-async fn, unsafe fn, const fn, extern "..." fn (each with its
  own diagnostic spanned at the offending qualifier);
- generic free fns;
- arity ≠ 2;
- first arg not `&…::ToolContext<…>` (trailing-path-segment match);
- second arg with `_` binding;
- return type that isn't `Result<_, _>`.

partition_attrs splits the fn's attribute list into #[tool(...)],
#[doc = "..."], and forward-to-helper categories.

description_from_docs joins per-#[doc]-attr strings, strips a single
leading space per line, trims trailing whitespace, and returns the
first paragraph only — per spec §5.1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B3: Resolve the support-crate path

**Files:**
- Modify: `crates/paigasus-helikon-macros/src/resolve.rs`

- [ ] **Step 1: Replace `resolve.rs`**

Overwrite `crates/paigasus-helikon-macros/src/resolve.rs`:

```rust
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
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p paigasus-helikon-macros`
Expected: exits 0 with no warnings.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-macros/src/resolve.rs
git commit -m "$(cat <<'EOF'
feat(macros): SMA-315 auto-resolve support-crate path

resolve_core_path probes the consumer's Cargo.toml via proc-macro-crate
for paigasus-helikon-core first, then falls back to paigasus-helikon
(facade). Honors renamed deps (FoundCrate::Name). #[tool(crate = ::path)]
override short-circuits both probes.

The facade fallback emits `::<n>::core` because paigasus-helikon
re-exports core unconditionally as `core` (facade lib.rs:4).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase C — Codegen for `#[tool]`

### Task C1: `#[tool]` core expansion (struct, helper mod, `impl` shell)

**Files:**
- Modify: `crates/paigasus-helikon-macros/src/expand.rs`

- [ ] **Step 1: Replace `expand.rs`'s `tool` function**

Replace the body of `pub(crate) fn tool(...)` in `crates/paigasus-helikon-macros/src/expand.rs` with the full implementation. Final file contents:

```rust
//! Codegen for `#[tool]` and `tools!`.

use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};
use syn::{parse2, spanned::Spanned, Error, ItemFn, LitStr, Path, Token};

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
    let core = resolve_core_path(attr_args.crate_path.as_ref(), item_fn.sig.fn_token.span())?;

    let vis = &item_fn.vis;
    let fn_ident = &item_fn.sig.ident;
    let helper_mod = format_ident!("__helikon_tool_{}", fn_ident);
    let input_schema_static = format_ident!("INPUT_SCHEMA");
    let output_schema_static = format_ident!("OUTPUT_SCHEMA");

    let ctx_ty = &sig.ctx_ty;
    let args_ty = &sig.args_ty;
    let out_ty = &sig.out_ty;

    let forward_attrs = &partitioned.forward_attrs;

    // Helper fn signature is forwarded *verbatim* from the user's syn::Signature.
    let helper_sig = &item_fn.sig;
    let helper_body = &item_fn.block;

    let expanded = quote! {
        #[allow(non_camel_case_types)]
        #vis struct #fn_ident;

        #[allow(non_snake_case)]
        mod #helper_mod {
            use super::*;

            pub(super) static #input_schema_static:
                ::std::sync::OnceLock<::serde_json::Value> =
                ::std::sync::OnceLock::new();
            pub(super) static #output_schema_static:
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
                #helper_mod::#input_schema_static.get_or_init(|| {
                    ::serde_json::to_value(::schemars::schema_for!(#args_ty))
                        .expect("schemars schema must serialize")
                })
            }

            fn output_schema(&self) -> ::std::option::Option<&::serde_json::Value> {
                #helper_mod::#output_schema_static
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
```

**Note on the helper-fn name in `invoke`:** the expansion uses `#helper_mod::#fn_ident` to call the helper. Since the helper module is named after the fn and the moved fn keeps its original ident inside the module, `__helikon_tool_add::add(ctx, parsed)` is the call form. (The §4.2 reference expansion in the spec uses `run` as the helper-fn name for narrative simplicity; the implementation reuses the original ident for less codegen branching — adjust §4.2 if reviewer flags it as a discrepancy. It's a non-load-bearing prose detail.)

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p paigasus-helikon-macros`
Expected: exits 0 with no warnings.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-macros/src/expand.rs
git commit -m "$(cat <<'EOF'
feat(macros): SMA-315 implement #[tool] codegen

Emits, in order:
- unit struct named after the fn (vis preserved);
- mod __helikon_tool_<ident> wrapping the user's body verbatim, with
  forwarded non-tool/non-doc attributes on the helper fn;
- impl Tool<Ctx> with name/description/schema/output_schema/invoke
  rooted at the auto-resolved support-crate path;
- OnceLock statics for schema caching inside the helper module.

invoke deserializes args via serde_json::from_value (errors become
ToolError::InvalidArgs), calls the helper, lets `?` convert the user's
E: Into<ToolError> (so Result<_, anyhow::Error> bodies work), and
serializes the output. The autoref-specialization probe lives in
core::__private and is reached via (&&Probe::<Out>::NEW).schema().

Description: attr-wins over doc comments; empty literal rejected;
both absent triggers a spanned compile_error. Name: attr-wins; raw
idents stripped of `r#`; regex-validated.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task C2: `tools!` codegen

**Files:**
- Modify: `crates/paigasus-helikon-macros/src/expand.rs`

- [ ] **Step 1: Replace the `tools` function**

Replace the placeholder `pub(crate) fn tools(...)` in `expand.rs` with:

```rust
pub(crate) fn tools(input: TokenStream) -> Result<TokenStream, Error> {
    use syn::parse::Parser;

    let parser = ToolsInput::parse;
    let parsed = parser.parse2(input.clone()).map_err(|e| {
        // Fallback diagnostic for empty/malformed input.
        Error::new(input.span(), format!("invalid `tools!` invocation: {e}"))
    })?;

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
            let __r: ::std::vec::Vec<
                ::std::sync::Arc<dyn #core::Tool<_>>
            > = ::std::vec![
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
    crate_path: Option<Path>,
    tools: Vec<syn::Expr>,
}

impl ToolsInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let crate_path = if input.peek(Token![crate]) {
            let _: Token![crate] = input.parse()?;
            let _: Token![=] = input.parse()?;
            let path: Path = input.parse()?;
            let _: Token![;] = input.parse()?;
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
            let _: Token![,] = input.parse()?;
        }

        Ok(ToolsInput { crate_path, tools })
    }
}
```

You will need to add `Path` to the existing `use syn::{...}` line at the top of the file if it isn't already imported.

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p paigasus-helikon-macros`
Expected: exits 0 with no warnings.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-macros/src/expand.rs
git commit -m "$(cat <<'EOF'
feat(macros): SMA-315 implement tools! codegen

ToolsInput parses an optional `crate = ::path;` prefix followed by a
comma-separated list of expressions. The macro emits a Vec of
Arc<dyn Tool<_>> with explicit `as` casts so coercion works even when
the LHS has no annotation but downstream usage pins Ctx.

Empty invocation is rejected with a dedicated diagnostic pointing at
`Vec::<Arc<dyn Tool<Ctx>>>::new()` as the alternative. Trailing comma
is accepted (handled by the while-loop terminator check).

The support-crate path uses the same proc-macro-crate auto-resolution
as #[tool], so facade-only consumers `paigasus_helikon::tools![…]`
work without ceremony.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase D — Facade re-exports + initial round-trip check

### Task D1: Re-export `tool` and `tools` from the facade

**Files:**
- Modify: `crates/paigasus-helikon/src/lib.rs`

- [ ] **Step 1: Add the re-exports**

In `crates/paigasus-helikon/src/lib.rs`, replace the line:

```rust
#[cfg(feature = "macros")]
pub use paigasus_helikon_macros as macros;
```

with:

```rust
/// Proc macros for the SDK. Enabled via the `macros` feature.
#[cfg(feature = "macros")]
pub use paigasus_helikon_macros as macros;

/// `#[tool]` attribute macro — enabled via the `macros` feature.
#[cfg(feature = "macros")]
pub use paigasus_helikon_macros::tool;

/// `tools!` function-like macro — enabled via the `macros` feature.
#[cfg(feature = "macros")]
pub use paigasus_helikon_macros::tools;
```

The existing `pub use paigasus_helikon_core as core;` re-export above remains untouched — `proc-macro-crate` resolution depends on it for facade-only consumers.

- [ ] **Step 2: Verify the facade builds with `macros`**

Run: `cargo build -p paigasus-helikon --features macros`
Expected: exits 0 with no warnings.

- [ ] **Step 3: Verify it still builds without features (no-default)**

Run: `cargo build -p paigasus-helikon`
Expected: exits 0 with no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(facade): SMA-315 re-export tool / tools macros

Adds `pub use paigasus_helikon_macros::{tool, tools};` behind the
existing `macros` feature gate. Facade-only consumers can now write
`paigasus_helikon::tool` and `paigasus_helikon::tools` without naming
the macros crate. The unconditional `pub use paigasus_helikon_core as
core;` re-export remains the resolution target for proc-macro-crate's
facade fallback path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task D2: Smoke-test the round-trip in a doctest

**Files:**
- Modify: `crates/paigasus-helikon-macros/src/lib.rs`

- [ ] **Step 1: Replace the `#[proc_macro_attribute] pub fn tool` rustdoc with a smoke-test doctest**

Replace the existing `tool` proc-macro definition (with its short docstring) with:

```rust
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
```

- [ ] **Step 2: Replace the `#[proc_macro] pub fn tools` rustdoc with a doctest**

Replace the existing `tools` proc-macro definition with:

```rust
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
```

- [ ] **Step 3: Run the doctests**

Run: `cargo test -p paigasus-helikon-macros --doc`
Expected: 2 doctests pass.

If they fail, the most likely cause is a missed import or a typo in the macro expansion. Re-read the failure carefully — doctests show the full expansion error.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-macros/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(macros): SMA-315 add doctest smoke tests for #[tool] and tools!

Each macro's rustdoc now contains an end-to-end doctest that exercises
the full expansion path: attribute parsing → signature validation →
support-crate resolution → codegen → schemars derive → Tool impl.

Doctests run automatically under `cargo test --doc` and act as a
canary against codegen regressions. The tools! rustdoc also documents
the Arc<T> footgun and the Ctx-mismatch error string per spec §10.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase E — Schema golden + end-to-end tests

### Task E1: Schema golden test (AC #1)

**Files:**
- Create: `crates/paigasus-helikon-macros/tests/schema_golden.rs`
- Create: `crates/paigasus-helikon-macros/tests/snapshots/.gitkeep`

- [ ] **Step 1: Create the test file**

Create `crates/paigasus-helikon-macros/tests/schema_golden.rs`:

```rust
//! AC #1: a two-arg tool with doc comments produces a JSON Schema
//! matching the checked-in golden file.

use paigasus_helikon_core::{Tool, ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct MyCtx;

#[derive(Deserialize, JsonSchema)]
struct AddArgs {
    /// First addend.
    a: i64,
    /// Second addend.
    b: i64,
}

#[derive(Serialize, JsonSchema)]
struct AddOut {
    sum: i64,
}

/// Adds two numbers.
#[tool]
async fn add(
    _ctx: &ToolContext<MyCtx>,
    args: AddArgs,
) -> Result<AddOut, ToolError> {
    Ok(AddOut { sum: args.a + args.b })
}

#[test]
fn add_schema_matches_golden() {
    let serialized = serde_json::to_string_pretty(add.schema()).unwrap();
    insta::assert_snapshot!(serialized);
}
```

- [ ] **Step 2: Create the snapshots directory (empty, with placeholder)**

Run:

```bash
mkdir -p crates/paigasus-helikon-macros/tests/snapshots
touch crates/paigasus-helikon-macros/tests/snapshots/.gitkeep
```

- [ ] **Step 3: Run the test (it will create the snapshot)**

Run: `INSTA_UPDATE=auto cargo test -p paigasus-helikon-macros --test schema_golden`
Expected: 1 passed. A new file `tests/snapshots/schema_golden__add_schema_matches_golden.snap` is created. Open it and eyeball the content — confirm it contains a JSON object with `"type": "object"`, a `"properties"` map with `"a"` and `"b"` (both with `"type": "integer"` and `"description"` strings), and a `"required"` array listing both.

- [ ] **Step 4: Re-run to confirm the snapshot is stable**

Run: `cargo test -p paigasus-helikon-macros --test schema_golden`
Expected: 1 passed (no `INSTA_UPDATE` needed; snapshot now matches).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-macros/tests/schema_golden.rs \
        crates/paigasus-helikon-macros/tests/snapshots/
git commit -m "$(cat <<'EOF'
test(macros): SMA-315 schema-golden snapshot (AC #1)

A two-arg tool with doc-commented args produces a JSON Schema that
serializes to the checked-in snapshot. Schemars patch/minor bumps
will trigger snapshot review (routine — accept layout-only diffs
via cargo insta review); schemars major bumps are not routine
(see spec §12).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task E2: End-to-end behavioral test

**Files:**
- Create: `crates/paigasus-helikon-macros/tests/end_to_end.rs`

- [ ] **Step 1: Create the test file**

Create `crates/paigasus-helikon-macros/tests/end_to_end.rs`:

```rust
//! End-to-end behavioral lock for #[tool] and tools!. Verifies the
//! contract specified in SMA-315's spec §6.3.
//!
//! The file-level `deny(non_snake_case)` makes step 10's
//! attribute-forwarding assertion load-bearing: if the macro fails
//! to forward `#[allow(non_snake_case)]` to the helper fn, the deny
//! turns the lint into a hard compile error.

#![deny(non_snake_case)]

use std::sync::Arc;

use anyhow::anyhow;
use paigasus_helikon_core::{
    CancellationToken, Tool, ToolContext, ToolError, ToolOutput, TracerHandle,
};
use paigasus_helikon_macros::{tool, tools};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

struct MyCtx;

fn make_ctx() -> ToolContext<MyCtx> {
    ToolContext::new(
        Arc::new(MyCtx),
        TracerHandle::default(),
        CancellationToken::new(),
    )
}

// ---------- Tool 1: AddArgs/AddOut both derive JsonSchema -------------------

#[derive(Deserialize, JsonSchema)]
struct AddArgs {
    /// First addend.
    a: i64,
    /// Second addend.
    b: i64,
}

#[derive(Serialize, JsonSchema)]
struct AddOut {
    sum: i64,
}

/// Adds two numbers.
///
/// Subsequent paragraph — must NOT appear in description().
#[tool]
async fn add(
    _ctx: &ToolContext<MyCtx>,
    args: AddArgs,
) -> Result<AddOut, ToolError> {
    Ok(AddOut { sum: args.a + args.b })
}

// ---------- Tool 2: explicit description overrides doc comment --------------

/// Long rustdoc-style description that will not be used as the
/// tool description because the attr below takes precedence.
///
/// Including a second paragraph for thoroughness.
#[tool(description = "Short.")]
async fn explicit_desc(
    _ctx: &ToolContext<MyCtx>,
    args: AddArgs,
) -> Result<AddOut, ToolError> {
    Ok(AddOut { sum: args.a + args.b })
}

// ---------- Tool 3: Out without JsonSchema → output_schema() = None ---------

#[derive(Serialize)]
struct OpaqueOut(String);

/// A tool whose output type does not derive JsonSchema.
#[tool]
async fn opaque(
    _ctx: &ToolContext<MyCtx>,
    args: AddArgs,
) -> Result<OpaqueOut, ToolError> {
    Ok(OpaqueOut(format!("{}+{}={}", args.a, args.b, args.a + args.b)))
}

// ---------- Tool 4: anyhow body — `?` does the From conversion --------------

#[derive(Deserialize, JsonSchema)]
struct EmptyArgs {}

#[derive(Serialize, JsonSchema)]
struct EmptyOut {}

/// Always fails with an anyhow error.
#[tool]
async fn anyhow_failer(
    _ctx: &ToolContext<MyCtx>,
    _args: EmptyArgs,
) -> Result<EmptyOut, anyhow::Error> {
    Err(anyhow!("boom"))
}

// ---------- Tool 5: forwarded #[allow] + camelCase name --------------------

/// Legacy adder kept around for compatibility.
#[tool]
#[allow(non_snake_case)]
async fn legacyAdd(
    _ctx: &ToolContext<MyCtx>,
    args: AddArgs,
) -> Result<AddOut, ToolError> {
    Ok(AddOut { sum: args.a + args.b })
}

// ---------- Tests ----------------------------------------------------------

#[tokio::test]
async fn registry_basics() {
    let registry: Vec<Arc<dyn Tool<MyCtx>>> = tools![add];
    assert_eq!(registry.len(), 1);
    assert_eq!(registry[0].name(), "add");
    assert_eq!(registry[0].description(), "Adds two numbers.");
}

#[tokio::test]
async fn attr_description_wins_over_doc() {
    assert_eq!(explicit_desc.description(), "Short.");
}

#[tokio::test]
async fn invoke_valid_args() {
    let ctx = make_ctx();
    let out = add.invoke(&ctx, json!({ "a": 2, "b": 3 })).await.unwrap();
    assert_eq!(out.content, json!({ "sum": 5 }));
}

#[tokio::test]
async fn invoke_invalid_args() {
    let ctx = make_ctx();
    let err = add
        .invoke(&ctx, json!({ "a": "not-a-number", "b": 3 }))
        .await
        .unwrap_err();
    match err {
        ToolError::InvalidArgs { schema_errors } => {
            assert!(!schema_errors.is_empty(), "schema_errors must be non-empty");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn schema_returns_cached_reference() {
    let s1: &serde_json::Value = add.schema();
    let s2: &serde_json::Value = add.schema();
    assert!(
        std::ptr::eq(s1, s2),
        "OnceLock must hand back the same &Value across calls"
    );
}

#[tokio::test]
async fn output_schema_present_when_jsonschema_derived() {
    assert!(add.output_schema().is_some());
}

#[tokio::test]
async fn output_schema_absent_for_non_jsonschema_out() {
    assert!(opaque.output_schema().is_none());
}

#[tokio::test]
async fn anyhow_error_surfaces_as_tool_error_other() {
    let ctx = make_ctx();
    let err = anyhow_failer.invoke(&ctx, json!({})).await.unwrap_err();
    match err {
        ToolError::Other(e) => {
            assert!(e.to_string().contains("boom"));
        }
        other => panic!("expected Other(anyhow::Error), got {other:?}"),
    }
}

#[tokio::test]
async fn forwarded_allow_silences_camelcase_lint() {
    // If `#[allow(non_snake_case)]` did not reach the helper fn (the
    // body uses `legacyAdd` as the call site through the helper mod),
    // the file-level `#![deny(non_snake_case)]` would have failed
    // compilation. Reaching this assertion proves forwarding works.
    let _ = ToolOutput::new(json!({}));
    assert_eq!(legacyAdd.name(), "legacyAdd");
}
```

- [ ] **Step 2: Run the suite**

Run: `cargo test -p paigasus-helikon-macros --test end_to_end`
Expected: 9 tests pass.

If `forwarded_allow_silences_camelcase_lint` fails to even *compile*, the macro is missing the `#[allow(non_snake_case)]` on the generated module OR not forwarding the user's `#[allow]`. Both must hold (see spec §4.2 + Decision row #10).

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-macros/tests/end_to_end.rs
git commit -m "$(cat <<'EOF'
test(macros): SMA-315 end-to-end behavioral coverage

Nine tests covering the contract specified in spec §6.3:
registry shape, attr-wins description precedence, valid/invalid
invoke paths, OnceLock pointer-equality caching, output_schema
present/absent across JsonSchema/non-JsonSchema Out, anyhow ↔
ToolError::Other via the `?`-from-conversion path, and the
attribute-forwarding load-bearing case gated by #![deny(non_snake_case)]
at file scope.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase F — Trybuild UI suite

### Task F1: Wire the trybuild entry point (stable-gated)

**Files:**
- Create: `crates/paigasus-helikon-macros/tests/trybuild.rs`
- Create: `crates/paigasus-helikon-macros/tests/ui/.gitkeep`

- [ ] **Step 1: Create the entry point**

Create `crates/paigasus-helikon-macros/tests/trybuild.rs`:

```rust
//! UI tests for #[tool] and tools!. Gated to stable rustc because
//! trybuild `.stderr` captures pin rustc diagnostic text byte-for-byte
//! and that text drifts across rustc releases — including between
//! stable and the 1.75 MSRV CI matrix entry.

#[rustversion::attr(not(stable), ignore)]
#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/bad_*.rs");
    t.compile_fail("tests/ui/no_description.rs");
    t.compile_fail("tests/ui/empty_description.rs");
    t.pass("tests/ui/facade_only_consumer.rs");
}
```

- [ ] **Step 2: Create the ui directory**

Run:

```bash
mkdir -p crates/paigasus-helikon-macros/tests/ui
touch crates/paigasus-helikon-macros/tests/ui/.gitkeep
```

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-macros/tests/trybuild.rs \
        crates/paigasus-helikon-macros/tests/ui/.gitkeep
git commit -m "$(cat <<'EOF'
test(macros): SMA-315 wire trybuild UI test entry point

The `ui` test is gated to stable rustc via rustversion::attr(not(stable),
ignore). trybuild's `.stderr` captures pin rustc diagnostic text
byte-for-byte; pinning them against both stable and 1.75 would flag
every PR red. Same convention serde, thiserror, anyhow use.

Cases are populated in subsequent tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task F2: Compile-fail cases — bad signature qualifiers

**Files:**
- Create: `crates/paigasus-helikon-macros/tests/ui/bad_signature_not_async.rs`
- Create: `crates/paigasus-helikon-macros/tests/ui/bad_signature_unsafe.rs`
- Create: `crates/paigasus-helikon-macros/tests/ui/bad_signature_const.rs`
- Create: `crates/paigasus-helikon-macros/tests/ui/bad_signature_generic.rs`
- Create: `crates/paigasus-helikon-macros/tests/ui/bad_signature_wrong_ctx.rs`

- [ ] **Step 1: Create `bad_signature_not_async.rs`**

```rust
use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// Not async.
#[tool]
fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}
```

- [ ] **Step 2: Create `bad_signature_unsafe.rs`**

```rust
use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// Unsafe.
#[tool]
unsafe async fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}
```

- [ ] **Step 3: Create `bad_signature_const.rs`**

```rust
use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// Const.
#[tool]
const fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}
```

- [ ] **Step 4: Create `bad_signature_generic.rs`**

```rust
use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// Generic.
#[tool]
async fn nope<T>(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError>
where
    T: Send,
{
    Ok(O {})
}

fn main() {}
```

- [ ] **Step 5: Create `bad_signature_wrong_ctx.rs`**

```rust
use paigasus_helikon_core::ToolError;
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// First arg isn't &ToolContext<C>.
#[tool]
async fn nope(_ctx: &C, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}
```

- [ ] **Step 6: Generate the .stderr captures**

Run: `TRYBUILD=overwrite cargo test -p paigasus-helikon-macros --test trybuild`
Expected: 5 compile-fail cases captured. Each pair `<name>.rs` + `<name>.stderr` lands in `tests/ui/`. Open each `.stderr` and confirm:

- `bad_signature_not_async.stderr` contains ``#[tool] requires an `async fn```.
- `bad_signature_unsafe.stderr` contains ``#[tool] cannot wrap an `unsafe fn```.
- `bad_signature_const.stderr` contains ``#[tool] cannot wrap a `const fn```.
- `bad_signature_generic.stderr` contains ``#[tool] does not support generic free fns``.
- `bad_signature_wrong_ctx.stderr` contains ``#[tool] expects the first argument to be `&…::ToolContext<Ctx>```.

If a `.stderr` is generated but the message doesn't match, the codegen in `signature.rs` regressed — fix the codegen, not the snapshot.

- [ ] **Step 7: Re-run to confirm stability**

Run: `cargo test -p paigasus-helikon-macros --test trybuild`
Expected: 5 compile-fail cases pass; 1 pass case is missing (will be added by Task F4) — the runner reports an error but only for the missing pass file. We'll re-run after F4.

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-macros/tests/ui/bad_signature_*
git commit -m "$(cat <<'EOF'
test(macros): SMA-315 trybuild bad-signature compile-fail cases

Five UI tests pin the macro's signature-validation diagnostics:
non-async, unsafe, const, generic free fns, and wrong first-arg type.
Each .stderr capture exercises a distinct branch of signature.rs.

Captures regenerate via TRYBUILD=overwrite when codegen messages
change intentionally; reviewers diff the .stderr files in the PR.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task F3: Compile-fail cases — descriptions, args, output, name

**Files:**
- Create: `crates/paigasus-helikon-macros/tests/ui/no_description.rs`
- Create: `crates/paigasus-helikon-macros/tests/ui/empty_description.rs`
- Create: `crates/paigasus-helikon-macros/tests/ui/bad_args_not_deserialize.rs`
- Create: `crates/paigasus-helikon-macros/tests/ui/bad_args_not_jsonschema.rs`
- Create: `crates/paigasus-helikon-macros/tests/ui/bad_out_not_serialize.rs`
- Create: `crates/paigasus-helikon-macros/tests/ui/bad_name.rs`

- [ ] **Step 1: `no_description.rs`**

```rust
use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

#[tool]
async fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}
```

- [ ] **Step 2: `empty_description.rs`**

```rust
use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

#[tool(description = "")]
async fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}
```

- [ ] **Step 3: `bad_args_not_deserialize.rs`**

```rust
use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::Serialize;

struct C;

// Missing `Deserialize`.
#[derive(JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// Description.
#[tool]
async fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}
```

- [ ] **Step 4: `bad_args_not_jsonschema.rs`**

```rust
use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

// Missing `JsonSchema`.
#[derive(Deserialize)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// Description.
#[tool]
async fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}
```

- [ ] **Step 5: `bad_out_not_serialize.rs`**

```rust
use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::Deserialize;

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

// Missing `Serialize`.
struct O {}

/// Description.
#[tool]
async fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}
```

- [ ] **Step 6: `bad_name.rs`**

```rust
use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// Description.
#[tool(name = "has spaces")]
async fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}
```

- [ ] **Step 7: Generate .stderr captures**

Run: `TRYBUILD=overwrite cargo test -p paigasus-helikon-macros --test trybuild`
Expected: the new cases get `.stderr` files. Inspect each:

- `no_description.stderr` → ``tool `nope` requires a description``.
- `empty_description.stderr` → ``empty `description```.
- `bad_args_not_deserialize.stderr` → ``the trait bound `A: ... Deserialize`` (rustc-driven).
- `bad_args_not_jsonschema.stderr` → ``the trait bound `A: ... JsonSchema`` (rustc-driven).
- `bad_out_not_serialize.stderr` → ``the trait bound `O: ... Serialize`` (rustc-driven).
- `bad_name.stderr` → ``tool name must match `[A-Za-z_][A-Za-z0-9_-]*```.

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-macros/tests/ui/no_description.* \
        crates/paigasus-helikon-macros/tests/ui/empty_description.* \
        crates/paigasus-helikon-macros/tests/ui/bad_args_*.* \
        crates/paigasus-helikon-macros/tests/ui/bad_out_*.* \
        crates/paigasus-helikon-macros/tests/ui/bad_name.*
git commit -m "$(cat <<'EOF'
test(macros): SMA-315 trybuild description/derive/name compile-fail cases

Six UI tests pin the remaining macro-emitted and rustc-emitted
diagnostics: missing/empty description, args struct missing
Deserialize / JsonSchema, output type missing Serialize, bad name
literal. The derive-bound cases verify that rustc's downstream
trait-bound errors fire on the correct lines of the generated code.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task F4: Facade-only consumer compile-pass case

**Files:**
- Create: `crates/paigasus-helikon-macros/tests/ui/facade_only_consumer.rs`

- [ ] **Step 1: Create the pass case**

Create `crates/paigasus-helikon-macros/tests/ui/facade_only_consumer.rs`:

```rust
//! Compile-pass: this file mentions only `paigasus_helikon` (the
//! facade), never `paigasus_helikon_core` directly. It locks the
//! proc-macro-crate auto-resolution: when only the facade is in the
//! dep graph, the macro must emit paths rooted at
//! `::paigasus_helikon::core::…`.

use std::sync::Arc;

use paigasus_helikon::core::{Tool, ToolContext, ToolError};
use paigasus_helikon::{tool, tools};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct MyCtx;

#[derive(Deserialize, JsonSchema)]
struct AddArgs {
    a: i64,
    b: i64,
}

#[derive(Serialize, JsonSchema)]
struct AddOut {
    sum: i64,
}

/// Adds two numbers.
#[tool]
async fn add(
    _ctx: &ToolContext<MyCtx>,
    args: AddArgs,
) -> Result<AddOut, ToolError> {
    Ok(AddOut { sum: args.a + args.b })
}

fn main() {
    let _r: Vec<Arc<dyn Tool<MyCtx>>> = tools![add];
}
```

This file inherits the macros crate's dev-deps via trybuild's transient manifest (see spec §6.2 note), so `schemars::JsonSchema` and `serde::{Deserialize, Serialize}` resolve even though the synthesized manifest only lists `paigasus-helikon`.

- [ ] **Step 2: Run the suite**

Run: `cargo test -p paigasus-helikon-macros --test trybuild`
Expected: 11 compile-fail cases pass + 1 compile-pass case passes. The pass case is the load-bearing one for resolve.rs's facade fallback.

If the pass case fails with ``use of undeclared crate or module `paigasus_helikon_core```, then `resolve_core_path` is not falling back to the facade path — debug `resolve.rs`.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-macros/tests/ui/facade_only_consumer.rs
git commit -m "$(cat <<'EOF'
test(macros): SMA-315 trybuild facade-only-consumer compile-pass

Locks the proc-macro-crate auto-resolution: a consumer that depends
only on `paigasus-helikon` (the facade), never `paigasus-helikon-core`
directly, must still compile #[tool] and tools! invocations. The
macro emits paths rooted at `::paigasus_helikon::core::…` for this
case.

Without this test, the facade path would silently regress on any
refactor of resolve.rs — exactly the bug the staff-eng review
flagged as the single most valuable test to add.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase G — Final verification

### Task G1: Full local CI gate

**Files:** (no edits — verification only)

- [ ] **Step 1: Format check**

Run: `cargo fmt --all -- --check`
Expected: exits 0 with no output.

If it fails, run `cargo fmt --all` and commit the formatting separately with `chore(workspace): SMA-315 cargo fmt`.

- [ ] **Step 2: Clippy with workspace defaults**

Run: `cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: exits 0 with no warnings.

Likely friction points:
- Missing rustdoc on a new public item (the workspace lints with `missing_docs = "warn"`, and `cargo clippy -D warnings` upgrades it). Fix by adding doc comments to any public item that lacks them.
- `unused_imports` in `signature.rs` or `expand.rs` from leftover stubs. Remove the unused import.

- [ ] **Step 3: Test suite**

Run: `cargo test --workspace --all-features`
Expected: every test passes. Specifically:
- `paigasus-helikon-macros` doctests: 2 passed.
- `paigasus-helikon-macros` `schema_golden`: 1 passed.
- `paigasus-helikon-macros` `end_to_end`: 9 passed.
- `paigasus-helikon-macros` `trybuild` (on stable): 11 compile-fail + 1 compile-pass.
- `paigasus-helikon-core` `private_probe`: 2 passed.
- All other crates' tests: unchanged from baseline.

- [ ] **Step 4: Rustdoc with `-D warnings`**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
Expected: exits 0.

Likely friction:
- A `[unresolved]` link in a doc comment. Either fix the link target or wrap in backticks.
- A public item in `__private` lacking a `#[doc]`. The module is `#[doc(hidden)]` so its contents shouldn't be required by `missing_docs`, but if rustdoc still complains, add brief doc comments per item.

- [ ] **Step 5: Doc coverage (if `nightly-2026-05-01` is installed)**

If you have the pinned nightly:

```bash
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
```

Expected: exits 0; coverage at or above 80% for every non-excluded crate. `paigasus-helikon-macros` (proc-macro crate, currently not in the missing_docs opt-in set per CLAUDE.md) is excluded from the aggregator.

If nightly isn't installed locally, skip this step — CI will run it on PR.

- [ ] **Step 6: Inspect the `cargo expand` output for AC #2**

Run: `cargo expand -p paigasus-helikon-macros --test end_to_end 2>/dev/null | head -200`

Eyeball the expansion of the `add` tool. Confirm:
- The unit struct `add` appears with `#[allow(non_camel_case_types)]`.
- The `__helikon_tool_add` module is annotated `#[allow(non_snake_case)]`.
- The `impl Tool<MyCtx> for add` block is rooted at `::paigasus_helikon_core::Tool<…>`.
- The `output_schema()` body contains the `(&&::paigasus_helikon_core::__private::OutputSchemaProbe::<…>::NEW).schema()` autoref call.
- The helper fn signature is verbatim from the user's source (no path normalization).

This is the AC #2 "readable expansion" criterion. There's no automated assertion; the PR description records that the check was done.

- [ ] **Step 7: Confirm no working-tree changes**

Run: `git status`
Expected: working tree clean, branch ahead of `main` by ~15 commits.

The implementation is complete when all six prior steps pass and the working tree is clean.

---

## Out-of-band

Once the local CI gate is green:

1. **Open the PR.** Title format: `feat(macros): SMA-315 add #[tool] proc-macro and tools! companion macro`. The `pr-title.yml` workflow gates on sentence-case after the SMA prefix (lowercase verb), so this title is compliant. Body includes a one-paragraph summary, a checklist confirming the AC #2 manual review was performed, and links to the spec.
2. **CI must pass `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`.** The MSRV matrix entry (`test (..., 1.75)`) is a non-required signal; the `rustversion`-gated trybuild test means it won't flag red.
3. **Linear auto-closes SMA-315** when the PR merges; no manual transition.

End of plan.
