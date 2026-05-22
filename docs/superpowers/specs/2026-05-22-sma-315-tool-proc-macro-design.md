# SMA-315 — `#[tool]` proc-macro with schemars-derived JSON Schema — design

- **Linear:** [SMA-315](https://linear.app/smaschek/issue/SMA-315/tool-proc-macro-with-schemars-derived-json-schema)
- **Branch:** `feature/sma-315-tool-proc-macro-with-schemars-derived-json-schema`
- **Status:** design (awaiting implementation plan)
- **Author:** Sven Maschek
- **Date:** 2026-05-22

## 1. Goal

Ship the ergonomic path for in-process Rust tools. An `async fn` annotated with `#[tool]` (plus a `#[derive(Deserialize, JsonSchema)]` args struct) expands into a fully-formed `impl Tool<Ctx>` — `name`, `description`, `schema`, `output_schema`, `invoke` — that downstream agent code can hold as `Arc<dyn Tool<Ctx>>`. A companion `tools![ … ]` macro boxes a heterogeneous set into `Vec<Arc<dyn Tool<Ctx>>>`.

The Linear ticket's acceptance criteria are:

1. A two-arg tool with doc comments produces a JSON Schema matching a golden file.
2. `cargo expand` output is readable (no surprising lifetimes).
3. A bad args struct (e.g. non-`Deserialize`) fails to compile with a clear diagnostic via `compile_fail` tests in `trybuild`.

AC #1 is locked by `tests/schema_golden.rs` with `insta` snapshots (§6). AC #3 is locked by a set of UI tests under `tests/ui/` driven by `trybuild` (§6). AC #2 is a manual-review checklist on the PR — a representative expansion is captured in §4.2 of this spec as the reference point.

### 1.1 Scope boundary (against peer tickets)

The trait surface this ticket implements against — `Tool<Ctx>`, `ToolContext<Ctx>`, `ToolOutput`, `ToolError` — was landed by SMA-312 and SMA-313 and is **not modified** by SMA-315. `paigasus-helikon-tools` (first-party tool crates) stays a stub; HTTP/FS/exec tools land in later tickets and will consume `#[tool]` as their primary authoring path.

A second proc-macro (`#[agent]`, planned in a downstream ticket) will share infrastructure with `#[tool]` — attribute parsing patterns, doc-comment extraction, crate-path resolution. SMA-315 builds those primitives without pre-abstracting them; refactoring lands when the second consumer arrives.

## 2. Decisions and rationale

Ten decisions, scoped to the SMA-315 surface.

| Decision | Choice | Rationale |
|---|---|---|
| Function signature shape | **`async fn foo(ctx: &ToolContext<Ctx>, args: Args) -> Result<Out, E>` where `E: Into<ToolError>`.** The `ToolContext` is mandatory and positionally first; the user's `E` is preserved through the helper fn and `?` does the `From` conversion. | Mirrors `Tool::invoke` 1:1 on the call side, but does **not** force users into `ToolError` everywhere. `ToolError::Other(#[from] anyhow::Error)` already exists in `paigasus-helikon-core`, so `Result<_, anyhow::Error>` "just works" via `?` — which is the exact ergonomic tax the macro should eliminate. Matches the Notion *Tools* page reference example. |
| Generated artifact | **Replace the fn with a unit struct of the same ident** plus `impl Tool<Ctx>` for it. The original fn body moves into a sibling **module** `__helikon_tool_<ident>` to scope the helper fn safely. | Bare-ident call site (`tools![add, mul]`) — no `()` constructor, no `Tool`-suffix naming convention. The module wrapper namespaces the helper fn so a hand-written `__helikon_orig_add` in the user's crate cannot collide. |
| Generated struct visibility | **Preserve the user's `vis` token on the fn.** A `pub(crate) async fn foo` produces `pub(crate) struct foo;`; a private fn produces a private struct. | Silently widening visibility (e.g. by hardcoding `pub`) would leak internal tools into the user's public API. Preservation is the only safe default. |
| Description source | **`#[tool(description = "lit")]` (non-empty) wins; fall back to the first paragraph of `///` doc comments; compile error if neither.** Both `///` and `#[doc = "…"]` syntactic forms are accepted (they parse to the same AST node). | Description is required by the LLM contract (the trait returns `&str`). Attr-wins matches typical Rust semantics. *First paragraph* (split on `\n\n`) keeps the model-facing description focused and avoids dumping entire rustdoc bodies (rationale paragraphs, examples, code fences) into prompts. Users who want the full rustdoc as description override with the explicit attr. |
| Output schema generation | **Auto-emit when `Out: JsonSchema`; else fall back to `None`.** Implemented via the autoref-specialization trick (§4.3). | Zero-config when the user already derives `JsonSchema` on the output type; no hard `JsonSchema` bound on users returning `serde_json::Value` or other non-schemars types. Autoref-specialization is well-trodden on stable Rust. |
| Crate-path resolution | **Auto-resolve via `proc-macro-crate`.** The macro probes the consumer's `Cargo.toml` for `paigasus-helikon-core` first (preferred) and falls back to `paigasus-helikon` (facade — expands paths as `::paigasus_helikon::core::…`). `#[tool(crate = ::path)]` exists as an escape hatch for renamed deps. | Solves the "facade-only consumer compile-fails on `::paigasus_helikon_core::Tool`" trap *automatically* rather than asking the user to remember a `crate = …` knob. Same pattern as `serde`/`thiserror`. |
| `tools![ … ]` shape | **Function-like proc-macro `tools!(...)` in `paigasus-helikon-macros`.** Not a `macro_rules!` macro — that form cannot resolve crate paths through the facade re-export (`$crate` always points to the *defining* crate). | Unifies path resolution with `#[tool]` (same `proc-macro-crate` probe). Facade-only consumers `paigasus_helikon::tools![a, b]` work without ceremony. The `macros` feature on the facade gates both macros together. Manual `vec![Arc::new(t) as Arc<dyn Tool<_>>, …]` is the no-feature fallback for users who don't pull `macros`. |
| Attribute parsing | **Hand-rolled `syn`/`quote`, no `darling`.** | The attribute surface is three keys (`description`, `name`, `crate`). Well-spanned `syn::Error`s are at parity with `darling`'s diagnostics, and a 5-crate parsing framework is the wrong default at this scale. Revisit if `#[agent]` later adds 8+ keys. |
| Schema cache | **`OnceLock<serde_json::Value>` per tool, lazy on first `schema()` call.** | `Tool::schema()` returns `&Value`, so per-call computation is impossible. `OnceLock` (stable since 1.70, comfortably below our 1.75 MSRV) pays the schemars cost once on first registration. `LazyLock` would be cleaner but stabilized in 1.80 — revisit at the next MSRV bump. |
| Attribute forwarding | **Forward every non-`#[tool(...)]`, non-doc attribute on the user fn to the helper `run` fn unchanged.** `#[cfg(...)]` is evaluated by rustc before `#[tool]` fires (no action needed); `#[tool(...)]` and `#[doc = "..."]`/`///` are consumed; everything else (`#[tracing::instrument]`, `#[allow(...)]`, `#[deprecated]`, user attribute macros) lands on the helper. | The helper is the only thing the user's body lives inside; `#[tracing::instrument]` on the unit struct or the impl block would trace nothing meaningful. The rule works regardless of attribute order — if another proc-macro fires before `#[tool]`, it has already rewritten the body and `#[tool]` simply moves the result; if it fires after `#[tool]`, the forwarded attribute on the helper does the right thing. |

## 3. Files added / modified

### Added

| Path | Purpose |
|---|---|
| `crates/paigasus-helikon-macros/src/lib.rs` | `#[proc_macro_attribute] tool` + `#[proc_macro] tools` entry points; module imports. |
| `crates/paigasus-helikon-macros/src/attr.rs` | Parse `#[tool(description = …, name = …, crate = …)]` and the optional `crate = …` prefix on `tools!(…)`. |
| `crates/paigasus-helikon-macros/src/signature.rs` | Parse and validate the target `async fn` signature; extract `Args`, `Out`, `Ctx`; check `ToolContext` path-match. |
| `crates/paigasus-helikon-macros/src/resolve.rs` | `proc-macro-crate` wrapper — resolves the consumer's path to `paigasus-helikon-core` (direct) or `paigasus-helikon` (facade), or honors the `crate = ::path` override. |
| `crates/paigasus-helikon-macros/src/expand.rs` | Codegen — unit struct (with preserved vis), `impl Tool<Ctx>`, helper module containing the moved body, `OnceLock` statics. |
| `crates/paigasus-helikon-macros/tests/schema_golden.rs` | Snapshot test for the two-arg-tool schema (AC #1). Uses `insta::assert_snapshot!`. |
| `crates/paigasus-helikon-macros/tests/snapshots/schema_golden__add_schema.snap` | Golden file for the schema snapshot. |
| `crates/paigasus-helikon-macros/tests/trybuild.rs` | Entry point for the `trybuild` UI suite (compile-fail + compile-pass). |
| `crates/paigasus-helikon-macros/tests/ui/bad_args_not_deserialize.rs` (+ `.stderr`) | Compile-fail: args struct missing `Deserialize`. |
| `crates/paigasus-helikon-macros/tests/ui/bad_args_not_jsonschema.rs` (+ `.stderr`) | Compile-fail: args struct missing `JsonSchema`. |
| `crates/paigasus-helikon-macros/tests/ui/bad_out_not_serialize.rs` (+ `.stderr`) | Compile-fail: output type missing `Serialize`. |
| `crates/paigasus-helikon-macros/tests/ui/no_description.rs` (+ `.stderr`) | Compile-fail: no attr description and no doc comment. |
| `crates/paigasus-helikon-macros/tests/ui/empty_description.rs` (+ `.stderr`) | Compile-fail: `#[tool(description = "")]`. |
| `crates/paigasus-helikon-macros/tests/ui/bad_signature_wrong_ctx.rs` (+ `.stderr`) | Compile-fail: first arg isn't `&ToolContext<_>`. |
| `crates/paigasus-helikon-macros/tests/ui/bad_signature_not_async.rs` (+ `.stderr`) | Compile-fail: non-async fn. |
| `crates/paigasus-helikon-macros/tests/ui/bad_signature_unsafe.rs` (+ `.stderr`) | Compile-fail: `unsafe async fn`. |
| `crates/paigasus-helikon-macros/tests/ui/bad_signature_const.rs` (+ `.stderr`) | Compile-fail: `const fn`. |
| `crates/paigasus-helikon-macros/tests/ui/bad_signature_generic.rs` (+ `.stderr`) | Compile-fail: generic free fn (`async fn foo<T>(…)`). |
| `crates/paigasus-helikon-macros/tests/ui/bad_name.rs` (+ `.stderr`) | Compile-fail: `#[tool(name = "has spaces")]`. |
| `crates/paigasus-helikon-macros/tests/ui/facade_only_consumer.rs` (+ `.stderr` empty / compile-pass) | **Compile-pass** case where the test crate depends *only* on `paigasus-helikon` (no direct core dep). Locks the `proc-macro-crate` resolution path. |
| `crates/paigasus-helikon-macros/tests/end_to_end.rs` | Behavioral test — invoke a generated tool, assert `name`/`description`/`schema`/`invoke` semantics and `tools!(…)` macro output shape. |
| `crates/paigasus-helikon-core/src/__private.rs` | **Semver-exempt** `#[doc(hidden)] pub mod __private` — hosts `OutputSchemaProbe`/`OutputSchemaProbeSpec` and a `pub use async_trait` re-export for use by macro-generated code. Module docstring states the semver exemption. |

### Modified

| Path | Change |
|---|---|
| `crates/paigasus-helikon-macros/Cargo.toml` | Deps: `proc-macro2`, `quote`, `syn`, `proc-macro-crate`. Dev-deps: `paigasus-helikon-core` (path), `paigasus-helikon` (path, `features = ["macros"]` — for the facade-only-consumer trybuild case), `async-trait`, `schemars`, `serde`, `serde_json`, `tokio` (features `macros`, `rt`), `trybuild`, `insta`. |
| `crates/paigasus-helikon-core/Cargo.toml` | No dependency change. `async-trait` and `schemars` are already direct deps; the new `__private` module re-exports them via `pub use`. |
| `crates/paigasus-helikon-core/src/lib.rs` | Add `#[doc(hidden)] pub mod __private;` (the module file is added; the re-export makes it reachable as `paigasus_helikon_core::__private`). |
| `crates/paigasus-helikon/Cargo.toml` | No structural change. `paigasus-helikon-macros` stays optional behind the existing `macros` feature, per the facade convention. |
| `crates/paigasus-helikon/src/lib.rs` | Add `#[cfg(feature = "macros")] pub use paigasus_helikon_macros::{tool, tools};`. (Core is already re-exported as `paigasus_helikon::core`, which is what the macro's `proc-macro-crate` resolution uses for facade-only consumers.) |
| `Cargo.toml` (workspace) | Add `proc-macro2 = "1"`, `quote = "1"`, `syn = { version = "2", features = ["full"] }`, `proc-macro-crate = "3"`, `trybuild = "1"` to `[workspace.dependencies]`. (`schemars`, `serde`, `serde_json`, `tokio`, `async-trait`, `insta` already declared.) |

## 4. Generated code shape

### 4.1 User-facing example

(Facade-only consumers replace `paigasus_helikon_core` with `paigasus_helikon::core` in the imports below; the `proc-macro-crate` resolution in §5.3 makes the generated code follow the same substitution.)

```rust
use paigasus_helikon::{tool, tools};
use paigasus_helikon_core::{Tool, ToolContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
///
/// (Subsequent paragraphs are NOT sent to the model — see §5.)
#[tool]
async fn add(
    _ctx: &ToolContext<MyCtx>,
    args: AddArgs,
) -> Result<AddOut, anyhow::Error> {        // user picks any E: Into<ToolError>
    Ok(AddOut { sum: args.a + args.b })
}

let registry: Vec<Arc<dyn Tool<MyCtx>>> = tools![add /* , mul, … */];
```

### 4.2 Reference expansion (for AC #2 "readable" criterion)

Expansion of the `#[tool]` block above (paths shown for a direct-core consumer; facade-only consumers see `::paigasus_helikon::core::…` everywhere):

```rust
#[allow(non_camel_case_types)]
struct add;     // vis preserved from `async fn add` (private here)

mod __helikon_tool_add {
    // Pulls in the user's args/output types and any third-party imports
    // they wrote at module scope (AddArgs, AddOut, anyhow, etc.).
    use super::*;

    pub(super) static INPUT_SCHEMA: ::std::sync::OnceLock<::serde_json::Value> =
        ::std::sync::OnceLock::new();
    pub(super) static OUTPUT_SCHEMA: ::std::sync::OnceLock<Option<::serde_json::Value>> =
        ::std::sync::OnceLock::new();

    // Signature is forwarded *verbatim* from the user's source — no
    // path rewriting. `&ToolContext<MyCtx>` and `anyhow::Error` resolve
    // via the `use super::*;` glob above.
    pub(super) async fn run(
        _ctx: &ToolContext<MyCtx>,
        args: AddArgs,
    ) -> Result<AddOut, anyhow::Error> {
        // user's body, verbatim
        Ok(AddOut { sum: args.a + args.b })
    }
}

#[::paigasus_helikon_core::__private::async_trait::async_trait]
impl ::paigasus_helikon_core::Tool<MyCtx> for add {
    fn name(&self) -> &str { "add" }
    fn description(&self) -> &str { "Adds two numbers." }

    fn schema(&self) -> &::serde_json::Value {
        __helikon_tool_add::INPUT_SCHEMA.get_or_init(|| {
            ::serde_json::to_value(::schemars::schema_for!(AddArgs))
                .expect("schemars schema must serialize")
        })
    }

    fn output_schema(&self) -> ::std::option::Option<&::serde_json::Value> {
        __helikon_tool_add::OUTPUT_SCHEMA
            .get_or_init(|| {
                // Autoref-specialization — see §4.3.
                (&&::paigasus_helikon_core::__private::OutputSchemaProbe::<AddOut>::NEW)
                    .schema()
            })
            .as_ref()
    }

    async fn invoke(
        &self,
        ctx: &::paigasus_helikon_core::ToolContext<MyCtx>,
        args: ::serde_json::Value,
    ) -> ::std::result::Result<
        ::paigasus_helikon_core::ToolOutput,
        ::paigasus_helikon_core::ToolError,
    > {
        let parsed: AddArgs = ::serde_json::from_value(args)
            .map_err(|e| ::paigasus_helikon_core::ToolError::InvalidArgs {
                schema_errors: ::std::vec![e.to_string()],
            })?;
        // `?` converts the user's `E` via `ToolError: From<E>` (e.g. `From<anyhow::Error>`).
        let out = __helikon_tool_add::run(ctx, parsed).await?;
        let content = ::serde_json::to_value(&out)
            .map_err(|e| ::paigasus_helikon_core::ToolError::Other(e.into()))?;
        ::std::result::Result::Ok(
            ::paigasus_helikon_core::ToolOutput::new(content)
        )
    }
}
```

**Note on the helper's signature.** The macro emits the helper `run` fn's signature by cloning the user's `syn::Signature` token-tree verbatim. Absolute-path normalization is reserved for the macro-generated items (struct, `impl Tool<Ctx>`, `__private` references) where the absolute paths do load-bearing work. The helper's signature resolves through the `use super::*;` glob, which means whatever the user wrote at the call site — `&ToolContext<Ctx>`, `&core::ToolContext<Ctx>`, `Result<_, anyhow::Error>`, `Result<_, MyError>` — works without the macro caring how it was named.

**Trade-off on `use super::*;`.** A wildcard glob also pulls in every other `#[tool]`-generated symbol in the same parent module, so rustc's diagnostics inside the body can occasionally surface "did you mean `other_tool`?" suggestions. The alternative — emitting a narrow `use` list — requires re-parsing the body to find identifiers, which would defeat the verbatim-body invariant. The diagnostic noise is acceptable; the verbatim guarantee is not negotiable.

### 4.3 Autoref-specialization for `output_schema`

The proc-macro cannot ask "does `Out: JsonSchema`?" at expansion time — proc-macros operate on syntax, not types. The autoref-specialization trick gives us trait-aware codegen on stable Rust by arranging two `schema(&self)` candidates at different deref levels and letting method resolution pick the closer one:

```rust
// In paigasus_helikon_core::__private (semver-exempt, only macro-generated code touches it):
pub struct OutputSchemaProbe<T>(::std::marker::PhantomData<T>);

impl<T> OutputSchemaProbe<T> {
    pub const NEW: Self = Self(::std::marker::PhantomData);
}

// Specialized arm — trait impl on `&OutputSchemaProbe<T>` (one deref).
pub trait OutputSchemaProbeSpec {
    fn schema(&self) -> Option<::serde_json::Value>;
}
impl<T: ::schemars::JsonSchema> OutputSchemaProbeSpec
    for &OutputSchemaProbe<T>
{
    fn schema(&self) -> Option<::serde_json::Value> {
        ::serde_json::to_value(::schemars::schema_for!(T)).ok()
    }
}

// Fallback arm — inherent method on `OutputSchemaProbe<T>` (two deref steps via autoref).
impl<T> OutputSchemaProbe<T> {
    pub fn schema(&self) -> Option<::serde_json::Value> { None }
}
```

When the macro emits `(&&OutputSchemaProbe::<Out>::NEW).schema()`, method resolution starts at `&&Probe<Out>`, auto-derefs once to `&Probe<Out>`, and finds the `OutputSchemaProbeSpec::schema` impl **iff** `Out: JsonSchema`. If the bound holds, that impl wins — it's fewer deref steps away. If it doesn't, resolution falls through to `Probe<Out>` and finds the inherent `fn schema(&self) -> None` — the fallback. No nightly features, no `JsonSchema` bound leaked onto the user's `Out`.

**This probe lives in `paigasus-helikon-core`, not `paigasus-helikon-macros`.** Proc-macro crates (`[lib] proc-macro = true`) cannot publicly export anything other than `#[proc_macro*]` items, so the support module must sit in a regular lib. This mirrors the `serde_derive` ↔ `serde::__private` pattern.

### 4.4 `tools!(…)` proc-macro

`tools!` is a function-like proc-macro (`#[proc_macro] pub fn tools(...)`). It accepts a comma-separated list of tool expressions, optionally prefixed with a `crate = ::path;` override. The bracket form `tools![…]` is the canonical invocation syntax (matches Rust's collection-macro convention); paren form is equivalent but not used in docs.

```rust
// Common case (auto-resolved path):
let r: Vec<Arc<dyn Tool<MyCtx>>> = tools![add, mul];

// Explicit override (renamed dep / unusual setup):
let r: Vec<Arc<dyn Tool<MyCtx>>> = tools![crate = ::my_renamed_helikon; add, mul];
```

**Argument contract:** each comma-separated argument must be a value of a type that implements `Tool<Ctx>` directly. Do **not** pre-wrap with `Arc` — `tools![Arc::new(t)]` would generate `Arc::new(Arc::new(t)) as Arc<dyn Tool<_>>`, which fails the cast because `Arc<T>` does not itself implement `Tool<Ctx>`. The resulting rustc diagnostic is recoverable but ugly; the macro's rustdoc spells this rule out.

Expansion (direct-core consumer):

```rust
{
    let __r: ::std::vec::Vec<
        ::std::sync::Arc<dyn ::paigasus_helikon_core::Tool<_>>
    > = ::std::vec![
        ::std::sync::Arc::new(add)
            as ::std::sync::Arc<dyn ::paigasus_helikon_core::Tool<_>>,
        ::std::sync::Arc::new(mul)
            as ::std::sync::Arc<dyn ::paigasus_helikon_core::Tool<_>>,
    ];
    __r
}
```

**No empty arm.** `tools![]` is rejected by the parser with a diagnostic pointing at the macro invocation: ``tools! expects at least one tool; use `Vec::<Arc<dyn Tool<Ctx>>>::new()` for an empty registry``. The empty case is rare and an explicit `Vec::new` is more honest than a turbofish-driven macro arm that compiles only when an LHS annotation pins `Ctx`. The trailing comma is allowed.

The `Ctx` parameter is inferred at the call site from the LHS type annotation or from how the registry is consumed downstream. Mismatched `Ctx` across tools surfaces as a stock rustc trait-bound error. The canonical error text is reproduced in the `tools!` rustdoc so users can grep for it (see §10).

## 5. Description, name, and crate resolution — exact rules

### 5.1 Description

| Input | Behavior |
|---|---|
| `#[tool(description = "non-empty literal")]` | Use the literal verbatim. Wins over any doc comments. |
| `#[tool(description = "")]` | **Compile error** — empty descriptions are useless to the model. Diagnostic: ``empty `description`; provide a non-empty literal or remove the attr to fall back to doc comments``. |
| `///` doc comments or `#[doc = "…"]` attrs (semantically identical AST node) | Concatenate lines, strip the leading space, join with `\n`, trim trailing whitespace. **Take the first paragraph only** — everything up to the first `\n\n`. Use as description. |
| Neither attr nor doc | **Compile error**: ``tool `<name>` requires a description: add `#[tool(description = "…")]` or a `///` doc comment``. |

Argument-field descriptions are **not** handled by the `#[tool]` macro. They flow through `#[derive(JsonSchema)]`, which already extracts `///` doc comments on struct fields. This is the explicit boundary — `JsonSchema` owns the args struct, `#[tool]` owns the fn glue.

### 5.2 Tool name

| Input | Behavior |
|---|---|
| `#[tool(name = "lit")]` | Use the literal. Validated against `^[A-Za-z_][A-Za-z0-9_-]*$`. |
| Otherwise | Use the fn ident as a string. **Raw idents** (`r#async`) — strip the `r#` prefix; document this rule in the macro rustdoc. The validation regex still applies to the stripped form. |

If the validated name fails the regex, emit ``tool name must match `[A-Za-z_][A-Za-z0-9_-]*`; got "<actual>"`` spanned at the literal (or the fn ident).

### 5.3 Crate path

Resolution is automatic, with a manual override:

```text
resolve_crate_path() {
    if attr `crate = ::path` is present:
        return that path
    via proc-macro-crate, look up `paigasus-helikon-core` in the consumer Cargo.toml:
        if FoundCrate::Itself          → ::paigasus_helikon_core
        if FoundCrate::Name(n)         → ::<n>
        if NotFound, look up `paigasus-helikon`:
            if FoundCrate::Itself       → ::paigasus_helikon::core
            if FoundCrate::Name(n)      → ::<n>::core
            if NotFound                 → compile_error!
                "#[tool] requires either `paigasus-helikon-core` or
                 `paigasus-helikon` (features=[\"macros\"]) as a
                 direct dependency; or set `#[tool(crate = ::path)]`."
}
```

The resolved path stem is used for `<stem>::Tool`, `<stem>::ToolContext`, `<stem>::ToolError`, `<stem>::ToolOutput`, `<stem>::__private::OutputSchemaProbe`, and `<stem>::__private::async_trait`. `serde_json` and `schemars` and the user's `anyhow` (when used as `E`) are referenced by `::serde_json`, `::schemars`, `::anyhow` — they are guaranteed direct deps of the user's crate because the user's source already names them (`#[derive(Deserialize, JsonSchema)]`; user-typed return type).

### 5.4 `ToolContext` match rule

The macro determines `Ctx` by inspecting the first fn argument's type, which must take one of these shapes:

- `&ToolContext<T>`
- `&<path::segments>::ToolContext<T>` where the trailing path segment is `ToolContext`

The macro operates on **syntax**, not types — it cannot tell a type alias apart from a struct it doesn't recognize. The diagnostic is therefore phrased about what the macro looked for, not about what the user did: ``#[tool] expects the first argument to be `&…::ToolContext<Ctx>` (matched on the trailing path segment `ToolContext` with one type argument); aliases and renames are not unwrapped — name the type directly``. The macro accepts any number of leading path segments (`&core::ToolContext<…>`, `&paigasus_helikon::core::ToolContext<…>`), as long as the trailing segment is `ToolContext` with one type argument.

## 6. Testing strategy

Three layers, all under `crates/paigasus-helikon-macros/tests/`.

### 6.1 Schema golden file (AC #1)

`tests/schema_golden.rs`:

- Defines `AddArgs` and `AddOut` exactly as in §4.1.
- Constructs `add` (the macro-generated unit struct), calls `.schema()`, serializes via `serde_json::to_string_pretty`.
- `insta::assert_snapshot!(serialized)`.

First-run writes `tests/snapshots/schema_golden__add_schema.snap`. Reviewer eyeballs the snapshot during PR — it's the "schema matching golden file" artifact the AC names. Subsequent runs diff; `cargo insta review` updates intentionally. Snapshot covers type mapping (`i64` → `integer`), `required` array population, per-field `description` from doc comments.

### 6.2 `trybuild` UI tests (AC #3 + path-resolution coverage)

`tests/trybuild.rs`:

```rust
#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/bad_*.rs");
    t.compile_fail("tests/ui/no_description.rs");
    t.compile_fail("tests/ui/empty_description.rs");
    t.pass("tests/ui/facade_only_consumer.rs");
}
```

| Case | Verifies |
|---|---|
| `bad_args_not_deserialize.rs` | Args struct lacks `Deserialize` → rustc trait-bound error from generated `from_value` call. |
| `bad_args_not_jsonschema.rs` | Args struct lacks `JsonSchema` → rustc trait-bound error from generated `schema_for!` call. |
| `bad_out_not_serialize.rs` | Output type lacks `Serialize` → rustc trait-bound error from generated `to_value(&out)` call. |
| `no_description.rs` | No attr description and no doc comment → macro `compile_error!`. |
| `empty_description.rs` | `#[tool(description = "")]` → macro `compile_error!`. |
| `bad_signature_wrong_ctx.rs` | First arg is `&MyCtx` instead of `&ToolContext<MyCtx>` → macro diagnostic spanned at the first arg. |
| `bad_signature_not_async.rs` | `fn` instead of `async fn` → macro diagnostic. |
| `bad_signature_unsafe.rs` | `unsafe async fn` → macro diagnostic spanned at the `unsafe` keyword. |
| `bad_signature_const.rs` | `const fn` → macro diagnostic spanned at the `const` keyword. |
| `bad_signature_generic.rs` | `async fn foo<T: JsonSchema>(…)` → macro diagnostic, *before* the autoref probe expands across the user's screen. |
| `bad_name.rs` | `#[tool(name = "has spaces")]` → macro diagnostic spanned at the name literal. |
| `facade_only_consumer.rs` | **Compile-pass.** Test crate depends only on `paigasus-helikon = { features = ["macros"] }` — no direct `paigasus-helikon-core` dep. Locks the `proc-macro-crate` resolution of `paigasus-helikon` → `::paigasus_helikon::core::…`. |

Stderr files are regenerated with `TRYBUILD=overwrite cargo test --test trybuild` when a diagnostic intentionally changes. Reviewers diff the `.stderr` files alongside the source.

**Note on `facade_only_consumer.rs`:** `trybuild` generates a transient `Cargo.toml` for each UI test that inherits the host crate's dev-dependencies. So a UI test file can `use schemars::JsonSchema;` and `use serde::{Deserialize, Serialize};` despite its synthesized manifest listing only `paigasus-helikon = { features = ["macros"] }` — the dev-deps from `paigasus-helikon-macros` carry over. This is correct behavior and is the only way the facade-only compile-pass case can also use the derive crates the user's code references; it's worth noting because a reviewer who expects strict crate isolation per UI test will be surprised.

### 6.3 End-to-end behavioral test

`tests/end_to_end.rs` — a single `#[tokio::test]`:

1. Construct the registry via `tools![add]`.
2. Assert `registry.len() == 1`, `registry[0].name() == "add"`, `registry[0].description() == "Adds two numbers."`. Also define a sibling tool with a long multi-paragraph `///` doc comment **and** an explicit `#[tool(description = "Short.")]`; assert `description() == "Short."` (locks attr-wins-over-doc-comments precedence).
3. Build a minimal `ToolContext<MyCtx>` via `ToolContext::new`.
4. Invoke with valid JSON `{"a": 2, "b": 3}` → assert `Ok(ToolOutput { content: json!({"sum": 5}) })`.
5. Invoke with invalid JSON `{"a": "not a number", "b": 3}` → assert `Err(ToolError::InvalidArgs { schema_errors })` with non-empty `schema_errors`.
6. Call `schema()` twice; assert pointer equality on the returned `&Value` (proves `OnceLock` caching).
7. Call `output_schema()`; assert `Some(_)` (because `AddOut: JsonSchema`).
8. Define a second tool `OpaqueTool` whose `Out` is a non-`JsonSchema` newtype; assert its `output_schema()` returns `None`.
9. Define a third tool returning `Result<…, anyhow::Error>`; assert it `impl`s `Tool<MyCtx>` and a body-side `Err(anyhow!("boom"))` surfaces as `ToolError::Other` at the runner level.
10. Define a fourth tool with `#[allow(non_snake_case)]` and `#[deprecated(note = "use new_add")]` on the user fn (alongside `#[tool]`), and rename the fn to `legacyAdd`. Assert the tool compiles and `invoke` works end-to-end — the `non_snake_case` lint must be silenced by the forwarded `#[allow]`, proving the attribute reached the helper `run` fn (where the body uses the renamed identifier). Locks the attribute-forwarding rule from §2.

### 6.4 The "readable `cargo expand`" criterion (AC #2)

Not automatable. Captured as:

- The reference expansion in §4.2, kept current when the macro changes.
- PR-description checklist: "Ran `cargo expand --test end_to_end` and verified expansion matches §4.2 within whitespace/comment differences."

## 7. Error handling & diagnostics

### 7.1 Compile-time (proc-macro side)

All parse failures use `syn::Error::new_spanned(tok, msg).to_compile_error()` so the error span lands on the offending token.

| Condition | Span | Message |
|---|---|---|
| Missing description | fn ident | ``tool `<name>` requires a description: add `#[tool(description = "…")]` or a `///` doc comment`` |
| Empty description literal | description literal | ``empty `description`; provide a non-empty literal or remove the attr to fall back to doc comments`` |
| Non-async fn | `fn` keyword | ``#[tool] requires an `async fn` `` |
| `unsafe async fn` | `unsafe` keyword | ``#[tool] cannot wrap an `unsafe fn`; `Tool::invoke` is safe — drop the `unsafe` qualifier or inline the unsafe block inside the body`` |
| `const fn` | `const` keyword | ``#[tool] cannot wrap a `const fn`; `Tool::invoke` is not const`` |
| `extern "<abi>" fn` | `extern` keyword | ``#[tool] cannot wrap a fn with an `extern` ABI; remove the ABI specifier`` |
| Generic free fn | `fn` keyword (or first generic param) | ``#[tool] does not support generic free fns; instantiate the generic and apply #[tool] to the concrete fn`` |
| Wrong arity | fn signature | ``#[tool] expects two args: `&ToolContext<Ctx>` and an args struct`` |
| First arg not `&ToolContext<…>` (path-segment match) | first arg's pattern | ``#[tool] expects the first argument to be `&…::ToolContext<Ctx>` (matched on the trailing path segment `ToolContext` with one type argument); aliases and renames are not unwrapped — name the type directly`` |
| `Self` or trait-method form | fn keyword | ``#[tool] applies to free `async fn` only`` |
| Bad `name = …` | name literal | ``tool name must match `[A-Za-z_][A-Za-z0-9_-]*`; got "<actual>" `` |
| Unknown attribute key | key token | ``unknown #[tool] attribute `<key>`; expected one of `description`, `name`, `crate` `` |
| Neither `paigasus-helikon-core` nor `paigasus-helikon` dep | macro span | ``#[tool] requires either `paigasus-helikon-core` or `paigasus-helikon` (features=["macros"]) as a direct dependency; or set `#[tool(crate = ::path)]` `` |

Trait-bound failures on `Args` (missing `Deserialize`/`JsonSchema`), `Out` (missing `Serialize`), and the user's `E` (missing `Into<ToolError>`) are caught downstream by rustc when the expanded code references those traits. The `trybuild` UI tests pin the resulting rustc diagnostics.

### 7.2 Runtime (generated code)

| Failure | Outcome |
|---|---|
| `serde_json::from_value::<Args>(args)` fails | `Err(ToolError::InvalidArgs { schema_errors: vec![e.to_string()] })` — recoverable per ADR-10. |
| User body returns `Err(E)` where `E: Into<ToolError>` | `?` invokes `ToolError::from(E)` automatically. For `E = anyhow::Error`, the existing `#[from] anyhow::Error` impl on `ToolError::Other` carries it through. For `E = ToolError`, identity conversion. |
| `serde_json::to_value(&out)` fails | `Err(ToolError::Other(e.into()))` — non-recoverable. Output serialization failures are programmer errors (non-string map keys, etc.). |

`schemars::schema_for!` cannot fail at runtime — schemars generates the schema infallibly given the type compiles. The `OnceLock` initializer's `.expect("schemars schema must serialize")` covers the `to_value` step; failure there is a `serde_json` bug and panicking on first use is correct.

## 8. Cargo wiring

### 8.1 Workspace `[workspace.dependencies]` additions

The workspace already declares `schemars = "1"`, `serde`, `serde_json`, `tokio`, `async-trait`, `insta = "1"`. New pins for SMA-315:

```toml
proc-macro2 = "1"
quote = "1"
syn = { version = "2", features = ["full"] }
proc-macro-crate = "3"
trybuild = "1"
```

`syn` uses `features = ["full"]` only — `extra-traits` (Debug/Eq/Hash across the whole AST) measurably slows macro-crate compile times and is a debugging convenience we don't need for codegen. `proc-macro-crate` 3.x is the current major as of 2026-05.

Schemars 1.x is the current stable line; the schema layout it emits is what the golden file pins. See §11 (open questions) — bumping to 2.x is not a chore.

### 8.2 `crates/paigasus-helikon-macros/Cargo.toml`

```toml
[dependencies]
proc-macro2.workspace = true
quote.workspace = true
syn.workspace = true
proc-macro-crate.workspace = true

[dev-dependencies]
paigasus-helikon-core = { path = "../paigasus-helikon-core" }
paigasus-helikon = { path = "../paigasus-helikon", features = ["macros"] }   # for the facade-only trybuild case
async-trait.workspace = true
schemars.workspace = true
serde = { workspace = true, features = ["derive"] }
serde_json.workspace = true
tokio = { workspace = true, features = ["macros", "rt"] }
anyhow.workspace = true   # for end_to_end.rs and bad_args trybuild cases
trybuild.workspace = true
insta.workspace = true
```

### 8.3 `crates/paigasus-helikon-core/Cargo.toml` and source

No `Cargo.toml` change — `async-trait` and `schemars` are already direct deps. Source changes:

- New file `crates/paigasus-helikon-core/src/__private.rs`:

  ```rust
  //! Implementation details exposed to macro-generated code.
  //!
  //! **Semver-exempt.** Items in this module are not part of the public
  //! API. Only the `#[tool]` and `tools!` macros in
  //! `paigasus-helikon-macros` are expected to reference them. Direct
  //! use by application code is unsupported and may break without notice.

  use std::marker::PhantomData;

  pub use async_trait;          // re-export so generated code can name it absolutely

  pub struct OutputSchemaProbe<T>(PhantomData<T>);
  impl<T> OutputSchemaProbe<T> {
      pub const NEW: Self = Self(PhantomData);
  }
  pub trait OutputSchemaProbeSpec {
      fn schema(&self) -> Option<serde_json::Value>;
  }
  impl<T: schemars::JsonSchema> OutputSchemaProbeSpec for &OutputSchemaProbe<T> {
      fn schema(&self) -> Option<serde_json::Value> {
          serde_json::to_value(schemars::schema_for!(T)).ok()
      }
  }
  impl<T> OutputSchemaProbe<T> {
      pub fn schema(&self) -> Option<serde_json::Value> { None }
  }
  ```

- In `crates/paigasus-helikon-core/src/lib.rs`: `#[doc(hidden)] pub mod __private;`.

### 8.4 `crates/paigasus-helikon/Cargo.toml` (facade)

No structural change. Existing wiring stays:

```toml
[dependencies]
paigasus-helikon-macros = { workspace = true, optional = true }

[features]
macros = ["dep:paigasus-helikon-macros"]
```

In `src/lib.rs`:

```rust
#[cfg(feature = "macros")]
pub use paigasus_helikon_macros::{tool, tools};
```

`paigasus_helikon::core` is already re-exported unconditionally — that's the path `proc-macro-crate` resolution targets for facade-only consumers.

## 9. Non-goals (out of scope for SMA-315)

- **No first-party tool implementations.** `paigasus-helikon-tools` stays a stub.
- **No streaming tool outputs.** `ToolOutput` is single-shot.
- **No multi-modal content.** `ToolOutput.content` stays `serde_json::Value`. The `#[non_exhaustive]` preserves room for later.
- **No `JsonSchema`-driven pre-deserialize validation.** We rely on `serde`. Schemars-level validation as a pre-deserialize step is a follow-up if real "schema accepts but serde rejects" cases emerge.
- **No `Ctx` inference fallback in `tools!`.** Mismatched `Ctx` across tools surfaces as a stock rustc trait-bound error. The canonical text is reproduced in §10 and in the `tools!` rustdoc.
- **No trait-method or `impl`-block `#[tool]` form.** Free `async fn` only.
- **No generic free fns.** `async fn foo<T: …>(…)` is rejected with a dedicated diagnostic.
- **No registry-side name collision detection** — that's a runtime registry concern.
- **No third macro for `#[derive(ToolArgs)]` ergonomics.** Users write `#[derive(Deserialize, JsonSchema)]` directly.
- **No panic-handling around the user body.** A panic in the user body propagates as a Rust panic; the runner's tool-call scheduler decides whether to `catch_unwind` (out of scope here).
- **No `#[agent]` proc-macro.** Its own ticket will land later and may refactor shared parsing helpers out of `paigasus-helikon-macros`.

## 10. `tools!` rustdoc — canonical Ctx-mismatch error

The `tools!` macro rustdoc explains the rule: every tool in a single `tools!` invocation must `impl Tool<Ctx>` for the *same* `Ctx`. The shared `Ctx` is inferred from the first tool, or from the LHS type annotation if present.

The macro deliberately does **not** emit a custom diagnostic for `Ctx` mismatches — rustc's stock trait-bound error already names the offending tool. The rustdoc points users at the search string: when rustc reports ``the trait `Tool<…>` is not implemented for ``<tool-name>``, the cause is a `Ctx` mismatch inside `tools!`. (Exact rustc wording drifts across stable releases; the rustdoc paraphrases the grep target rather than pinning a verbatim transcript.)

## 11. Acceptance criteria → evidence map

| AC | Evidence |
|---|---|
| 1. Two-arg tool with doc comments → schema matches golden file | `tests/schema_golden.rs` + `tests/snapshots/schema_golden__add_schema.snap`. |
| 2. `cargo expand` output is readable | §4.2 (reference expansion) + PR-description checklist. |
| 3. Bad args struct → clear compile-fail diagnostic | `tests/trybuild.rs` + `tests/ui/bad_args_*.rs`, `bad_out_not_serialize.rs`, `bad_signature_*.rs`. |
| (Implied) `tools![ … ]` companion macro exists | `tests/end_to_end.rs` (step 1) and the proc-macro entry in `paigasus-helikon-macros`. |
| (Implied) Facade-only consumers can use `#[tool]` and `tools!` | `tests/ui/facade_only_consumer.rs` (compile-pass). |

## 12. Open questions

Carried into implementation review:

- **Schemars 1.x → 2.x risk.** A schemars major-version bump is **not** a routine chore — it changes the JSON-Schema layout the model sees and may require re-prompting evals. Surface this in the workspace's dependency policy when 2.x ships; for SMA-315 we pin 1.x and document the migration risk in the macro rustdoc.
- **`OutputType` (agent.rs) carries `schemars::Schema`; `Tool::schema()` returns `serde_json::Value`.** Two representations for the same concept in one workspace. SMA-320 (structured output) is the natural home for unifying these. SMA-315 stays on `serde_json::Value` because that's the trait's return type today; flag the asymmetry for the future trait-edit ticket.
- **`OnceLock` → `LazyLock`.** When the workspace MSRV moves past 1.80, `LazyLock` lets us drop the `get_or_init` closure. Trivial follow-up chore.
- **`Tool::name()` return type.** Today `&str`. The macro has a compile-time string; a future trait edit to `&'static str` would match what the macro can guarantee. Out of scope for SMA-315 — would be a core trait edit owned separately.
- **No-context-tool sugar.** SMA-315 mandates `&ToolContext<Ctx>` as the first arg. Loosening this to "first arg is optional, defaulting to `Ctx = ()`" is a possible future ergonomic — the path-segment matching rule in §5.4 would need an "if first arg is absent, infer `Ctx = ()`" branch. Not on the SMA-315 path; flagged as a future-ticket trade-off so the decision isn't sliding in implicitly.
