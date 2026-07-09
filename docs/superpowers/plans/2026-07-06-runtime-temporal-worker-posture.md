# SMA-455 Worker-Side Posture + Ctx-Seed + Heartbeats — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a `paigasus-helikon-runtime-temporal` worker configure the security posture of the `RunContext` its activities fabricate, let a client optionally attach a serializable `Ctx` seed that crosses the client→worker boundary, and add opt-in heartbeat-aware model/tool activities — all while keeping v0 behavior byte-identical when unused.

**Architecture:** All work is in `paigasus-helikon-runtime-temporal`; **no `paigasus-helikon-core` change** (every posture knob is an already-`pub` `RunContext::with_*` method). Posture is worker-static (a `WorkerPosture<Ctx>` bundle applied in `TypedRuntime::run_context`); the seed is a type-erased `serde_json::Value` threaded through `WorkflowInput` into the `render_instructions`/`invoke_tool` activities and rehydrated by a **fallible** worker factory; heartbeats add a ticker in `call_model`/`invoke_tool` plus `heartbeat_timeout` on their `ActivityOptions`.

**Tech Stack:** Rust (edition per workspace, MSRV 1.94), `temporalio-sdk` / `temporalio-workflow` / `temporalio-common` 0.5.0 (`ActivityOptions` uses `bon::Builder`; `ActivityContext::{record_heartbeat(Vec<Payload>), cancelled()}`), `serde`/`serde_json`, `tokio`, `async-trait`, `thiserror`.

## Global Constraints

- **MSRV `1.94`**; workspace inheritance mandatory — per-crate `Cargo.toml` sets only crate-specific bits. No new third-party deps unless pinned in root `[workspace.dependencies]` (this plan needs none).
- **`missing_docs = warn` + doc-coverage gate ≥80%:** every new `pub` item needs a `///` doc comment.
- **Full CI gate parity (run before the final commit):**
  `cargo fmt --all -- --check` · `cargo clippy --workspace --all-features --all-targets -- -D warnings` · `cargo test --workspace --all-features` · `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`.
- **Per-task hygiene (memory):** run `cargo fmt --all` and `cargo clippy -p paigasus-helikon-runtime-temporal --all-targets -- -D warnings` **before every commit**; the pre-commit hook is a no-op, pre-push is the safety net — don't rely on it.
- **Run cargo synchronously, in the foreground.** Do not background `cargo test`/`build`; do not end a task turn until the command shows a terminal pass/fail.
- **Commits are 1Password-signed.** If a commit fails with "failed to fill whole buffer", the vault is locked — stop and ask the user to unlock; never bypass signing.
- **Commit convention:** `feat(runtime-temporal): SMA-455 <lowercase subaject>` for code, `docs(runtime-temporal): SMA-455 …` for docs (`runtime-temporal`, `spec`, `plan`, `docs` are all allowed convco scopes). Stage new files by **explicit path** — never `git add -A` (`.env`/`.claude` are untracked-but-not-ignored).
- **No existing public signature is removed.** `with_ctx(Fn() -> Ctx)` keeps its exact signature and meaning.

---

## File map

| File | Responsibility | Change |
|------|----------------|--------|
| `crates/paigasus-helikon-runtime-temporal/src/worker.rs` | Worker builder | Add `WorkerPosture<Ctx>`, `CtxSeedError`, `.posture()`, `.with_seeded_ctx()`, `.try_with_seeded_ctx()`, `.heartbeat_interval()`; fallible ctx-factory slot |
| `crates/paigasus-helikon-runtime-temporal/src/activities.rs` | Activity layer | `posture` + fallible seeded factory + `heartbeat_interval` on `TypedRuntime`/`AgentActivities`; `run_context(seed, cancel) -> Result`; thread `ctx_seed`; rewrite the cancel/heartbeat race |
| `crates/paigasus-helikon-runtime-temporal/src/workflow.rs` | Durable workflow | Thread `input.ctx_seed` into activity tuples; set `heartbeat_timeout` on model/tool `ActivityOptions` |
| `crates/paigasus-helikon-runtime-temporal/src/payloads.rs` | Wire types | `WorkflowInput.ctx_seed: Option<Value>` (`#[serde(default)]`) |
| `crates/paigasus-helikon-runtime-temporal/src/runner.rs` | Client runner | Private `ctx_seed` on `TemporalRunnerConfig` + `with_ctx_seed`; thread into `WorkflowInput` |
| `crates/paigasus-helikon-runtime-temporal/src/lib.rs` | Crate docs | Rewrite the "Worker-Side Posture and Security Boundary" section; upgrade/heartbeat notes |
| `crates/paigasus-helikon-runtime-temporal/README.md`, `CHANGELOG.md` | Crate docs | Usage + changelog |
| `docs/superpowers/specs/2026-07-05-runtime-temporal-agentcore-design.md` | Prior spec | §5.8 as-built: mark landed in SMA-455 |
| `docs/book/src/concepts/runtimes.md` | Book | Align posture description |

Tests live inline in each `src/*.rs` `#[cfg(test)] mod tests` (matching the crate's existing convention), except the request-scoped-policy composition test which goes in `activities.rs`.

---

## Task 1: `WorkerPosture<Ctx>` and wiring it into the activity path

Adds the posture bundle and applies it where the activity fabricates its `RunContext`. `run_context` stays infallible and seed-less here (seed lands in Task 2), so this task is a self-contained, testable "worker posture is now configurable" deliverable.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/worker.rs` (add `WorkerPosture`, `posture` field + `.posture()`; thread into `build()`)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activities.rs` (`TypedRuntime.posture`; `run_context` applies it; `build_activities` gains a `posture` param)
- Test: inline `#[cfg(test)]` in both files

**Interfaces:**
- Produces:
  - `pub struct WorkerPosture<Ctx: Send + Sync + 'static>` with `impl Default`, and chainable setters `with_permission_mode(PermissionMode)`, `with_deny_rules(Vec<DenyRule>)`, `with_allow_rules(Vec<AllowRule>)`, `with_guard_rules(Vec<GuardRule>)`, `with_permission_policy(Arc<dyn PermissionPolicy<Ctx>>)`, `with_approval_handler(Arc<dyn ApprovalHandler>)`, `without_default_guards()`, `without_output_redaction()`, `with_extra_secrets(Vec<String>)`.
  - `pub(crate) fn WorkerPosture::apply(&self, ctx: RunContext<Ctx>) -> RunContext<Ctx>`.
  - `TemporalAgentWorkerBuilder::posture(self, WorkerPosture<Ctx>) -> Self`.
  - `build_activities(registry, ctx_factory, posture)` (new 3rd param).

- [ ] **Step 1: Write the failing test — default posture is a no-op** (in `worker.rs`'s `#[cfg(test)] mod tests`)

```rust
#[test]
fn worker_posture_default_matches_ephemeral_defaults() {
    use paigasus_helikon_core::RunContext;
    let ctx = WorkerPosture::<()>::default().apply(RunContext::ephemeral(()));
    let bare: RunContext<()> = RunContext::ephemeral(());
    assert_eq!(ctx.permission_mode(), bare.permission_mode());
    assert_eq!(ctx.default_guards(), true);
    assert_eq!(ctx.redact_output(), true);
    assert!(ctx.deny_rules().is_empty());
    assert!(ctx.allow_rules().is_empty());
    assert!(ctx.guard_rules().is_empty());
    assert!(ctx.extra_secrets().is_empty());
    assert!(ctx.permission_policy().is_none());
    assert!(ctx.approval_handler().is_none());
}
```

- [ ] **Step 2: Write the failing test — setters install each knob**

```rust
#[test]
fn worker_posture_applies_each_knob() {
    use paigasus_helikon_core::{DenyRule, PermissionMode, RunContext};
    let posture = WorkerPosture::<()>::default()
        .with_permission_mode(PermissionMode::Plan)
        .with_deny_rules(vec![DenyRule::tool("Bash")])
        .with_extra_secrets(vec!["sk-123".to_owned()])
        .without_default_guards()
        .without_output_redaction();
    let ctx = posture.apply(RunContext::ephemeral(()));
    assert_eq!(ctx.permission_mode(), PermissionMode::Plan);
    assert_eq!(ctx.deny_rules().len(), 1);
    assert_eq!(ctx.extra_secrets(), ["sk-123".to_owned()]);
    assert!(!ctx.default_guards());
    assert!(!ctx.redact_output());
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-temporal worker_posture -- --nocapture`
Expected: FAIL to compile (`WorkerPosture` not found).

- [ ] **Step 4: Implement `WorkerPosture` + `apply`** — add near the top of `worker.rs` (after the `use` block). Add the needed imports to the existing `use paigasus_helikon_core::{...}` line: `AllowRule, ApprovalHandler, DenyRule, GuardRule, PermissionMode, PermissionPolicy, RunContext`.

```rust
/// Worker-side security posture applied to every `RunContext` the durable
/// activities fabricate. `Default` reproduces the crate's v0 fixed defaults
/// (`PermissionMode::Default`, built-in destructive guards on, output redaction
/// on, no custom rules / policy / approval handler / extra secrets).
pub struct WorkerPosture<Ctx: Send + Sync + 'static> {
    permission_mode: PermissionMode,
    deny_rules: Vec<DenyRule>,
    allow_rules: Vec<AllowRule>,
    guard_rules: Vec<GuardRule>,
    permission_policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,
    approval_handler: Option<Arc<dyn ApprovalHandler>>,
    default_guards: bool,
    redact_output: bool,
    extra_secrets: Vec<String>,
}

impl<Ctx: Send + Sync + 'static> Default for WorkerPosture<Ctx> {
    fn default() -> Self {
        Self {
            permission_mode: PermissionMode::default(),
            deny_rules: Vec::new(),
            allow_rules: Vec::new(),
            guard_rules: Vec::new(),
            permission_policy: None,
            approval_handler: None,
            default_guards: true,
            redact_output: true,
            extra_secrets: Vec::new(),
        }
    }
}

impl<Ctx: Send + Sync + 'static> WorkerPosture<Ctx> {
    /// Set the permission mode the activities enforce (tighten-only from `Default`).
    pub fn with_permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = mode;
        self
    }
    /// Install deny rules (evaluated before mode; override even `Bypass`).
    pub fn with_deny_rules(mut self, rules: Vec<DenyRule>) -> Self {
        self.deny_rules = rules;
        self
    }
    /// Install allow rules (positive short-circuit in any mode).
    pub fn with_allow_rules(mut self, rules: Vec<AllowRule>) -> Self {
        self.allow_rules = rules;
        self
    }
    /// Install user guard rules (evaluated before mode; may ask or deny).
    pub fn with_guard_rules(mut self, rules: Vec<GuardRule>) -> Self {
        self.guard_rules = rules;
        self
    }
    /// Install the `canUseTool` permission policy. It can read the per-run
    /// (seeded) `RunContext::user_ctx` for request-scoped decisions.
    pub fn with_permission_policy(mut self, policy: Arc<dyn PermissionPolicy<Ctx>>) -> Self {
        self.permission_policy = Some(policy);
        self
    }
    /// Install the approval handler that resolves `AskUser` / guard `Ask` decisions.
    pub fn with_approval_handler(mut self, handler: Arc<dyn ApprovalHandler>) -> Self {
        self.approval_handler = Some(handler);
        self
    }
    /// Disable the always-on built-in destructive guard set (power-user opt-out).
    pub fn without_default_guards(mut self) -> Self {
        self.default_guards = false;
        self
    }
    /// Disable automatic secret redaction of tool output. Note: unredacted tool
    /// output then enters permanent Temporal history.
    pub fn without_output_redaction(mut self) -> Self {
        self.redact_output = false;
        self
    }
    /// Add extra secret values to redact from tool output, beyond the env set.
    pub fn with_extra_secrets(mut self, secrets: Vec<String>) -> Self {
        self.extra_secrets = secrets;
        self
    }

    /// Apply this posture onto a freshly fabricated `RunContext`.
    ///
    /// NB: this is the **fifth** hand-copy of core's nine-field permission
    /// bundle (see `RunContext`'s fields and core's `pub(crate) PermissionFields`).
    /// `PermissionFields` cannot be reused across the crate boundary. If core
    /// gains a tenth posture knob, add it here too — the default-equivalence
    /// unit test enumerates every field to catch the omission.
    pub(crate) fn apply(&self, ctx: RunContext<Ctx>) -> RunContext<Ctx> {
        let mut ctx = ctx
            .with_permission_mode(self.permission_mode)
            .with_deny_rules(self.deny_rules.clone())
            .with_allow_rules(self.allow_rules.clone())
            .with_guard_rules(self.guard_rules.clone())
            .with_extra_secrets(self.extra_secrets.clone());
        if let Some(p) = &self.permission_policy {
            ctx = ctx.with_permission_policy(Arc::clone(p));
        }
        if let Some(h) = &self.approval_handler {
            ctx = ctx.with_approval_handler(Arc::clone(h));
        }
        if !self.default_guards {
            ctx = ctx.without_default_guards();
        }
        if !self.redact_output {
            ctx = ctx.without_output_redaction();
        }
        ctx
    }
}
```

- [ ] **Step 5: Run the two tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-temporal worker_posture -- --nocapture`
Expected: PASS (2 tests).

- [ ] **Step 6: Wire `posture` into the builder and activity path.** In `worker.rs`:
  - Add `posture: WorkerPosture<Ctx>` to `TemporalAgentWorkerBuilder<Ctx>` and initialize it to `WorkerPosture::default()` in `TemporalAgentWorker::builder`.
  - Add the setter:
    ```rust
    /// Set the worker-side security posture applied to every fabricated
    /// `RunContext`. Defaults to `WorkerPosture::default()` (v0 fixed defaults).
    pub fn posture(mut self, posture: WorkerPosture<Ctx>) -> Self {
        self.posture = posture;
        self
    }
    ```
  - In `build()`, change the `build_activities` call to pass the posture (move it out of `self` before `self.registry` is wrapped): `let activities = activities::build_activities(Arc::clone(&agent_registry), Arc::clone(&ctx_factory), self.posture);`

  In `activities.rs`:
  - Add `posture: crate::worker::WorkerPosture<Ctx>` to `struct TypedRuntime<Ctx>`.
  - Change `run_context` to apply it (still infallible/seedless in this task):
    ```rust
    fn run_context(&self, cancel: CancellationToken) -> RunContext<Ctx> {
        let ctx = RunContext::ephemeral((self.ctx_factory)()).with_cancel(cancel);
        self.posture.apply(ctx)
    }
    ```
  - Change `build_activities` to accept and store the posture:
    ```rust
    pub(crate) fn build_activities<Ctx: Send + Sync + 'static>(
        registry: Arc<HashMap<String, Arc<DurableAgentDef<Ctx>>>>,
        ctx_factory: Arc<dyn Fn() -> Ctx + Send + Sync>,
        posture: crate::worker::WorkerPosture<Ctx>,
    ) -> AgentActivities {
        AgentActivities {
            runtime: Arc::new(TypedRuntime { registry, ctx_factory, posture }),
        }
    }
    ```

- [ ] **Step 7: Write the failing test — a posture reaches the fabricated context** (in `activities.rs`'s `#[cfg(test)] mod tests`)

```rust
#[test]
fn typed_runtime_run_context_applies_posture() {
    use crate::worker::WorkerPosture;
    use paigasus_helikon_core::{DenyRule, PermissionMode};
    let rt = TypedRuntime::<()> {
        registry: Arc::new(HashMap::new()),
        ctx_factory: Arc::new(|| ()),
        posture: WorkerPosture::default()
            .with_permission_mode(PermissionMode::Plan)
            .with_deny_rules(vec![DenyRule::tool("Bash")]),
    };
    let ctx = rt.run_context(CancellationToken::new());
    assert_eq!(ctx.permission_mode(), PermissionMode::Plan);
    assert_eq!(ctx.deny_rules().len(), 1);
}
```

- [ ] **Step 8: Run the whole crate's tests to verify everything compiles and passes**

Run: `cargo test -p paigasus-helikon-runtime-temporal`
Expected: PASS (all existing tests + the 3 new ones). Existing `worker.rs` tests still call `.with_ctx(|| ())` unchanged.

- [ ] **Step 9: fmt + clippy, then commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-temporal/src/worker.rs crates/paigasus-helikon-runtime-temporal/src/activities.rs
git commit -m "feat(runtime-temporal): SMA-455 add WorkerPosture and apply it in fabricated RunContext"
```

---

## Task 2: Fallible seeded ctx factory (`run_context` becomes fallible)

Re-points the internal ctx-factory slot to `Fn(Option<Value>) -> Result<Ctx, CtxSeedError>`, makes `run_context` take a seed and return a `Result`, and maps a factory error to a **non-retryable** activity failure (the BLOCKER fix). The `#[activity]` wrappers pass `None` for now — the real seed is plumbed in Task 3. Includes the §4.6 request-scoped-policy composition test (all pieces exist here).

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/worker.rs` (`CtxSeedError`, fallible factory slot, `with_ctx` rewrap, `with_seeded_ctx`, `try_with_seeded_ctx`)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activities.rs` (`ctx_factory` type; `run_context(seed, cancel) -> Result`; `DurableAgentRuntime::{render_instructions, invoke_tool}` gain `ctx_seed`; wrappers pass `None`)
- Test: inline in both files

**Interfaces:**
- Consumes: `WorkerPosture::apply`, `TypedRuntime` (Task 1).
- Produces:
  - `pub struct CtxSeedError` (Display/Error) with `pub(crate) fn new(impl Into<String>)`.
  - Factory slot type `Arc<dyn Fn(Option<serde_json::Value>) -> Result<Ctx, CtxSeedError> + Send + Sync>`.
  - `TemporalAgentWorkerBuilder::with_seeded_ctx(impl Fn(Option<Value>) -> Ctx + Send + Sync + 'static)`.
  - `TemporalAgentWorkerBuilder::try_with_seeded_ctx(impl Fn(Option<Value>) -> Result<Ctx, E> + …)` where `E: Display`.
  - `TypedRuntime::run_context(&self, Option<Value>, CancellationToken) -> Result<RunContext<Ctx>, ActivityError>`.
  - `DurableAgentRuntime::{render_instructions, invoke_tool}` now take `ctx_seed: Option<serde_json::Value>`.
  - `build_activities`'s `ctx_factory` param is now the fallible slot type.

- [ ] **Step 1: Write the failing test — a rejected seed is a non-retryable activity error** (in `activities.rs` tests)

```rust
#[tokio::test]
async fn run_context_seed_error_is_non_retryable() {
    use crate::worker::{CtxSeedError, WorkerPosture};
    let rt = TypedRuntime::<()> {
        registry: Arc::new(HashMap::new()),
        ctx_factory: Arc::new(|_seed| Err(CtxSeedError::new("bad seed"))),
        posture: WorkerPosture::default(),
    };
    let err = rt
        .run_context(Some(serde_json::json!({"x": 1})), CancellationToken::new())
        .expect_err("a rejected seed must be an Err");
    match err {
        ActivityError::Application(app) => assert!(
            app.is_non_retryable(),
            "seed-rejection activity errors must be non-retryable"
        ),
        other => panic!("expected ActivityError::Application, got {other:?}"),
    }
}
```

- [ ] **Step 2: Write the failing test — the §4.6 composition (worker policy reads the seeded Ctx)** (in `activities.rs` tests)

```rust
#[tokio::test]
async fn seeded_ctx_feeds_request_scoped_policy() {
    use crate::worker::WorkerPosture;
    use paigasus_helikon_core::{
        PermissionDecision, PermissionPolicy, RunContext, ToolEffect,
    };

    struct Tenant { name: String }
    struct TenantPolicy;
    #[async_trait]
    impl PermissionPolicy<Tenant> for TenantPolicy {
        async fn check(
            &self,
            ctx: &RunContext<Tenant>,
            _tool: &str,
            _args: &serde_json::Value,
        ) -> PermissionDecision {
            if ctx.user_ctx().name == "acme" {
                PermissionDecision::Allow
            } else {
                PermissionDecision::Deny { reason: "not acme".to_owned() }
            }
        }
    }

    let rt = TypedRuntime::<Tenant> {
        registry: Arc::new(HashMap::new()),
        ctx_factory: Arc::new(|seed| {
            let name = seed
                .and_then(|v| v.get("tenant").and_then(|t| t.as_str()).map(str::to_owned))
                .unwrap_or_default();
            Ok(Tenant { name })
        }),
        posture: WorkerPosture::default().with_permission_policy(Arc::new(TenantPolicy)),
    };

    let acme = rt
        .run_context(Some(serde_json::json!({"tenant": "acme"})), CancellationToken::new())
        .expect("factory ok");
    assert!(matches!(
        acme.authorize_tool("AnyTool", ToolEffect::ReadOnly, &serde_json::json!({})).await,
        PermissionDecision::Allow
    ));

    let other = rt
        .run_context(Some(serde_json::json!({"tenant": "evil"})), CancellationToken::new())
        .expect("factory ok");
    assert!(matches!(
        other.authorize_tool("AnyTool", ToolEffect::ReadOnly, &serde_json::json!({})).await,
        PermissionDecision::Deny { .. }
    ));
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-temporal run_context_seed_error seeded_ctx_feeds -- --nocapture`
Expected: FAIL to compile (`CtxSeedError` absent; `run_context` arity/return mismatch).

- [ ] **Step 4: Add `CtxSeedError` + fallible factory setters** in `worker.rs`. Add `use std::fmt;` if needed (or use `thiserror`). Definition:

```rust
/// Why a seeded `Ctx` factory rejected a run's seed. Surfaced as a
/// **non-retryable** activity failure so a malformed/hostile seed fails the run
/// fast instead of retry-looping.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct CtxSeedError(String);

impl CtxSeedError {
    pub(crate) fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}
```

Change the builder field type and `builder()` initializer:

```rust
// field
ctx_factory: Option<Arc<dyn Fn(Option<serde_json::Value>) -> Result<Ctx, CtxSeedError> + Send + Sync>>,
```

Rewrap `with_ctx` and add the two new setters:

```rust
/// Set the per-activity-invocation `Ctx` factory (seed ignored).
pub fn with_ctx(mut self, factory: impl Fn() -> Ctx + Send + Sync + 'static) -> Self {
    self.ctx_factory = Some(Arc::new(move |_seed| Ok(factory())));
    self
}

/// Set a seeded `Ctx` factory that reconstitutes the per-run context from the
/// client's `serde_json::Value` seed (`None` when the client set none).
///
/// **Totality contract:** this closure must never panic and should be cheap —
/// it runs once per `render_instructions` and per `invoke_tool` invocation. For
/// authorization-bearing seeds prefer [`Self::try_with_seeded_ctx`] so a bad
/// seed fails the run loudly instead of defaulting to the wrong identity.
pub fn with_seeded_ctx(
    mut self,
    factory: impl Fn(Option<serde_json::Value>) -> Ctx + Send + Sync + 'static,
) -> Self {
    self.ctx_factory = Some(Arc::new(move |seed| Ok(factory(seed))));
    self
}

/// Like [`Self::with_seeded_ctx`], but fallible: a seed the factory rejects
/// fails the run with a **non-retryable** activity error instead of proceeding
/// under a default identity.
pub fn try_with_seeded_ctx<E: std::fmt::Display>(
    mut self,
    factory: impl Fn(Option<serde_json::Value>) -> Result<Ctx, E> + Send + Sync + 'static,
) -> Self {
    self.ctx_factory =
        Some(Arc::new(move |seed| factory(seed).map_err(|e| CtxSeedError::new(e.to_string()))));
    self
}
```

- [ ] **Step 5: Make `run_context` fallible + thread `ctx_seed` through the trait** in `activities.rs`.
  - `TypedRuntime.ctx_factory` field type → `Arc<dyn Fn(Option<serde_json::Value>) -> Result<Ctx, crate::worker::CtxSeedError> + Send + Sync>`.
  - Rewrite `run_context`:
    ```rust
    fn run_context(
        &self,
        seed: Option<serde_json::Value>,
        cancel: CancellationToken,
    ) -> Result<RunContext<Ctx>, ActivityError> {
        let user_ctx = (self.ctx_factory)(seed).map_err(|e| {
            ActivityError::application(ApplicationFailure::non_retryable(format!(
                "ctx seed rejected: {e}"
            )))
        })?;
        let ctx = RunContext::ephemeral(user_ctx).with_cancel(cancel);
        Ok(self.posture.apply(ctx))
    }
    ```
  - Add `ctx_seed: Option<serde_json::Value>` to the `DurableAgentRuntime` trait methods `render_instructions` and `invoke_tool` (not `call_model`), and to the `TypedRuntime` impls:
    ```rust
    async fn render_instructions(
        &self,
        agent_name: &str,
        ctx_seed: Option<serde_json::Value>,
        cancel: CancellationToken,
    ) -> Result<String, ActivityError> {
        let def = self.resolve(agent_name)?;
        let run_ctx = self.run_context(ctx_seed, cancel)?;
        Ok(render_instructions_inner(&def, &run_ctx).await)
    }

    async fn invoke_tool(
        &self,
        agent_name: &str,
        call: ToolCallRequest,
        ctx_seed: Option<serde_json::Value>,
        cancel: CancellationToken,
    ) -> Result<ToolCallOutcome, ActivityError> {
        let def = self.resolve(agent_name)?;
        let run_ctx = self.run_context(ctx_seed, cancel)?;
        Ok(invoke_tool_inner(&def, &run_ctx, call).await)
    }
    ```
  - Update `build_activities`'s `ctx_factory` param type to the fallible slot type (mirroring the builder field).
  - In the `#[activities] impl AgentActivities`, the `render_instructions` and `invoke_tool` methods pass **`None`** for now:
    ```rust
    // inside render_instructions activity:
    self.runtime.render_instructions(&agent_name, None, cancel)
    // inside invoke_tool activity:
    self.runtime.invoke_tool(&agent_name, call, None, cancel)
    ```

- [ ] **Step 6: Run the whole crate's tests**

Run: `cargo test -p paigasus-helikon-runtime-temporal`
Expected: PASS (new fallible + composition tests pass; existing tests unaffected — the `#[activity]` wrappers still compile with `None`).

- [ ] **Step 7: fmt + clippy, then commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-temporal/src/worker.rs crates/paigasus-helikon-runtime-temporal/src/activities.rs
git commit -m "feat(runtime-temporal): SMA-455 add fallible seeded ctx factory with non-retryable seed rejection"
```

---

## Task 3: Thread the Ctx seed across the client→worker boundary

Adds the wire field, the client-side config knob, and threads the seed through the workflow into the `render_instructions`/`invoke_tool` activity argument tuples so a real client seed reaches the factory added in Task 2.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/payloads.rs` (`WorkflowInput.ctx_seed`)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/runner.rs` (private `ctx_seed` + `with_ctx_seed`; thread into `WorkflowInput`)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/workflow.rs` (thread `input.ctx_seed` into activity tuples)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activities.rs` (the two `#[activity]` methods gain a `ctx_seed` param and pass it through instead of `None`)
- Test: inline in `payloads.rs` and `runner.rs`

**Interfaces:**
- Consumes: `DurableAgentRuntime::{render_instructions, invoke_tool}` seed param (Task 2).
- Produces:
  - `WorkflowInput.ctx_seed: Option<serde_json::Value>` (`#[serde(default)]`, public field).
  - `TemporalRunnerConfig::with_ctx_seed(self, serde_json::Value) -> Self` (backing field private).
  - `#[activity] render_instructions(self, ctx, agent_name: String, ctx_seed: Option<Value>)` and `invoke_tool(self, ctx, agent_name: String, call: ToolCallRequest, ctx_seed: Option<Value>)`.

- [ ] **Step 1: Write the failing test — `WorkflowInput` seed round-trips and defaults** (in `payloads.rs` tests)

```rust
#[test]
fn workflow_input_ctx_seed_roundtrips_and_defaults() {
    // present
    let input = WorkflowInput {
        agent_name: "a".to_owned(),
        conversation: vec![],
        config: DriverConfig { max_turns: 4, parallel_tool_call_limit: None },
        timeout_ms: None,
        ctx_seed: Some(serde_json::json!({"tenant": "acme"})),
    };
    let json = serde_json::to_string(&input).expect("serialize");
    let back: WorkflowInput = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.ctx_seed, Some(serde_json::json!({"tenant": "acme"})));

    // legacy payload without the field deserializes to None
    let legacy = r#"{"agent_name":"a","conversation":[],"config":{"max_turns":4,"parallel_tool_call_limit":null},"timeout_ms":null}"#;
    let back: WorkflowInput = serde_json::from_str(legacy).expect("legacy deserialize");
    assert_eq!(back.ctx_seed, None);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-runtime-temporal workflow_input_ctx_seed -- --nocapture`
Expected: FAIL to compile (`WorkflowInput` has no `ctx_seed`).

- [ ] **Step 3: Add the field** in `payloads.rs`, at the end of `WorkflowInput`:

```rust
    /// Run timeout as milliseconds; None = no deadline.
    pub timeout_ms: Option<u64>,
    /// Optional, explicit request-scoped seed the worker's seeded ctx factory
    /// reconstitutes into a `Ctx`. `#[serde(default)]` so pre-SMA-455 payloads
    /// (which lack the field) still deserialize. Recorded in Temporal history —
    /// keep it small and secret-free.
    #[serde(default)]
    pub ctx_seed: Option<serde_json::Value>,
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p paigasus-helikon-runtime-temporal workflow_input_ctx_seed -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Write the failing test — the runner config carries the seed** (in `runner.rs` tests; add a `#[cfg(test)] mod tests` if none exists)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_ctx_seed_stores_seed() {
        let cfg = TemporalRunnerConfig::new("q").with_ctx_seed(serde_json::json!({"tenant": "acme"}));
        assert_eq!(cfg.ctx_seed, Some(serde_json::json!({"tenant": "acme"})));
    }

    #[test]
    fn ctx_seed_defaults_none() {
        assert_eq!(TemporalRunnerConfig::new("q").ctx_seed, None);
    }
}
```

- [ ] **Step 6: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-runtime-temporal with_ctx_seed_stores -- --nocapture`
Expected: FAIL to compile (`ctx_seed` field absent).

- [ ] **Step 7: Add the private field + builder** in `runner.rs`.
  - Add to `struct TemporalRunnerConfig` (note: **not** `pub`):
    ```rust
        /// Optional request-scoped seed forwarded to the worker's seeded ctx
        /// factory. Private: set via [`Self::with_ctx_seed`]. Default `None`.
        ctx_seed: Option<serde_json::Value>,
    ```
  - Initialize it in `TemporalRunnerConfig::new` (`ctx_seed: None,`).
  - Add the builder method:
    ```rust
    /// Attach a request-scoped seed forwarded (explicitly) to the worker's
    /// seeded ctx factory for every run this config drives. Recorded in
    /// Temporal history — keep it small and secret-free.
    pub fn with_ctx_seed(mut self, seed: serde_json::Value) -> Self {
        self.ctx_seed = Some(seed);
        self
    }
    ```
  - In `run_inner`, set the new `WorkflowInput` field: `ctx_seed: self.config.ctx_seed.clone(),`.

- [ ] **Step 8: Run to verify pass**

Run: `cargo test -p paigasus-helikon-runtime-temporal with_ctx_seed_stores ctx_seed_defaults -- --nocapture`
Expected: PASS.

- [ ] **Step 9: Thread the seed through the workflow into the activity tuples.** In `activities.rs`, change the two `#[activity]` methods to accept and forward the seed:

```rust
#[activity]
pub(crate) async fn render_instructions(
    self: Arc<Self>,
    ctx: ActivityContext,
    agent_name: String,
    ctx_seed: Option<serde_json::Value>,
) -> Result<String, ActivityError> {
    let cancel = CancellationToken::new();
    race_with_activity_cancellation(
        &ctx,
        cancel.clone(),
        self.runtime.render_instructions(&agent_name, ctx_seed, cancel),
    )
    .await
}

#[activity]
pub(crate) async fn invoke_tool(
    self: Arc<Self>,
    ctx: ActivityContext,
    agent_name: String,
    call: ToolCallRequest,
    ctx_seed: Option<serde_json::Value>,
) -> Result<ToolCallOutcome, ActivityError> {
    let cancel = CancellationToken::new();
    race_with_activity_cancellation(
        &ctx,
        cancel.clone(),
        self.runtime.invoke_tool(&agent_name, call, ctx_seed, cancel),
    )
    .await
}
```

In `workflow.rs`, thread `input.ctx_seed` down. `drive` already owns `input`; capture the seed once and pass it to `run_effects`/`execute_tools`:
  - In `drive`, after `let parallel_limit = …;` add: `let ctx_seed = input.ctx_seed.clone();` and pass `&ctx_seed` into `run_effects(ctx, &config, &agent_name, &ctx_seed, parallel_limit, &mut driver)`.
  - `run_effects` signature gains `ctx_seed: &Option<serde_json::Value>`; update the two `start_activity` calls:
    ```rust
    // RenderInstructions arm:
    .start_activity(
        AgentActivities::render_instructions,
        (agent_name.to_owned(), ctx_seed.clone()),
        config.instructions_activity_opts.clone(),
    )
    // ExecuteTools arm: pass ctx_seed into execute_tools(...)
    let outcomes = execute_tools(ctx, config, agent_name, ctx_seed, parallel_limit, calls).await;
    ```
  - `execute_tools` signature gains `ctx_seed: &Option<serde_json::Value>`; in the per-call async block change the tuple:
    ```rust
    .start_activity(
        AgentActivities::invoke_tool,
        (agent_name, call, ctx_seed_cloned),
        opts,
    )
    ```
    (clone `ctx_seed` into `ctx_seed_cloned` alongside the existing `let call = call.clone();` etc. so the `async move` owns it). `call_model`'s `start_activity` tuple is **unchanged**.

- [ ] **Step 10: Run the whole crate's tests to verify it all compiles and passes**

Run: `cargo test -p paigasus-helikon-runtime-temporal`
Expected: PASS. The activity-marker test (`activity_markers_exist_with_expected_names`) still passes (the consts exist regardless of arity).

- [ ] **Step 11: fmt + clippy, then commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-temporal/src/payloads.rs crates/paigasus-helikon-runtime-temporal/src/runner.rs crates/paigasus-helikon-runtime-temporal/src/workflow.rs crates/paigasus-helikon-runtime-temporal/src/activities.rs
git commit -m "feat(runtime-temporal): SMA-455 thread request-scoped ctx seed from client to worker"
```

---

## Task 4: Opt-in heartbeat-aware activities

Adds a `heartbeat_interval` worker knob (1 s floor), sets `heartbeat_timeout` on the model/tool `ActivityOptions`, and rewrites the cancellation race into a loop that also emits liveness heartbeats. The race logic is extracted into a generic, unit-testable helper (an `ActivityContext` can't be faked in a unit test).

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/worker.rs` (`heartbeat_interval` field + `.heartbeat_interval()` with floor; thread into `build_activities` + `build_activity_config`)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/workflow.rs` (`activity_opts` + `build_activity_config` set `heartbeat_timeout`)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activities.rs` (`AgentActivities.heartbeat_interval`; extract `race_loop`; rewrite `race_with_activity_cancellation`; `call_model`/`invoke_tool` pass the interval, `render_instructions` passes `None`)
- Test: inline in `workflow.rs` and `activities.rs`

**Interfaces:**
- Consumes: `build_activities` (Task 1/2), `build_activity_config`/`activity_opts` (existing), the `#[activity]` methods (Task 3).
- Produces:
  - `TemporalAgentWorkerBuilder::heartbeat_interval(self, Duration) -> Self` (clamps to ≥ `MIN_HEARTBEAT_INTERVAL` = 1 s).
  - `build_activities(registry, ctx_factory, posture, heartbeat_interval: Option<Duration>)` (new 4th param).
  - `build_activity_config(plans, model_retry, tool_retry, timeouts, heartbeat_timeout: Option<Duration>)` (new 5th param).
  - generic `async fn race_loop<T>(work, cancelled, on_cancel, heartbeat_interval, on_heartbeat) -> T`.

- [ ] **Step 1: Write the failing test — `build_activity_config` sets heartbeat_timeout on model/tool only** (in `workflow.rs` tests)

```rust
#[test]
fn build_activity_config_sets_heartbeat_timeout_on_model_and_tool_only() {
    let config = build_activity_config(
        HashMap::new(),
        &RetryPolicyConfig::default(),
        &RetryPolicyConfig::default(),
        &ActivityTimeouts::default(),
        Some(Duration::from_secs(4)),
    );
    assert_eq!(config.model_activity_opts.heartbeat_timeout, Some(Duration::from_secs(4)));
    assert_eq!(config.tool_activity_opts.heartbeat_timeout, Some(Duration::from_secs(4)));
    assert_eq!(config.instructions_activity_opts.heartbeat_timeout, None);
}

#[test]
fn build_activity_config_no_heartbeat_when_none() {
    let config = build_activity_config(
        HashMap::new(),
        &RetryPolicyConfig::default(),
        &RetryPolicyConfig::default(),
        &ActivityTimeouts::default(),
        None,
    );
    assert_eq!(config.model_activity_opts.heartbeat_timeout, None);
    assert_eq!(config.tool_activity_opts.heartbeat_timeout, None);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-runtime-temporal build_activity_config_sets_heartbeat -- --nocapture`
Expected: FAIL to compile (`build_activity_config` arity).

- [ ] **Step 3: Wire `heartbeat_timeout` into the activity options** in `workflow.rs`:
  - Change `activity_opts` to take a heartbeat timeout and set it:
    ```rust
    fn activity_opts(
        start_to_close: Duration,
        retry_policy: Option<RetryPolicy>,
        heartbeat_timeout: Option<Duration>,
    ) -> ActivityOptions {
        ActivityOptions::with_start_to_close_timeout(start_to_close)
            .maybe_retry_policy(retry_policy)
            .maybe_heartbeat_timeout(heartbeat_timeout)
            .build()
    }
    ```
  - Change `build_activity_config` to accept `heartbeat_timeout: Option<Duration>` and pass it to the model/tool opts (and `None` to instructions):
    ```rust
    pub(crate) fn build_activity_config(
        plans: HashMap<String, AgentPlan>,
        model_retry: &RetryPolicyConfig,
        tool_retry: &RetryPolicyConfig,
        timeouts: &ActivityTimeouts,
        heartbeat_timeout: Option<Duration>,
    ) -> WorkflowActivityConfig {
        WorkflowActivityConfig {
            plans,
            instructions_activity_opts: activity_opts(timeouts.instructions, None, None),
            model_activity_opts: activity_opts(
                timeouts.model,
                to_proto_retry_policy(model_retry),
                heartbeat_timeout,
            ),
            tool_activity_opts: activity_opts(
                timeouts.tool,
                to_proto_retry_policy(tool_retry),
                heartbeat_timeout,
            ),
        }
    }
    ```
  - Update the existing `build_activity_config` call sites in `workflow.rs`'s own tests (`build_activity_config_attaches_retry_policies`, `build_activity_config_applies_timeout_overrides`) to pass a trailing `None`.

- [ ] **Step 4: Run to verify the two new tests pass**

Run: `cargo test -p paigasus-helikon-runtime-temporal build_activity_config -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Write the failing test — the generic race loop heartbeats and never leaks work** (in `activities.rs` tests)

```rust
#[tokio::test]
async fn race_loop_awaits_work_after_cancel() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc as StdArc;
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let done = StdArc::new(AtomicBool::new(false));
    let done2 = StdArc::clone(&done);
    let cancelled_flag = StdArc::new(AtomicBool::new(false));
    let cf = StdArc::clone(&cancelled_flag);

    let work = async move {
        let _ = rx.await; // completes only after on_cancel fires tx
        done2.store(true, Ordering::SeqCst);
        7u8
    };
    let result = race_loop(
        work,
        async { /* cancelled: immediately */ },
        move || {
            cf.store(true, Ordering::SeqCst);
            let _ = tx.send(()); // let the work future wind down
        },
        None,
        || {},
    )
    .await;

    assert_eq!(result, 7);
    assert!(cancelled_flag.load(Ordering::SeqCst), "on_cancel ran");
    assert!(done.load(Ordering::SeqCst), "work was awaited to completion, not dropped");
}

#[tokio::test]
async fn race_loop_heartbeats_until_work_done() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc as StdArc;
    let beats = StdArc::new(AtomicU32::new(0));
    let b2 = StdArc::clone(&beats);
    let work = async {
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        1u8
    };
    let result = race_loop(
        work,
        std::future::pending::<()>(), // never cancelled
        || {},
        Some(std::time::Duration::from_millis(10)),
        move || {
            b2.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;
    assert_eq!(result, 1);
    assert!(beats.load(Ordering::SeqCst) >= 1, "at least one heartbeat fired");
}
```

- [ ] **Step 6: Run to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-temporal race_loop -- --nocapture`
Expected: FAIL to compile (`race_loop` absent).

- [ ] **Step 7: Extract `race_loop` and rewrite the wrapper** in `activities.rs`. Replace `race_with_activity_cancellation` with a thin binding over a generic loop:

```rust
/// Generic cancellation/heartbeat race, decoupled from `ActivityContext` so it
/// is unit-testable. Polls `work` and `cancelled` before the heartbeat tick
/// (`biased`). On cancellation it runs `on_cancel` then **awaits `work` to
/// completion** (never drops it — no detached task leak). When
/// `heartbeat_interval` is `Some`, `on_heartbeat` fires each tick until `work`
/// completes.
async fn race_loop<T>(
    work: impl std::future::Future<Output = T>,
    cancelled: impl std::future::Future<Output = ()>,
    on_cancel: impl FnOnce(),
    heartbeat_interval: Option<std::time::Duration>,
    mut on_heartbeat: impl FnMut(),
) -> T {
    tokio::pin!(work, cancelled);
    let mut ticker = heartbeat_interval.map(tokio::time::interval);
    loop {
        tokio::select! {
            biased;
            result = &mut work => return result,
            () = &mut cancelled => {
                on_cancel();
                return work.await;
            }
            _ = async {
                match ticker.as_mut() {
                    Some(t) => { t.tick().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {
                on_heartbeat();
            }
        }
    }
}

/// Race `work` against the activity's cancellation signal, emitting liveness
/// heartbeats every `heartbeat` while it runs (when configured).
async fn race_with_activity_cancellation<T>(
    activity_ctx: &ActivityContext,
    cancel: CancellationToken,
    heartbeat: Option<std::time::Duration>,
    work: impl std::future::Future<Output = T>,
) -> T {
    race_loop(
        work,
        activity_ctx.cancelled(),
        || cancel.cancel(),
        heartbeat,
        || activity_ctx.record_heartbeat(Vec::new()),
    )
    .await
}
```

- [ ] **Step 8: Thread the interval into the activities.** Add `heartbeat_interval: Option<Duration>` to `struct AgentActivities`, set it in `build_activities` (new 4th param), and pass it at the call sites:
  - `render_instructions` activity → `race_with_activity_cancellation(&ctx, cancel.clone(), None, …)`.
  - `call_model` activity → `race_with_activity_cancellation(&ctx, cancel.clone(), self.heartbeat_interval, …)`.
  - `invoke_tool` activity → `race_with_activity_cancellation(&ctx, cancel.clone(), self.heartbeat_interval, …)`.
  - Add `use std::time::Duration;` to `activities.rs` if not present.

- [ ] **Step 9: Add the builder knob + thread through `build()`** in `worker.rs`:
  - Add `const MIN_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);` and a `heartbeat_interval: Option<Duration>` field (init `None` in `builder()`).
  - Setter:
    ```rust
    /// Enable liveness heartbeats on the `call_model`/`invoke_tool` activities,
    /// ticking every `interval` (clamped to a 1 s minimum) and setting
    /// `heartbeat_timeout = 2 × interval` on those activities so Temporal
    /// reclaims a **crashed** worker's in-flight attempt promptly. Off by
    /// default. See the crate docs for the executor-starvation caveat.
    pub fn heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = Some(interval.max(MIN_HEARTBEAT_INTERVAL));
        self
    }
    ```
  - In `build()`: pass `self.heartbeat_interval` to `build_activities(...)` (4th arg) and compute + pass the timeout to `build_activity_config`:
    ```rust
    let heartbeat_timeout = self.heartbeat_interval.map(|iv| iv * 2);
    let workflow_config = Arc::new(crate::workflow::build_activity_config(
        plans,
        &self.model_retry_policy,
        &self.tool_retry_policy,
        &timeouts,
        heartbeat_timeout,
    ));
    ```

- [ ] **Step 10: Write the failing test — the interval floor clamps** (in `worker.rs` tests)

```rust
#[test]
fn heartbeat_interval_is_floored_to_one_second() {
    let b = TemporalAgentWorker::builder::<()>()
        .heartbeat_interval(std::time::Duration::from_millis(100));
    assert_eq!(b.heartbeat_interval, Some(std::time::Duration::from_secs(1)));
    let b = TemporalAgentWorker::builder::<()>()
        .heartbeat_interval(std::time::Duration::from_secs(5));
    assert_eq!(b.heartbeat_interval, Some(std::time::Duration::from_secs(5)));
}
```

- [ ] **Step 11: Run the whole crate's tests**

Run: `cargo test -p paigasus-helikon-runtime-temporal`
Expected: PASS (heartbeat floor, race_loop ×2, build_activity_config ×2, plus all prior).

- [ ] **Step 12: fmt + clippy, then commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-temporal/src/worker.rs crates/paigasus-helikon-runtime-temporal/src/workflow.rs crates/paigasus-helikon-runtime-temporal/src/activities.rs
git commit -m "feat(runtime-temporal): SMA-455 add opt-in heartbeat-aware model and tool activities"
```

---

## Task 5: Documentation, live-test scaffolding, and full-CI gate

Brings all user-facing docs in line and runs the complete CI gate set before handing off.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/lib.rs` (rewrite the "Worker-Side Posture and Security Boundary" section; update the payload-budget + determinism sections)
- Modify: `crates/paigasus-helikon-runtime-temporal/README.md`
- Modify: `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md`
- Modify: `docs/superpowers/specs/2026-07-05-runtime-temporal-agentcore-design.md` (§5.8 as-built)
- Modify: `docs/book/src/concepts/runtimes.md`
- Optional: `crates/paigasus-helikon-runtime-temporal/tests/temporal_live.rs` (env-gated seed+policy assertion, if a live harness exists)

- [ ] **Step 1: Rewrite the `lib.rs` "Worker-Side Posture and Security Boundary" section.** Replace the "fixed, non-configurable defaults" + "future work" prose with: (a) posture is now configurable via `WorkerPosture` on the builder, defaults unchanged; (b) the config-level `Ctx` seed (`TemporalRunnerConfig::with_ctx_seed`) + the seeded/`try_`-seeded worker factories, incl. the fail-fast-on-bad-seed contract and "seed is recorded in history — keep small & secret-free"; (c) the seed→policy composition (per-run authz without serializing the policy) and the note that policy is only consulted under `Default`/`AcceptEdits`/`Plan`. In the **payload-budget** section, add that the seed is serialized per `render_instructions`/`invoke_tool` call. In the **determinism/upgrade** section, describe the additions honestly (additive `#[serde(default)]` field; the activity-input change only affects newly-scheduled activities; drain-before-upgrade remains the conservative path) — do not claim an unqualified "replay-breaking". In a heartbeat note, recommend tools offload blocking work via `tokio::task::spawn_blocking` and cross-reference the existing "tool idempotency under crash-retry" warning. Keep all doctests `no_run`/compiling.

- [ ] **Step 2: Update `README.md`** with a short posture + seed usage snippet (worker sets `.posture(WorkerPosture::default().with_permission_mode(...))` and `.try_with_seeded_ctx(...)`; client sets `TemporalRunnerConfig::new(q).with_ctx_seed(json!({...}))`) and a one-line mention of opt-in heartbeats. Match the README's existing style.

- [ ] **Step 3: Update `CHANGELOG.md`** (runtime-temporal) under an unreleased/next section: the three features + the honest upgrade note (per Task 5 Step 1). Do not hand-edit a version number — release-plz owns versions.

- [ ] **Step 4: Update the SMA-332 spec §5.8 as-built note.** In `docs/superpowers/specs/2026-07-05-runtime-temporal-agentcore-design.md`, change the two lines that call the optional worker-side posture configuration and the serializable-`Ctx` seed "future work" to "landed in SMA-455", with a pointer to `docs/superpowers/specs/2026-07-06-runtime-temporal-worker-posture-design.md`.

- [ ] **Step 5: Align `docs/book/src/concepts/runtimes.md`** if it states the durable runtime's posture is fixed/non-configurable — update it to reflect `WorkerPosture` + the seed. If it says nothing posture-specific, make a conscious no-op and note it in the commit body.

- [ ] **Step 6: (If a live harness exists) add an env-gated seed+policy live test** to `tests/temporal_live.rs`, mirroring the existing loud-skip pattern (`FORKD`/temporal env gate): start a worker with a `try_with_seeded_ctx` factory + a tenant policy, run with `with_ctx_seed`, assert allow/deny by tenant. If no such harness is present, skip this step and rely on the Task 2 composition unit test — note the manual validation in the CHANGELOG upgrade note.

- [ ] **Step 7: Run the full CI gate set (foreground, synchronous).**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
mdbook build docs/book
```
Expected: all green. Fix any doc-coverage or link-check failures inline (every new `pub` item has a `///`).

- [ ] **Step 8: Commit the docs**

```bash
cargo fmt --all
git add crates/paigasus-helikon-runtime-temporal/src/lib.rs crates/paigasus-helikon-runtime-temporal/README.md crates/paigasus-helikon-runtime-temporal/CHANGELOG.md docs/superpowers/specs/2026-07-05-runtime-temporal-agentcore-design.md docs/book/src/concepts/runtimes.md
# add tests/temporal_live.rs only if Step 6 was done
git commit -m "docs(runtime-temporal): SMA-455 document worker posture, ctx seed, and heartbeats"
```

---

## Self-review (spec coverage)

| Spec section | Covered by |
|--------------|-----------|
| §4.1 `WorkerPosture` + `apply` (one-way toggles, drift note) | Task 1 (+ default-equivalence test) |
| §4.2 fallible factory, `with_ctx`/`with_seeded_ctx`/`try_with_seeded_ctx`, `CtxSeedError` | Task 2 |
| §4.3 `run_context(seed) -> Result`, non-retryable mapping, seed→render/tool only | Task 2 (+ Task 3 for the real seed) |
| §4.4 `WorkflowInput.ctx_seed` `#[serde(default)]`, private config field, workflow threading | Task 3 |
| §4.5 heartbeats: floor, `heartbeat_timeout` on model/tool only, loop rewrite + no-leak, empty details | Task 4 |
| §4.6 seed→policy composition | Task 2 (composition test) |
| §5 security (DoS fail-fast, mode reachability, history/redaction) | Task 2 (behavior) + Task 5 (docs) |
| §6 public API surface, **no re-exports** | Tasks 1–4 (items) + Task 5 (doc comments) |
| §7 testing strategy | one-to-one with the failing-test steps in Tasks 1–4; live test = Task 5 Step 6 |
| §8 docs to update | Task 5 |
| §9/§11 release + no core change | No core edit in any task; release via release-plz (facade/version bump considered post-merge) |

**Type consistency:** `run_context(Option<Value>, CancellationToken) -> Result<RunContext<Ctx>, ActivityError>`, `build_activities(registry, ctx_factory, posture, heartbeat_interval)`, and `build_activity_config(…, heartbeat_timeout)` are used with identical signatures across the tasks that define and call them. `WorkerPosture::apply` / `with_*` names match §4.1. `ctx_seed` is the single field/param name everywhere.
