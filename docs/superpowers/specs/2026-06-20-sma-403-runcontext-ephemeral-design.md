# SMA-403 — `RunContext` convenience constructor (`ephemeral` + dependency setters)

**Status:** design approved 2026-06-20
**Ticket:** [SMA-403](https://linear.app/smaschek/issue/SMA-403) — RunContext convenience constructor (builder / ephemeral) to cut boilerplate
**Surfaced by:** [SMA-323](https://linear.app/smaschek/issue/SMA-323) (side-by-side parity examples + dispatch benchmark)
**Crate:** `paigasus-helikon-core` (pure additive)
**Labels:** `area:core`, `stage:2`

## Problem

Every ephemeral run — and every SMA-323 parity example — repeats a 5-argument
incantation in which four arguments are pure boilerplate:

```rust
let ctx: RunContext<()> = RunContext::new(
    Arc::new(()),
    Arc::new(MemorySession::new()),
    HookRegistry::<()>::new(),
    TracerHandle::default(),
    CancellationToken::new(),
);
```

This ceremony is the dominant reason the SMA-323 examples cannot hit the ±20% LOC
parity target against their Python originals, where the equivalent setup is implicit.

`RunContext` is already a **consuming self-builder**: it carries ~11
`with_*` / `without_*` methods (`with_permission_mode`, `with_guard_rules`,
`with_run_config`, `with_extra_secrets`, …) covering every *optional* field. The
only friction is the constructor: `new` is the **single** entry point and it
demands all four dependency arguments (`session`, `hooks`, `tracer`, `cancel`)
even for the all-defaults case. There is no `with_*` setter for those four
fields, so there is no "start from defaults, override one thing" path.

## Goal

Add a low-ceremony constructor and the four missing dependency setters, so the
common case is one line and any single dependency can be overridden by reusing
the existing self-builder idiom — **no new builder type, no behavior change to
`new`.**

## Design

### New public surface on `impl<Ctx> RunContext<Ctx>` (`crates/paigasus-helikon-core/src/context.rs`)

```rust
/// Zero-config context for the common ephemeral case: in-memory session,
/// no hooks, default tracer, fresh cancellation token. Takes the user
/// context **by value** and wraps it in an `Arc` internally, so the unit
/// case is simply `RunContext::ephemeral(())`.
pub fn ephemeral(user_ctx: Ctx) -> Self {
    Self::ephemeral_shared(Arc::new(user_ctx))
}

/// As [`RunContext::ephemeral`], but takes a pre-built `Arc<Ctx>` to **share**
/// one user context across several ephemeral runs (e.g. a request-scoped
/// server sharing an `Arc<AppCtx>`) without cloning it.
pub fn ephemeral_shared(user_ctx: Arc<Ctx>) -> Self {
    Self::new(
        user_ctx,
        Arc::new(MemorySession::new()),
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
}

/// Replace the session handle. Pairs with [`RunContext::ephemeral`] to
/// install a persistent session over the in-memory default.
pub fn with_session(mut self, session: Arc<dyn Session>) -> Self {
    self.session = session;
    self
}

/// Replace the hook registry.
pub fn with_hooks(mut self, hooks: HookRegistry<Ctx>) -> Self {
    self.hooks = hooks;
    self
}

/// Replace the tracer handle (e.g. a populated `TracerHandle::builder()`).
pub fn with_tracer(mut self, tracer: TracerHandle) -> Self {
    self.tracer = tracer;
    self
}

/// Replace the cancellation token (e.g. to share one across runs).
///
/// Intended for the builder chain on a freshly constructed context. It
/// swaps the token wholesale and does **not** retroactively re-link any
/// child tokens already derived via `handoff_child` / `subagent_child` /
/// `to_tool_context` — so it is not a mid-run cancel swap.
pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
    self.cancel = cancel;
    self
}
```

### Semantics & rationale

- **Both constructors are thin delegations to `new`** (`ephemeral` →
  `ephemeral_shared` → `new`) — they produce a byte-for-byte identical
  `RunContext` to today's verbose form. There is no second code path to reason
  about and no behavioral divergence to test against.
- **`ephemeral` takes `Ctx` by value; `ephemeral_shared` takes `Arc<Ctx>`.**
  This two-method split was chosen over a single `ephemeral(impl Into<Arc<Ctx>>)`
  after a verified inference defect: with `impl Into<Arc<Ctx>>`, the value form
  is unambiguous, but the **`Arc` form is not** — `ephemeral(arc)` with an
  unconstrained `Ctx` fails to compile with `E0283` (both `From<T> for Arc<T>`
  and the reflexive `From<T> for T` satisfy the bound), and an explicit
  mis-annotation `RunContext::<Arc<MyCtx>>::ephemeral(arc)` compiles **silently
  double-wrapped**. Both behaviors were reproduced against the compiler
  (`rustc --edition 2021`, 2026-06-20). Taking `Ctx` by value (wrap internally)
  and `Arc<Ctx>` explicitly removes the ambiguity *by construction* — there is
  no `Into` to resolve, so the double-wrap path cannot be expressed. `Ctx` is
  pinned by the argument type in both methods.
- **`ephemeral_shared` has no in-repo caller** (every migration site uses the
  value form). It is a deliberate downstream affordance, so its coverage is a
  dedicated unit test + a rustdoc doctest rather than the migration sweep.
- **The four setters take the `Arc<dyn Session>` / concrete-type forms**,
  deliberately matching the established convention for the existing setters
  (`with_permission_policy` / `with_approval_handler` take `Arc<dyn …>`;
  `with_run_config` takes the concrete type). No `impl Into` magic on the
  setters — only `ephemeral` gets the ergonomic argument.
- **No new struct field.** All four setters write fields that already exist and
  are already copied at every clone site (`handoff_child`, `subagent_child`,
  `to_tool_context`, and `agent_as_tool`'s sub-context rebuild). The
  "add-a-field-means-four-copy-sites" hazard from SMA-414 therefore does **not**
  apply here — there is nothing new to propagate. (Called out explicitly so a
  reviewer does not flag a false omission.)
- `with_cancel` wholesale-replaces the token. Safe on a fresh `ephemeral`
  context because nothing has derived a child token yet; it is the documented
  "share one cancel across runs" path, not a mid-run swap.

### Usage after this change

```rust
// Common case — one line:
let ctx = RunContext::ephemeral(());

// Selective override, reusing the existing self-builder idiom:
let ctx = RunContext::ephemeral(my_ctx)
    .with_session(Arc::new(sqlite_session))
    .with_permission_mode(PermissionMode::Bypass);

// Custom tracer (the langfuse example):
let ctx = RunContext::ephemeral(())
    .with_tracer(TracerHandle::builder().with_session_id("sess-1").build());

// Share one user context across many ephemeral runs:
let app = Arc::new(AppCtx::new());
let ctx = RunContext::ephemeral_shared(Arc::clone(&app));
```

## Migration scope

Full sweep of the all-defaults / defaults-plus-one-override call sites outside
core's own unit tests. Collectively these exercise all four new setters, so the
sweep doubles as integration coverage.

**Migrate to `ephemeral` (+ `with_*` where a field is customized):**

- **Facade examples (6)** + `crates/paigasus-helikon/tests/otel_spans.rs`:
  `budget_assistant_anthropic`, `budget_assistant_openai`, `multi_agent_triage`,
  `structured_output`, `streaming_console` → bare `ephemeral(())`;
  `langfuse_tracing` uses both a `NoopSession` and a populated tracer →
  `ephemeral(()).with_session(Arc::new(NoopSession)).with_tracer(TracerHandle::builder()…build())`
  (exercises two setters in one call site).
- **`-tools` examples (3):** `web_research` (→ `ephemeral(()).with_permission_policy(…)`),
  `os_sandbox_demo`, `explore_sandbox`.
- **`-tools` integration tests:** `sandbox.rs`, `bash.rs`, `sandbox_navigation.rs`
  (bare `ephemeral(())` → `to_tool_context()`).
- **Shared test-helper modules** (high-leverage — transitively updates all their
  callers): `-core/tests/common/mod.rs` (`NoopSession`, generic `Ctx::default()`
  → `ephemeral(Ctx::default()).with_session(Arc::new(NoopSession))`),
  `-runtime-tokio/tests/common/mod.rs` (`NoopSession` + optional custom
  `cancel`/`registry`/`session` → `.with_session` / `.with_cancel` / `.with_hooks`),
  `-mcp/tests/support/mod.rs` (custom cancel → `ephemeral(()).with_cancel(cancel)`).
- **`-core` integration tests** that call `new` directly: `handoff.rs`,
  `workflow_sequential.rs`, `workflow_parallel.rs`, `workflow_loop.rs`,
  `workflow_pipeline.rs`, `workflow_tracing.rs`, `subagent_propagation.rs`,
  `agent_as_tool.rs`, `failure_slot.rs`.

**Deliberately kept on `new()`:**

- `crates/paigasus-helikon-core/src/context.rs` unit tests — these are the
  canonical coverage for `new` and the four clone sites; converting them would
  reduce `new` coverage. New `ephemeral`/setter tests are **added alongside**
  them.
- Other `-core/src/*.rs` inline unit tests (`agent.rs`, `runner.rs`,
  `control.rs`, `agent_as_tool.rs`) and `-mcp/src/server.rs` — internal, not
  "examples or helpers"; out of scope for this sweep.

## Docs

User-facing additive `-core` API → CLAUDE.md requires book + crate-README parity:

- Add rustdoc doctests to `context.rs` covering each migrated pattern so the
  examples are **compiler-checked** by `cargo test --doc` (mdBook code blocks
  are not compiled by CI — only linkcheck runs — so the book pages alone can
  drift to a typo'd API silently): the `ephemeral(())` one-liner, an
  `ephemeral_shared(Arc<_>)` form, and a `.with_session(...).with_tracer(...)`
  builder chain. (`MemorySession` is re-exported at the core crate root.)
- **mdBook:** `RunContext::new` appears across **9 pages** under `docs/book/src/`
  (`introduction`, `getting-started/quickstart`, and the `concepts/`
  pages `core-primitives`, `multi-agent-patterns`, `observability-evaluation`,
  `structured-output-builder`, `permissions-guardrails-hooks`, `sessions`,
  `agent-loop`). Switch the verbose form to `ephemeral` where it is **incidental
  setup boilerplate** (quickstart, multi-agent, structured-output, etc.). A page
  that is specifically *documenting construction* (e.g. `core-primitives` /
  `sessions` showing what the five arguments are, or a custom-session example)
  may legitimately keep `new` — make that a per-page call, not a blanket
  find-replace. `mdbook build docs/book` must stay clean
  (`linkcheck warning-policy = "error"`).
- **`crates/paigasus-helikon-core/README.md`:** mentions `RunContext` only in
  prose (no code snippet), so no README code change is required — confirm the
  prose still reads correctly and move on.

## Testing

Unit tests added to `runcontext_tests` in `context.rs`:

- `ephemeral_matches_new_defaults` — `agent_depth == 0`, `permission_mode ==
  Default`, `default_guards()`, `redact_output()`, `run_config().is_none()`,
  `hooks().is_empty()`, `deny_rules()`/`allow_rules()` empty. (Sync-only, to
  match the existing `#[test]` style of the inline `runcontext_tests` module —
  no tokio runtime is pulled in. The async session `append`/`snapshot` path is
  exercised instead by the migration sweep, which runs real agent loops through
  ephemeral-built `MemorySession` contexts.)
- `ephemeral_shared_keeps_inner_ctx_type` — build via `ephemeral_shared(Arc::new(Marker))`
  and assert the resolved `Ctx` is the **inner** type, not `Arc<Marker>` (guards
  against the double-wrap the old `impl Into` design could express; here it is
  structurally impossible, but the test locks the contract).
- `with_session_swaps_handle`, `with_hooks_installs_registry`,
  `with_tracer_round_trips` (surfaces `session_id`), `with_cancel_token_cancels`
  (the installed token's `cancel()` is observed via the context's `cancel()`).

Existing `new`-based tests in `context.rs` are retained unchanged.

## Versioning & release

Pure additive surface on the already-released `-core` crate.

- release-plz auto-bumps `-core` (**patch**, per the 0.x additive-feat rule) and
  cascades the facade pin on merge.
- **No same-PR manual `-core`/facade bump.** That ritual exists only for a stub
  ascending from `0.0.0`, or when a *separate published* crate consumes
  same-PR core API at publish-verify time. Here the only consumers are in-repo
  examples and tests, which never publish — so the normal release-plz flow
  applies untouched.

## Non-goals

- No separate `RunContextBuilder` type.
- No change to `RunContext::new` (signature or behavior).
- No `Default` impl for `RunContext` (a context without a session is meaningless
  — the existing rustdoc says so).
