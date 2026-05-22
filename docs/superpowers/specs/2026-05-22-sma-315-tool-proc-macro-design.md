# SMA-315 — `#[tool]` proc-macro with schemars-derived JSON Schema — design

- **Linear:** [SMA-315](https://linear.app/smaschek/issue/SMA-315/tool-proc-macro-with-schemars-derived-json-schema)
- **Branch:** `feature/sma-315-tool-proc-macro-with-schemars-derived-json-schema`
- **Status:** design (awaiting implementation plan)
- **Author:** Sven Maschek
- **Date:** 2026-05-22

## 1. Goal

Ship the ergonomic path for in-process Rust tools. An `async fn` annotated with `#[tool]` (plus a `#[derive(Deserialize, JsonSchema)]` args struct) expands into a fully-formed `impl Tool<Ctx>` — `name`, `description`, `schema`, `output_schema`, `invoke` — that downstream agent code can hold as `Arc<dyn Tool<Ctx>>`. A companion `tools![ … ]` declarative macro boxes a heterogeneous set into `Vec<Arc<dyn Tool<Ctx>>>`.

The Linear ticket's acceptance criteria are:

1. A two-arg tool with doc comments produces a JSON Schema matching a golden file.
2. `cargo expand` output is readable (no surprising lifetimes).
3. A bad args struct (e.g. non-`Deserialize`) fails to compile with a clear diagnostic via `compile_fail` tests in `trybuild`.

AC #1 is locked by `tests/schema_golden.rs` with `insta` snapshots (§6). AC #3 is locked by a set of UI tests under `tests/ui/` driven by `trybuild` (§6). AC #2 is a manual-review checklist on the PR — a representative expansion is captured in §4.4 of this spec as the reference point.

### 1.1 Scope boundary (against peer tickets)

The trait surface this ticket implements against — `Tool<Ctx>`, `ToolContext<Ctx>`, `ToolOutput`, `ToolError` — was landed by SMA-312 and SMA-313 and is **not modified** by SMA-315. `paigasus-helikon-tools` (first-party tool crates) stays a stub; HTTP/FS/exec tools land in later tickets and will consume `#[tool]` as their primary authoring path.

A second proc-macro (`#[agent]`, planned in a downstream ticket) will share infrastructure with `#[tool]` — attribute parsing patterns, doc-comment extraction, the `crate = …` override knob. SMA-315 builds those primitives without pre-abstracting them; refactoring lands when the second consumer arrives.

## 2. Decisions and rationale

Eight decisions, scoped to the SMA-315 surface.

| Decision | Choice | Rationale |
|---|---|---|
| Function signature shape | **`async fn foo(ctx: &ToolContext<Ctx>, args: Args) -> Result<Out, ToolError>`** — two positional args, mandatory `ToolContext` first. | Mirrors `Tool::invoke` 1:1, makes expansion mechanical, and keeps the macro's job to "wire JSON ↔ Args/Out and forward". The `&ToolContext<Ctx>` is mandatory even when the tool body ignores it — uniformity beats a per-tool arity-detection branch in the macro, and `_` patterns are free at the call site. |
| Generated artifact | **Hide the fn, emit a unit struct of the same ident** plus `impl Tool<Ctx>` for it. The original fn body moves into a private `__helikon_orig_<ident>` free fn the impl calls. | Lets `tools![add, mul]` use bare idents — no `()` constructor, no `Tool`-suffix naming convention to remember. The bare-ident UX is the dominant call-site readability win cited in the ticket. |
| Description source | **`#[tool(description = "…")]` wins; fall back to `///` doc comments; compile error if neither.** | Description is required for the LLM contract (the trait returns `&str`, not `Option<&str>`). Attr-wins matches typical Rust semantics ("the explicit thing overrides the implicit thing"). Doc-comment fallback satisfies the ticket's "reads doc comments from the function" requirement and removes duplication for users who already wrote rustdoc. |
| Output schema generation | **Auto-emit when `Out: JsonSchema`; else fall back to `None`.** Implemented via the autoref-specialization trick (§4.3). | Zero-config for users who derive `JsonSchema` on the output type (the common path), no hard `JsonSchema` bound on users returning `serde_json::Value` or other non-schemars types. Autoref-specialization is well-trodden on stable Rust (cf. `tracing-error`, `anyhow`'s context). |
| Crate-path resolution | **Default `::paigasus_helikon_core::…`, override via `#[tool(crate = ::path)]`.** | Matches serde/schemars/clap convention. The direct-core consumer pays no syntax cost; facade-only consumers opt in with `crate = ::paigasus_helikon::core`. Avoids the trap of routing through the facade unconditionally (which would force a `paigasus-helikon` dep on every consumer). |
| `tools![]` host crate | **`paigasus-helikon-core`** — `tools!` is a `macro_rules!` macro, not a proc-macro, and proc-macro crates can't export `macro_rules!`. Co-locates with the `Tool` trait so `$crate::Tool` resolves naturally. The facade re-exports it. | Single re-export site keeps the surface coherent (`paigasus_helikon::{tool, tools}`). Putting `tools!` in the facade would force the facade to host a `macro_rules!` definition for a trait it doesn't define — awkward layering. |
| Attribute parsing | **Hand-rolled `syn`/`quote`, no `darling`.** | The attribute surface is three keys (`description`, `name`, `crate`). `darling`'s diagnostics are marginally worse than well-spanned `syn::Error`s, and pulling a 5-crate parsing framework for ~30 LoC of attr parsing is the wrong default. If `#[agent]` later adds 8+ attribute keys, revisit. |
| Schema cache | **`OnceLock<serde_json::Value>` per tool.** Lazy-initialize on the first `schema()` call. | `Tool::schema()` returns `&Value` (per the trait), so per-call computation isn't an option. Eager `lazy_static`-style initialization at module load is overkill; `OnceLock` pays the schemars cost once on first registration and never again. |

## 3. Files added / modified

### Added

| Path | Purpose |
|---|---|
| `crates/paigasus-helikon-macros/src/lib.rs` | `#[proc_macro_attribute] tool` entry point + module imports. |
| `crates/paigasus-helikon-macros/src/attr.rs` | Parse `#[tool(description = …, name = …, crate = …)]`. |
| `crates/paigasus-helikon-macros/src/signature.rs` | Parse and validate the target `async fn` signature; extract `Args`, `Out`, `Ctx`. |
| `crates/paigasus-helikon-macros/src/expand.rs` | Codegen — emit the unit struct, the `impl Tool<Ctx>`, the moved body fn, the `OnceLock` statics, and the autoref-specialization helper. |
| `crates/paigasus-helikon-macros/tests/schema_golden.rs` | Snapshot test for the two-arg-tool schema (AC #1). Uses `insta::assert_snapshot!`. |
| `crates/paigasus-helikon-macros/tests/snapshots/schema_golden__add_schema.snap` | Golden file for the schema snapshot. |
| `crates/paigasus-helikon-macros/tests/trybuild.rs` | Entry point for the `trybuild` UI suite. |
| `crates/paigasus-helikon-macros/tests/ui/bad_args_not_deserialize.rs` (+ `.stderr`) | Compile-fail: args struct missing `Deserialize`. |
| `crates/paigasus-helikon-macros/tests/ui/bad_args_not_jsonschema.rs` (+ `.stderr`) | Compile-fail: args struct missing `JsonSchema`. |
| `crates/paigasus-helikon-macros/tests/ui/no_description.rs` (+ `.stderr`) | Compile-fail: no attr description and no doc comment. |
| `crates/paigasus-helikon-macros/tests/ui/bad_signature_wrong_ctx.rs` (+ `.stderr`) | Compile-fail: first arg isn't `&ToolContext<_>`. |
| `crates/paigasus-helikon-macros/tests/ui/bad_signature_not_async.rs` (+ `.stderr`) | Compile-fail: non-async fn. |
| `crates/paigasus-helikon-macros/tests/ui/bad_name.rs` (+ `.stderr`) | Compile-fail: `#[tool(name = "has spaces")]`. |
| `crates/paigasus-helikon-macros/tests/end_to_end.rs` | Behavioral test — invoke a generated tool, assert `name`/`description`/`schema`/`invoke` semantics and the `tools![…]` macro shape. |
| `crates/paigasus-helikon-core/src/macros.rs` | Hosts the `tools!` `macro_rules!` definition. |

### Modified

| Path | Change |
|---|---|
| `crates/paigasus-helikon-macros/Cargo.toml` | Add `proc-macro2`, `quote`, `syn` (deps); `paigasus-helikon-core` (path), `async-trait`, `schemars`, `serde`, `serde_json`, `tokio` (features `macros`, `rt`), `trybuild`, `insta` (dev-deps). |
| `crates/paigasus-helikon-core/Cargo.toml` | No dependency change. The new `macros` module is pure `macro_rules!`. |
| `crates/paigasus-helikon-core/src/lib.rs` | `mod macros;` + module-level docstring; the `#[macro_export]` on `tools!` makes it available as `paigasus_helikon_core::tools`. |
| `crates/paigasus-helikon/Cargo.toml` | No structural change. `paigasus-helikon-macros` stays optional behind the existing `macros` feature, per the facade convention (core unconditional, siblings feature-gated). |
| `crates/paigasus-helikon/src/lib.rs` | Add a `#[cfg(feature = "macros")] pub use paigasus_helikon_macros::tool;` re-export. `tools!` is `macro_rules!` in `paigasus-helikon-core` (always present) and is reachable as `paigasus_helikon::tools` via a `pub use paigasus_helikon_core::tools;` re-export at the crate root. |
| `Cargo.toml` (workspace) | Add `proc-macro2 = "1"`, `quote = "1"`, `syn = { version = "2", features = ["full", "extra-traits"] }`, `trybuild = "1"` to `[workspace.dependencies]`. (`schemars`, `serde`, `serde_json`, `tokio`, `async-trait`, `insta` are already declared.) |

## 4. Generated code shape

### 4.1 User-facing example

```rust
use paigasus_helikon::{tool, tools};
use paigasus_helikon_core::{Tool, ToolContext, ToolError};
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
#[tool]
async fn add(
    _ctx: &ToolContext<MyCtx>,
    args: AddArgs,
) -> Result<AddOut, ToolError> {
    Ok(AddOut { sum: args.a + args.b })
}

let registry: Vec<Arc<dyn Tool<MyCtx>>> = tools![add /* , mul, … */];
```

### 4.2 Reference expansion (for AC #2 "readable" criterion)

The expansion of the `#[tool]` block above, edited for spacing only:

```rust
#[allow(non_camel_case_types)]
pub struct add;

static ADD_INPUT_SCHEMA: ::std::sync::OnceLock<::serde_json::Value> =
    ::std::sync::OnceLock::new();
static ADD_OUTPUT_SCHEMA: ::std::sync::OnceLock<Option<::serde_json::Value>> =
    ::std::sync::OnceLock::new();

#[::async_trait::async_trait]
impl ::paigasus_helikon_core::Tool<MyCtx> for add {
    fn name(&self) -> &str { "add" }
    fn description(&self) -> &str { "Adds two numbers." }
    fn schema(&self) -> &::serde_json::Value {
        ADD_INPUT_SCHEMA.get_or_init(|| {
            ::serde_json::to_value(::schemars::schema_for!(AddArgs))
                .expect("schemars schema must serialize")
        })
    }
    fn output_schema(&self) -> Option<&::serde_json::Value> {
        ADD_OUTPUT_SCHEMA
            .get_or_init(|| {
                // Autoref-specialization picks the JsonSchema impl if available.
                (&&::paigasus_helikon_macros::__private::OutputSchemaProbe::<AddOut>::NEW)
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
        let out = __helikon_orig_add(ctx, parsed).await?;
        let content = ::serde_json::to_value(&out)
            .map_err(|e| ::paigasus_helikon_core::ToolError::Other(e.into()))?;
        ::std::result::Result::Ok(
            ::paigasus_helikon_core::ToolOutput::new(content)
        )
    }
}

async fn __helikon_orig_add(
    _ctx: &::paigasus_helikon_core::ToolContext<MyCtx>,
    args: AddArgs,
) -> ::std::result::Result<AddOut, ::paigasus_helikon_core::ToolError> {
    Ok(AddOut { sum: args.a + args.b })
}
```

### 4.3 Autoref-specialization for `output_schema`

The proc-macro cannot ask "does `Out: JsonSchema`?" at expansion time — proc-macros operate on syntax, not types. The autoref-specialization trick gives us trait-aware codegen on stable Rust by arranging two `schema(&self)` candidates at different deref levels and letting method resolution pick the closer one:

```rust
// In paigasus_helikon_macros::__private (re-exported privately for macro use):
pub struct OutputSchemaProbe<T>(::std::marker::PhantomData<T>);

impl<T> OutputSchemaProbe<T> {
    pub const NEW: Self = Self(::std::marker::PhantomData);
}

// Specialized arm — gated trait impl on `&OutputSchemaProbe<T>` (one autoref).
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

// Fallback arm — inherent method on `OutputSchemaProbe<T>` (two autoref steps).
impl<T> OutputSchemaProbe<T> {
    pub fn schema(&self) -> Option<::serde_json::Value> { None }
}
```

When the macro emits `(&&OutputSchemaProbe::<Out>::NEW).schema()`, method resolution starts at `&&Probe<Out>`, auto-derefs once to `&Probe<Out>`, and finds the `OutputSchemaProbeSpec::schema` impl **iff** `Out: JsonSchema`. If the bound holds, that impl wins because it's fewer deref steps away. If it doesn't, resolution falls through to `Probe<Out>` and finds the inherent `fn schema(&self) -> None` — the fallback. No nightly features, no `JsonSchema` bound leaked onto the user's `Out`.

The probe lives in `paigasus_helikon_macros::__private` (semver-exempt). User code never references it directly; only the generated code does.

### 4.4 `tools![]` expansion

```rust
#[macro_export]
macro_rules! tools {
    () => {
        ::std::vec::Vec::<
            ::std::sync::Arc<dyn $crate::Tool<_>>
        >::new()
    };
    ($($tool:expr),+ $(,)?) => {
        ::std::vec![
            $(
                ::std::sync::Arc::new($tool)
                    as ::std::sync::Arc<dyn $crate::Tool<_>>
            ),+
        ]
    };
}
```

Defined in `paigasus_helikon_core::macros`. `$crate::Tool` resolves to `paigasus_helikon_core::Tool` at expansion. The empty arm is necessary because a `vec![]` with no elements can't infer the `Tool<_>` element type without explicit annotation, and an empty registry is occasionally useful (e.g. building one progressively from config).

## 5. Description, name, and crate resolution — exact rules

| Aspect | Resolution order | On failure |
|---|---|---|
| Description | `#[tool(description = "lit")]` → concat `///` doc lines (strip leading space, join `\n`, trim trailing whitespace) → **error** | ``compile_error!("tool `<name>` requires a description: add `#[tool(description = \"…\")]` or a `///` doc comment");`` spanned to the fn ident. |
| Tool name | `#[tool(name = "lit")]` → fn ident as string | If override fails the `[A-Za-z_][A-Za-z0-9_-]*` regex, `compile_error!("tool name must match [A-Za-z_][A-Za-z0-9_-]*");` spanned to the name literal. |
| Crate path | `#[tool(crate = ::path)]` → `::paigasus_helikon_core` | Path is taken verbatim; invalid paths surface as rustc errors on the expanded code. |

Argument-field descriptions are **not** handled by the `#[tool]` macro. They flow through `#[derive(JsonSchema)]`, which already extracts `///` doc comments on struct fields into the schema's `description` field. This is the explicit boundary between the two derives — `JsonSchema` owns the args struct, `#[tool]` owns the fn glue.

## 6. Testing strategy

Three layers, all under `crates/paigasus-helikon-macros/tests/`.

### 6.1 Schema golden file (AC #1)

`tests/schema_golden.rs`:

- Defines `AddArgs` and `AddOut` exactly as in §4.1.
- Constructs `add` (the macro-generated unit struct), calls `.schema()`, serializes the returned `&Value` with `serde_json::to_string_pretty`.
- `insta::assert_snapshot!(serialized)`.

First-run writes `tests/snapshots/schema_golden__add_schema.snap`. The reviewer eyeballs the snapshot during PR — it's the "schema matching golden file" artifact the AC names. Subsequent runs diff; `cargo insta review` updates intentionally.

The snapshot covers the things the AC implicitly cares about: type mapping (`i64` → `integer`), `required` array population, per-field `description` from doc comments.

### 6.2 `trybuild` compile-fail UI tests (AC #3)

`tests/trybuild.rs`:

```rust
#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
```

UI cases (each paired with a `.stderr` golden):

| Case | What it exercises |
|---|---|
| `bad_args_not_deserialize.rs` | Args struct lacks `Deserialize` → rustc trait-bound error from the generated `from_value` call. |
| `bad_args_not_jsonschema.rs` | Args struct lacks `JsonSchema` → rustc trait-bound error from the generated `schema_for!` call. |
| `no_description.rs` | `#[tool]` with no description attr and no doc comment → macro-emitted `compile_error!`. |
| `bad_signature_wrong_ctx.rs` | First arg is `&MyCtx` instead of `&ToolContext<MyCtx>` → macro-emitted diagnostic spanned at the first arg. |
| `bad_signature_not_async.rs` | `fn` instead of `async fn` → macro-emitted diagnostic. |
| `bad_name.rs` | `#[tool(name = "has spaces")]` → macro-emitted diagnostic. |

Stderr files are regenerated with `TRYBUILD=overwrite cargo test --test trybuild` when a diagnostic intentionally changes. Reviewers diff the `.stderr` files alongside the source.

### 6.3 End-to-end behavioral test

`tests/end_to_end.rs` — a single `#[tokio::test]`:

1. Construct the registry via `tools![add]`.
2. Assert `registry.len() == 1`, `registry[0].name() == "add"`, `registry[0].description() == "Adds two numbers."`.
3. Build a minimal `ToolContext<MyCtx>` (via `ToolContext::new` with default tracer/cancel/user_ctx).
4. Invoke with valid JSON `{"a": 2, "b": 3}` → assert `Ok(ToolOutput { content: json!({"sum": 5}) })`.
5. Invoke with invalid JSON `{"a": "not a number", "b": 3}` → assert `Err(ToolError::InvalidArgs { schema_errors })` with non-empty `schema_errors`.
6. Call `schema()` twice; assert pointer equality on the returned `&Value` (proves `OnceLock` caching works).
7. Call `output_schema()`; assert `Some(_)` (because `AddOut: JsonSchema`).
8. Define a second tool `OpaqueTool` whose `Out` is a non-`JsonSchema` newtype; assert its `output_schema()` returns `None`.

### 6.4 The "readable `cargo expand`" criterion (AC #2)

Not automatable. Captured as:

- The reference expansion in §4.2 of this spec — kept current when the macro changes.
- A PR-description checklist item: "Ran `cargo expand --test end_to_end` and verified expansion matches §4.2 within whitespace/comment differences."

## 7. Error handling & diagnostics

### 7.1 Compile-time (proc-macro side)

All parse failures use `syn::Error::new_spanned(tok, msg).to_compile_error()` so the error span lands on the offending token, not on `#[tool]`.

| Condition | Span | Message |
|---|---|---|
| Missing description | fn ident | ``tool `<name>` requires a description: add `#[tool(description = "…")]` or a `///` doc comment`` |
| Non-async fn | `fn` keyword | ``#[tool] requires an `async fn` `` |
| Wrong arity | fn signature | ``#[tool] expects two args: `&ToolContext<Ctx>` and an args struct`` |
| First arg not `&ToolContext<_>` | first arg's pattern | ``#[tool] expects the first argument to be `&ToolContext<Ctx>`, found `<actual>` `` |
| `Self` or trait-method form | fn keyword | ``#[tool] applies to free `async fn` only`` |
| Bad `name = …` | name literal | ``tool name must match `[A-Za-z_][A-Za-z0-9_-]*` `` |
| Unknown attribute key | key token | ``unknown #[tool] attribute `<key>`; expected one of `description`, `name`, `crate` `` |

Trait-bound failures on `Args` (missing `Deserialize`/`JsonSchema`) and `Out` (missing `Serialize`) are caught downstream by rustc when the expanded code references those traits. The `trybuild` UI tests pin the resulting rustc diagnostics.

### 7.2 Runtime (generated code)

| Failure | Outcome |
|---|---|
| `serde_json::from_value::<Args>(args)` fails | `Err(ToolError::InvalidArgs { schema_errors: vec![e.to_string()] })` — recoverable per ADR-10. |
| User body returns `Err(ToolError)` | Propagated verbatim via `?`. |
| `serde_json::to_value(&out)` fails | `Err(ToolError::Other(e.into()))` — non-recoverable. Output serialization failures are programmer errors (e.g. non-string map keys); the loop should not retry. |

`schemars::schema_for!` failure is impossible at runtime — schemars generates the schema infallibly given the type compiles. The `OnceLock` initializer asserts via `.expect("schemars schema must serialize")` for the `to_value` step; failure here is a `serde_json` bug and panicking on first use is correct.

## 8. Cargo wiring

### 8.1 Workspace `[workspace.dependencies]` additions

The workspace already declares `schemars = "1"`, `serde`, `serde_json`, `tokio`, `async-trait`, and `insta = "1"`. Genuinely new pins for SMA-315:

```toml
proc-macro2 = "1"
quote = "1"
syn = { version = "2", features = ["full", "extra-traits"] }
trybuild = "1"
```

`schemars` 1.x is the current stable line as of 2026-05. The schema layout it emits is what the golden file pins; bumping to a hypothetical 2.x is a follow-up chore that updates the snapshot.

### 8.2 `crates/paigasus-helikon-macros/Cargo.toml`

```toml
[dependencies]
proc-macro2.workspace = true
quote.workspace = true
syn.workspace = true

[dev-dependencies]
paigasus-helikon-core = { path = "../paigasus-helikon-core" }
async-trait.workspace = true
schemars.workspace = true
serde = { workspace = true, features = ["derive"] }
serde_json.workspace = true
tokio = { workspace = true, features = ["macros", "rt"] }
trybuild.workspace = true
insta.workspace = true
```

A `__private` module is published behind a `#[doc(hidden)]` re-export so the macro can reference `::paigasus_helikon_macros::__private::OutputSchemaProbe`. This module is **semver-exempt** and documented as such in its module docstring — only the macro-generated code touches it.

### 8.3 `crates/paigasus-helikon-core/Cargo.toml`

No dep change. `tools!` is `macro_rules!`, no proc-macro toolchain needed.

### 8.4 `crates/paigasus-helikon/Cargo.toml` (facade)

No structural change. The existing wiring stays:

```toml
[dependencies]
paigasus-helikon-macros = { workspace = true, optional = true }

[features]
macros = ["dep:paigasus-helikon-macros"]
```

In `src/lib.rs`:

```rust
#[cfg(feature = "macros")]
pub use paigasus_helikon_macros::tool;

pub use paigasus_helikon_core::tools;
```

`tools!` is `macro_rules!` in `paigasus-helikon-core` (unconditional), so it's always reachable as `paigasus_helikon::tools`. `#[tool]` requires `features = ["macros"]` from the facade — same convention as every other Stage-1 crate.

## 9. Non-goals (out of scope for SMA-315)

- **No first-party tool implementations.** `paigasus-helikon-tools` stays a stub.
- **No streaming tool outputs.** `ToolOutput` is single-shot.
- **No multi-modal content.** `ToolOutput.content` stays `serde_json::Value`. The `#[non_exhaustive]` on the struct preserves room for later.
- **No `JsonSchema`-driven validation pass before `serde` deserialize.** We rely on `serde` to reject bad inputs. Schemars-level validation as a pre-deserialize step is a follow-up if real "schema accepts but serde rejects" cases emerge.
- **No `Ctx` inference fallback in `tools![]`.** Mismatched `Ctx` across tools surfaces as a stock rustc type error, not a macro-supplied diagnostic.
- **No trait-method or `impl`-block `#[tool]` form.** Free `async fn` only.
- **No registry-side name collision detection** — that's a runtime registry concern.
- **No third macro for `#[derive(ToolArgs)]` ergonomics.** Users write `#[derive(Deserialize, JsonSchema)]` directly; collapsing the derive list is a deliberate non-goal.
- **No `#[agent]` proc-macro.** Its own ticket will land later and may refactor shared parsing helpers out of `paigasus-helikon-macros`.

## 10. Acceptance criteria → evidence map

| AC | Evidence |
|---|---|
| 1. Two-arg tool with doc comments → schema matches golden file | `tests/schema_golden.rs` + `tests/snapshots/schema_golden__add_schema.snap`. |
| 2. `cargo expand` output is readable | §4.2 of this spec (reference expansion) + PR-description checklist. |
| 3. Bad args struct → clear compile-fail diagnostic | `tests/trybuild.rs` + `tests/ui/bad_args_*.rs` and `.stderr` goldens. |
| (Implied) `tools![]` companion macro exists | `crates/paigasus-helikon-core/src/macros.rs`, exercised by `tests/end_to_end.rs`. |

## 11. Open questions

None at design time. Likely revisits during implementation:

- Whether to publicize a `Tool::name() -> &'static str` change (currently `&str`) to align with the macro's compile-time string. Out of scope for SMA-315; would be a trait edit owned by core.
- Whether `OnceLock` becomes `LazyLock` (stabilized 1.80) when MSRV moves past 1.80. MSRV is 1.75 today; `OnceLock` is the correct primitive at MSRV.
