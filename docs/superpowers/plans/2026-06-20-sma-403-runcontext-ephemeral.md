# RunContext `ephemeral` Constructor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `RunContext::ephemeral(Ctx)` + `ephemeral_shared(Arc<Ctx>)` + four dependency setters (`with_session`/`with_hooks`/`with_tracer`/`with_cancel`) to cut the 5-argument `RunContext::new` boilerplate to one line, then migrate every default-ish call site to it.

**Architecture:** Pure-additive surface on `impl<Ctx> RunContext<Ctx>` in `paigasus-helikon-core`. Both constructors are thin delegations to the unchanged `new` (`ephemeral` → `ephemeral_shared` → `new`), so the result is byte-for-byte identical to today's verbose form. The setters write fields that already exist and are already copied at every clone site, so there is no new propagation surface.

**Tech Stack:** Rust (workspace MSRV 1.85), `cargo`, `clippy`, `mdbook`. Spec: `docs/superpowers/specs/2026-06-20-sma-403-runcontext-ephemeral-design.md`.

**Branch:** `feature/sma-403-runcontext-convenience-constructor-builder-ephemeral-to-cut` (already created and checked out; the design spec is already committed on it).

---

## File Structure

- `crates/paigasus-helikon-core/src/context.rs` — **the only production change.** Adds six methods to the existing `impl<Ctx> RunContext<Ctx>` block (after `new`), new unit tests in the `runcontext_tests` module, and rustdoc doctests. No new file, no new field.
- Migration-only edits (no behavior change): 6 facade examples + 1 facade test, 3 `-tools` examples + 3 `-tools` tests, 3 shared test-helper modules, 9 `-core` integration tests, and 9 mdBook pages.

**Recurring gotcha — orphaned imports.** Removing the verbose constructor orphans imports such as `TracerHandle`, `HookRegistry`, `CancellationToken`, `MemorySession`, and the `Session` trait (used only by the `as Arc<dyn Session>` cast). The helper modules carry `#![allow(dead_code)]`, but that lint does **not** silence `unused_imports`, and the `clippy -D warnings` gate is required. **Every migration task therefore ends with a clippy pass that names the exact unused imports to delete.** The `as Arc<dyn Session>` cast is dropped on migration because `with_session(Arc<dyn Session>)` coerces `Arc<ConcreteSession>` at the call site.

---

## Task 1: Add `ephemeral`, `ephemeral_shared`, and the four dependency setters

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs` (impl block, insert after `new` which ends at line 134; test module `runcontext_tests`, starts line 426)
- Test: same file, `runcontext_tests` module

- [ ] **Step 1: Write the failing unit tests**

Add these six tests inside `mod runcontext_tests` (e.g. immediately after the existing `subagent_child_shares_state_fresh_actions_increments_depth` test, before the module's closing `}` at line 754):

```rust
#[test]
fn ephemeral_matches_new_defaults() {
    let ctx: RunContext<()> = RunContext::ephemeral(());
    assert_eq!(ctx.agent_depth(), 0);
    assert_eq!(ctx.permission_mode(), crate::PermissionMode::Default);
    assert!(ctx.default_guards());
    assert!(ctx.redact_output());
    assert!(ctx.run_config().is_none());
    assert!(ctx.hooks().is_empty());
    assert!(ctx.deny_rules().is_empty());
    assert!(ctx.allow_rules().is_empty());
}

#[test]
fn ephemeral_shared_keeps_inner_ctx_type() {
    struct Marker;
    let ctx: RunContext<Marker> = RunContext::ephemeral_shared(Arc::new(Marker));
    // Compile-time proof that Ctx == Marker, not Arc<Marker>: user_ctx()
    // returns &Arc<Marker>, so .as_ref() yields &Marker. If the old
    // `impl Into<Arc<Ctx>>` double-wrap interpretation were possible this
    // line would not type-check.
    let _inner: &Marker = ctx.user_ctx().as_ref();
}

#[test]
fn with_session_swaps_handle() {
    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let ctx: RunContext<()> = RunContext::ephemeral(()).with_session(Arc::clone(&session));
    assert!(Arc::ptr_eq(ctx.session(), &session));
}

#[test]
fn with_hooks_installs_registry() {
    struct NoopHook;
    #[async_trait::async_trait]
    impl crate::Hook<()> for NoopHook {
        async fn on_event(
            &self,
            _ctx: &RunContext<()>,
            _event: &crate::HookEvent,
        ) -> crate::HookDecision {
            crate::HookDecision::Allow
        }
    }
    let mut registry = HookRegistry::<()>::new();
    registry.push(Arc::new(NoopHook));
    let ctx: RunContext<()> = RunContext::ephemeral(()).with_hooks(registry);
    assert!(!ctx.hooks().is_empty());
}

#[test]
fn with_tracer_round_trips() {
    let ctx: RunContext<()> = RunContext::ephemeral(())
        .with_tracer(TracerHandle::builder().with_session_id("sess-1").build());
    assert_eq!(ctx.tracer().session_id(), Some("sess-1"));
}

#[test]
fn with_cancel_token_cancels() {
    let token = CancellationToken::new();
    let ctx: RunContext<()> = RunContext::ephemeral(()).with_cancel(token.clone());
    assert!(!ctx.cancel().is_cancelled());
    token.cancel();
    assert!(ctx.cancel().is_cancelled());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --lib runcontext_tests 2>&1 | tail -20`
Expected: FAIL — compile errors `no function or associated item named 'ephemeral' found` (and `ephemeral_shared`, `with_session`, `with_hooks`, `with_tracer`, `with_cancel`).

- [ ] **Step 3: Write the minimal implementation**

Insert these six methods into `impl<Ctx> RunContext<Ctx>` immediately after the `new` method (after its closing `}` on line 134, before the `user_ctx` getter):

```rust
    /// Zero-config context for the common ephemeral case: in-memory session,
    /// no hooks, default tracer, fresh cancellation token. Takes the user
    /// context **by value** and wraps it in an `Arc` internally, so the unit
    /// case is simply `RunContext::ephemeral(())`.
    ///
    /// # Example
    ///
    /// ```
    /// use paigasus_helikon_core::RunContext;
    ///
    /// let ctx: RunContext<()> = RunContext::ephemeral(());
    /// assert!(ctx.hooks().is_empty());
    /// ```
    pub fn ephemeral(user_ctx: Ctx) -> Self {
        Self::ephemeral_shared(Arc::new(user_ctx))
    }

    /// As [`RunContext::ephemeral`], but takes a pre-built `Arc<Ctx>` to
    /// **share** one user context across several ephemeral runs (e.g. a
    /// request-scoped server sharing an `Arc<AppCtx>`) without cloning it.
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use paigasus_helikon_core::RunContext;
    ///
    /// struct AppCtx;
    /// let app = Arc::new(AppCtx);
    /// let _ctx = RunContext::ephemeral_shared(Arc::clone(&app));
    /// ```
    pub fn ephemeral_shared(user_ctx: Arc<Ctx>) -> Self {
        Self::new(
            user_ctx,
            Arc::new(crate::MemorySession::new()),
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
    }

    /// Replace the session handle. Pairs with [`RunContext::ephemeral`] to
    /// install a persistent session over the in-memory default.
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use paigasus_helikon_core::{MemorySession, RunContext, TracerHandle};
    ///
    /// let _ctx: RunContext<()> = RunContext::ephemeral(())
    ///     .with_session(Arc::new(MemorySession::new()))
    ///     .with_tracer(TracerHandle::builder().with_session_id("s").build());
    /// ```
    pub fn with_session(mut self, session: Arc<dyn Session>) -> Self {
        self.session = session;
        self
    }

    /// Replace the hook registry.
    pub fn with_hooks(mut self, hooks: HookRegistry<Ctx>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Replace the tracer handle (e.g. a populated [`TracerHandle::builder`]).
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

Note: `MemorySession` is referenced as `crate::MemorySession` (it is not currently in `context.rs`'s top-level `use`; the fully-qualified path avoids touching the import block).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core --lib runcontext_tests 2>&1 | tail -20`
Expected: PASS — all existing `runcontext_tests` plus the six new tests green.

- [ ] **Step 5: Format and lint**

Run:
```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
```
Expected: clean (no diagnostics).

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/context.rs
git commit -m "feat(core): SMA-403 add runcontext ephemeral constructor and dependency setters"
```

---

## Task 2: Verify the rustdoc doctests compile

The doctests were authored inline in Task 1 (on `ephemeral`, `ephemeral_shared`, and `with_session`). This task confirms they are compiler-checked — mdBook code blocks are **not** compiled by CI, so these rustdoc doctests are the only compiled examples of the new API.

**Files:**
- Verify: `crates/paigasus-helikon-core/src/context.rs` (doctests added in Task 1)

- [ ] **Step 1: Run the doctests**

Run: `cargo test -p paigasus-helikon-core --doc 2>&1 | tail -20`
Expected: PASS — including `src/context.rs - context::RunContext::<Ctx>::ephemeral (line …)`, `… ephemeral_shared …`, and `… with_session …`.

- [ ] **Step 2: Confirm the docs gate is clean**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps 2>&1 | tail -20`
Expected: builds with no warnings (no broken intra-doc links from the new `[`TracerHandle::builder`]` reference).

- [ ] **Step 3: No commit needed** if Task 1 already committed the doctests. If the doctests were adjusted here, commit:

```bash
git add crates/paigasus-helikon-core/src/context.rs
git commit -m "docs(core): SMA-403 add ephemeral doctests to RunContext"
```

---

## Task 3: Migrate facade examples + `otel_spans` test

**Files:**
- Modify: `crates/paigasus-helikon/examples/budget_assistant_anthropic.rs`, `budget_assistant_openai.rs`, `multi_agent_triage.rs`, `structured_output.rs`, `streaming_console.rs`, `langfuse_tracing.rs`
- Modify: `crates/paigasus-helikon/tests/otel_spans.rs` (2 sites)

- [ ] **Step 1: Apply the bare-default transformation**

In `budget_assistant_anthropic.rs`, `budget_assistant_openai.rs`, `multi_agent_triage.rs`, `structured_output.rs`, `streaming_console.rs`, and both sites in `otel_spans.rs`, replace this block:

```rust
let ctx: RunContext<()> = RunContext::new(
    Arc::new(()),
    Arc::new(MemorySession::new()),
    HookRegistry::<()>::new(),
    TracerHandle::default(),
    CancellationToken::new(),
);
```

with:

```rust
let ctx: RunContext<()> = RunContext::ephemeral(());
```

(Some sites write `HookRegistry::new()` without the turbofish or bind to `run_ctx` instead of `ctx` — preserve the existing binding name and annotation; only the `RunContext::new(...)` expression changes.)

- [ ] **Step 2: Apply the custom-tracer transformation to `langfuse_tracing.rs`**

Replace:

```rust
let ctx = RunContext::new(
    Arc::new(()),
    Arc::new(NoopSession) as Arc<dyn Session>,
    HookRegistry::<()>::new(),
    TracerHandle::builder()
        .with_session_id("demo-session")
        .with_user_id("demo-user")
        .with_tag("example")
        .with_tag("sma-322")
        .build(),
    CancellationToken::new(),
);
```

with:

```rust
let ctx = RunContext::ephemeral(())
    .with_session(Arc::new(NoopSession))
    .with_tracer(
        TracerHandle::builder()
            .with_session_id("demo-session")
            .with_user_id("demo-user")
            .with_tag("example")
            .with_tag("sma-322")
            .build(),
    );
```

- [ ] **Step 3: Build and test, then clean orphaned imports**

Run:
```bash
cargo build -p paigasus-helikon --examples --all-features 2>&1 | tail -30
cargo test -p paigasus-helikon --test otel_spans --all-features 2>&1 | tail -20
cargo clippy -p paigasus-helikon --all-features --all-targets -- -D warnings 2>&1 | tail -40
```
Expected: build + test PASS. Clippy will report `unused_imports` for now-orphaned names (per file, typically some subset of `MemorySession`, `HookRegistry`, `TracerHandle`, `CancellationToken`, and — in `langfuse_tracing.rs` after dropping the cast — the `Session` trait). **Delete each import clippy names**, then re-run the clippy command until clean. In `langfuse_tracing.rs`, `TracerHandle` and `Arc` and `NoopSession`/`Session` are still used, so only truly-unused names go.

- [ ] **Step 4: Format**

Run: `cargo fmt --all`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon/examples crates/paigasus-helikon/tests/otel_spans.rs
git commit -m "refactor(facade): SMA-403 migrate examples and otel_spans to ephemeral"
```

---

## Task 4: Migrate `-tools` examples + integration tests

**Files:**
- Modify: `crates/paigasus-helikon-tools/examples/web_research.rs`, `os_sandbox_demo.rs`, `explore_sandbox.rs`
- Modify: `crates/paigasus-helikon-tools/tests/sandbox.rs`, `bash.rs`, `sandbox_navigation.rs` (2 sites)

- [ ] **Step 1: Bare-default sites (`os_sandbox_demo.rs`, `explore_sandbox.rs`, and the three test files)**

Replace each:

```rust
let run_ctx: RunContext<()> = RunContext::new(
    Arc::new(()),
    Arc::new(MemorySession::new()),
    HookRegistry::new(),
    TracerHandle::default(),
    CancellationToken::new(),
);
```

with:

```rust
let run_ctx: RunContext<()> = RunContext::ephemeral(());
```

(In `sandbox.rs` / `bash.rs` / `sandbox_navigation.rs` the very next line is `run_ctx.to_tool_context()` — leave it unchanged.)

- [ ] **Step 2: Policy-chain site (`web_research.rs`)**

Replace:

```rust
let ctx: RunContext<()> = RunContext::new(
    Arc::new(()),
    Arc::new(MemorySession::new()),
    HookRegistry::<()>::new(),
    TracerHandle::default(),
    CancellationToken::new(),
)
.with_permission_policy(Arc::new(AllowWebTools));
```

with:

```rust
let ctx: RunContext<()> = RunContext::ephemeral(())
    .with_permission_policy(Arc::new(AllowWebTools));
```

- [ ] **Step 3: Build, test, and clean orphaned imports**

Run:
```bash
cargo build -p paigasus-helikon-tools --examples --all-features 2>&1 | tail -30
cargo test -p paigasus-helikon-tools --all-features 2>&1 | tail -25
cargo clippy -p paigasus-helikon-tools --all-features --all-targets -- -D warnings 2>&1 | tail -40
```
Expected: build + tests PASS. Delete every `unused_imports` name clippy reports (per file: subset of `MemorySession`, `HookRegistry`, `TracerHandle`, `CancellationToken`), then re-run clippy until clean. `Arc` stays in `web_research.rs` (used by `Arc::new(AllowWebTools)`).

> Note: the sandbox tests run the Seatbelt backend only on macOS; on other platforms they are `#[cfg]`-gated. Run on macOS to exercise them, but the migration is platform-independent.

- [ ] **Step 4: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-tools/examples crates/paigasus-helikon-tools/tests
git commit -m "refactor(tools): SMA-403 migrate examples and tests to ephemeral"
```

---

## Task 5: Migrate `-core` test helper + integration tests

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/common/mod.rs` (`noop_run_context`)
- Modify: `crates/paigasus-helikon-core/tests/handoff.rs`, `failure_slot.rs`, `subagent_propagation.rs` (6 sites), `agent_as_tool.rs` (2 sites), `workflow_sequential.rs`, `workflow_parallel.rs`, `workflow_loop.rs`, `workflow_pipeline.rs`, `workflow_tracing.rs`

- [ ] **Step 1: Migrate the shared helper `noop_run_context`**

In `tests/common/mod.rs`, replace:

```rust
pub fn noop_run_context<Ctx>() -> RunContext<Ctx>
where
    Ctx: Default + Send + Sync + 'static,
{
    RunContext::new(
        Arc::new(Ctx::default()),
        Arc::new(NoopSession) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
}
```

with:

```rust
pub fn noop_run_context<Ctx>() -> RunContext<Ctx>
where
    Ctx: Default + Send + Sync + 'static,
{
    RunContext::ephemeral(Ctx::default()).with_session(Arc::new(NoopSession))
}
```

- [ ] **Step 2: Migrate the integration-test call sites**

Apply per the site's session argument (all other args are defaults):

- **Bare** (2nd arg `Arc::new(MemorySession::new())`): replace the whole `RunContext::new(<5 args>)` expression with `RunContext::ephemeral(())`. Applies to `handoff.rs`; all six sites in `subagent_propagation.rs` (lines ~65, 123, 165, 226, 301, 348 — including the `std::sync::Arc::new(...)` fully-qualified ones); `agent_as_tool.rs` site ~16; and all of `workflow_sequential.rs`, `workflow_parallel.rs`, `workflow_loop.rs`, `workflow_pipeline.rs`, `workflow_tracing.rs`.
- **NoopSession** (`Arc::new(NoopSession) as Arc<dyn Session>`): replace with `RunContext::ephemeral(()).with_session(Arc::new(NoopSession))`. Applies to `failure_slot.rs`.
- **Custom session expression `S`** (e.g. `parent_session.clone() as Arc<dyn Session>`): replace with `RunContext::ephemeral(()).with_session(S)` (drop the `as Arc<dyn Session>` cast — it coerces at the argument). Applies to `agent_as_tool.rs` site ~68 → `RunContext::ephemeral(()).with_session(parent_session.clone())`.

**Preserve any trailing `.with_*` chain** after the constructor (some `subagent_propagation.rs` sites append `.with_permission_mode(...)` / `.with_guard_rules(...)` — keep those exactly).

- [ ] **Step 3: Build, test, and clean orphaned imports**

Run:
```bash
cargo test -p paigasus-helikon-core --all-features 2>&1 | tail -30
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings 2>&1 | tail -50
```
Expected: all integration tests PASS. Per file, delete the `unused_imports` clippy names (subset of `MemorySession`, `HookRegistry`, `TracerHandle`, `CancellationToken`, `Session`). `Arc` and `NoopSession` remain where still referenced (`failure_slot.rs`, `agent_as_tool.rs`, `common/mod.rs`). Re-run clippy until clean.

- [ ] **Step 4: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/tests
git commit -m "test(core): SMA-403 migrate core test helpers and integration tests to ephemeral"
```

---

## Task 6: Migrate `-runtime-tokio` test helpers

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs` (5 helper functions, lines ~194–252)

- [ ] **Step 1: Replace the five helper bodies**

```rust
pub fn noop_run_context() -> RunContext<()> {
    RunContext::ephemeral(()).with_session(Arc::new(NoopSession))
}

pub fn run_context_with_cancel(cancel: CancellationToken) -> RunContext<()> {
    RunContext::ephemeral(())
        .with_session(Arc::new(NoopSession))
        .with_cancel(cancel)
}

pub fn run_context_with_cancel_and_hooks(
    cancel: CancellationToken,
    hooks: Vec<Arc<dyn Hook<()>>>,
) -> RunContext<()> {
    let mut registry = HookRegistry::new();
    for h in hooks {
        registry.push(h);
    }
    RunContext::ephemeral(())
        .with_session(Arc::new(NoopSession))
        .with_hooks(registry)
        .with_cancel(cancel)
}

pub fn run_context_with_session(session: Arc<dyn Session>) -> RunContext<()> {
    RunContext::ephemeral(()).with_session(session)
}

pub fn run_context_with_session_and_cancel(
    session: Arc<dyn Session>,
    cancel: CancellationToken,
) -> RunContext<()> {
    RunContext::ephemeral(())
        .with_session(session)
        .with_cancel(cancel)
}
```

- [ ] **Step 2: Build, test, clean imports**

Run:
```bash
cargo test -p paigasus-helikon-runtime-tokio --all-features 2>&1 | tail -25
cargo clippy -p paigasus-helikon-runtime-tokio --all-features --all-targets -- -D warnings 2>&1 | tail -30
```
Expected: tests PASS. Clippy flags `TracerHandle` as unused (it was only used by `TracerHandle::default()` in these helpers) — delete it from the `use paigasus_helikon_core::{…}` list. `HookRegistry`, `CancellationToken`, `Hook`, `Session`, `Arc` are all still used (registry building, parameter types) — keep them. Re-run until clean.

> Windows flake note: `sessions-sqlite`-style "database is locked" flakes don't apply here, but if a `runtime-tokio` test flakes on a non-required Windows signal job, that's unrelated to this change.

- [ ] **Step 3: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs
git commit -m "test(runtime-tokio): SMA-403 migrate test helpers to ephemeral"
```

---

## Task 7: Migrate `-mcp` test support helper

**Files:**
- Modify: `crates/paigasus-helikon-mcp/tests/support/mod.rs` (`tool_ctx_with_cancel`, lines ~100–112)

- [ ] **Step 1: Replace the helper body**

```rust
pub fn tool_ctx_with_cancel(
    cancel: paigasus_helikon_core::CancellationToken,
) -> paigasus_helikon_core::ToolContext<()> {
    paigasus_helikon_core::RunContext::ephemeral(())
        .with_cancel(cancel)
        .to_tool_context()
}
```

(This site uses fully-qualified paths, not `use` imports, so no import cleanup is needed for it. Confirm no other function in the file referenced the removed `MemorySession`/`HookRegistry`/`TracerHandle` paths via a `use`.)

- [ ] **Step 2: Build, test, lint**

Run:
```bash
cargo test -p paigasus-helikon-mcp --all-features 2>&1 | tail -25
cargo clippy -p paigasus-helikon-mcp --all-features --all-targets -- -D warnings 2>&1 | tail -30
```
Expected: tests PASS, clippy clean (delete any `unused_imports` it names).

- [ ] **Step 3: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-mcp/tests/support/mod.rs
git commit -m "test(mcp): SMA-403 migrate test support helper to ephemeral"
```

---

## Task 8: Update mdBook examples

`RunContext::new` appears across 9 pages under `docs/book/src/`. Per-page judgment: switch to `ephemeral` where construction is **incidental setup boilerplate**; keep `new` where the page is **teaching the constructor**.

**Files (modify as judged):**
- `docs/book/src/getting-started/quickstart.md` — incidental → `ephemeral`
- `docs/book/src/concepts/multi-agent-patterns.md` — incidental → `ephemeral`
- `docs/book/src/concepts/structured-output-builder.md` — incidental → `ephemeral`
- `docs/book/src/concepts/agent-loop.md` — incidental → `ephemeral`
- `docs/book/src/concepts/permissions-guardrails-hooks.md` — likely chains `.with_*`; convert the `new(...)` head to `ephemeral(())` and keep the chain
- `docs/book/src/concepts/observability-evaluation.md` — if it shows a populated tracer, use `ephemeral(()).with_tracer(...)`; else `ephemeral(())`
- `docs/book/src/introduction.md` — incidental → `ephemeral`
- `docs/book/src/concepts/core-primitives.md` — **construction-teaching**: keep a `new(...)` example showing the five arguments, and add a one-line note that `RunContext::ephemeral(())` is the shorthand for the all-defaults case
- `docs/book/src/concepts/sessions.md` — **construction-teaching** for custom sessions: keep `new(...)` (or show `ephemeral(()).with_session(...)` if that reads better for the page's point)

- [ ] **Step 1: Edit each page**

For an incidental page, replace the verbose block:
```rust
let ctx: RunContext<()> = RunContext::new(
    Arc::new(()),
    Arc::new(MemorySession::new()),
    HookRegistry::new(),
    TracerHandle::default(),
    CancellationToken::new(),
);
```
with:
```rust
let ctx: RunContext<()> = RunContext::ephemeral(());
```
and remove any now-unreferenced `use` lines in that page's snippet so the prose stays accurate. For the two construction-teaching pages, make the per-page call described above instead of a blanket replace.

- [ ] **Step 2: Build the book**

Run: `mdbook build docs/book 2>&1 | tail -20`
Expected: builds clean. `[output.linkcheck] warning-policy = "error"`, so any broken link fails — fix before proceeding. (mdBook code blocks are not compiled, so visually re-read each changed snippet against the Task 1 API names.)

- [ ] **Step 3: Commit**

```bash
git add docs/book/src
git commit -m "docs(docs): SMA-403 use ephemeral in mdBook examples"
```

---

## Task 9: Full local CI gate + handoff

**Files:** none (verification only).

- [ ] **Step 1: Run the full CI gate (matches `.github/workflows/ci.yml`)**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
mdbook build docs/book
```
Expected: every command exits 0. If clippy reports a missed orphaned import in any file, delete it and re-run.

- [ ] **Step 2: Confirm the commit range is convco-clean**

Run: `convco check origin/main..HEAD`
Expected: PASS — all commit types/scopes (`feat(core)`, `docs(core)`, `refactor(facade)`, `refactor(tools)`, `test(core)`, `test(runtime-tokio)`, `test(mcp)`, `docs(docs)`) are in the `.versionrc` allowlist.

- [ ] **Step 3: Push and open the PR**

```bash
git push -u origin feature/sma-403-runcontext-convenience-constructor-builder-ephemeral-to-cut
gh pr create \
  --title "feat(core): SMA-403 add runcontext ephemeral constructor and dependency setters" \
  --body "$(cat <<'EOF'
Adds `RunContext::ephemeral(Ctx)` + `ephemeral_shared(Arc<Ctx>)` and the four
dependency setters (`with_session`/`with_hooks`/`with_tracer`/`with_cancel`),
collapsing the 5-arg `RunContext::new` boilerplate to one line. Migrates all
default-ish call sites (facade + tools examples and tests, shared test helpers,
core integration tests) and updates the mdBook examples.

Spec: docs/superpowers/specs/2026-06-20-sma-403-runcontext-ephemeral-design.md

Closes SMA-403.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

The PR title satisfies both `pr-title.yml` rules: full `type(scope):` prefix and a lowercase subject after the `SMA-403 ` token. release-plz will patch-bump `-core` and cascade the facade on merge — **no manual core/facade bump** is needed (consumers are in-repo examples/tests that never publish).

---

## Notes for the implementer

- **No new `RunContext` field is added** — the four setters write existing fields already copied at `handoff_child` / `subagent_child` / `to_tool_context` / `agent_as_tool`. Do not add propagation code; there is nothing new to propagate.
- **Do not modify `RunContext::new`** or the existing `context.rs` unit tests — they remain the canonical coverage for `new` and the clone sites.
- **`with_session` drops `as Arc<dyn Session>` casts** because the argument position coerces `Arc<Concrete>` → `Arc<dyn Session>`.
- **Commits are signed via a 1Password SSH key.** If a commit fails with "failed to fill whole buffer", the vault is locked — ask the user to unlock it; do not bypass signing.
- **Never `git add -A`** — `.env` / `.claude` are untracked-but-not-ignored. Use the explicit paths shown in each commit step.
```
