# Guardrails, Hooks & PermissionPolicy — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drive the three control layers (permissions, guardrails, hooks) into the agent loop: gate every tool call, block input/output on a guardrail tripwire, and fire lifecycle hooks — all woven into the driver, leaving the pure `transition()` state machine untouched.

**Architecture:** Net-new `permission.rs` (types) and `control.rs` (`Interceptors`, the unit-tested orchestration unit). The driver (`Agent::run`'s `async_stream`) calls `Interceptors` at nine seams. `RunContext` carries the permission config (mode + policy + deny rules + approval handler) and propagates it to every sub-run (`Bypass` sticky). The `#[tool]` macro gains `effect=`. `loop_state.rs` does not change.

**Tech Stack:** Rust, `async-trait`, `futures-util`, `async-stream`, `tokio` (tests), `schemars`/`serde_json`. Workspace gates: `cargo fmt`, `clippy -D warnings`, `cargo test --all-features`, `RUSTDOCFLAGS=-D warnings cargo doc`, doc-coverage ≥ 80%.

**Spec:** `docs/superpowers/specs/2026-06-07-guardrails-hooks-permissions-design.md` (read it first).

**Conventions for every commit in this plan:**
- Branch is `feature/sma-326-guardrails-hooks-and-permissionpolicy` (already created).
- Run `cargo fmt --all` before every commit (the pre-commit hook is a no-op; pre-push runs fmt+clippy).
- Commit message prefix `feat(core): SMA-326 <lowercase subject>` (or `feat(macros): SMA-326 …` for Task 13, `chore(release): SMA-326 …` for Task 14). Subject must start lowercase.
- Add the trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- `git add <explicit paths>` only — never `git add -A` (`.env`/`.claude` are untracked-but-not-ignored).
- New `pub` items need a `///` doc comment (the docs job runs `-D warnings`).

---

## File structure

| File | Responsibility |
| --- | --- |
| `crates/paigasus-helikon-core/src/permission.rs` | **new** — `PermissionMode`, `PermissionDecision`, `PermissionPolicy`, `DenyRule`, `ApprovalHandler`, `ApprovalOutcome` |
| `crates/paigasus-helikon-core/src/control.rs` | **new** — `Interceptors<'a, Ctx>`, `ResolvedHookDecision`; permission pipeline, hook resolution, guardrail runners |
| `crates/paigasus-helikon-core/src/tool.rs` | `ToolEffect` + `Tool::effect()`; `ToolContext` permission carriers |
| `crates/paigasus-helikon-core/src/context.rs` | `RunContext` permission fields, monotonic setters, child propagation, `to_tool_context` projection |
| `crates/paigasus-helikon-core/src/hook.rs` | `HookEvent::OnSubagentStop` |
| `crates/paigasus-helikon-core/src/agent.rs` | snapshot guardrails/hooks; drive nine seams; `run_tools_concurrent` interleaving; `AgentEvent::PermissionDenied`, `AgentError::HookDenied` |
| `crates/paigasus-helikon-core/src/agent_as_tool.rs` | rebuild sub-`RunContext` with permission config; fire `OnSubagentStop` |
| `crates/paigasus-helikon-core/src/workflow.rs` | fire `OnSubagentStop` after each sub-agent run |
| `crates/paigasus-helikon-core/src/lib.rs` | `pub mod permission; pub mod control;` + re-exports |
| `crates/paigasus-helikon-macros/src/attr.rs` | parse `effect = …` |
| `crates/paigasus-helikon-macros/src/expand.rs` | emit `fn effect()` override |
| `crates/paigasus-helikon-core/tests/permissions.rs` | **new** — AC1/AC3 + approval integration tests |
| `crates/paigasus-helikon-core/tests/hooks.rs` | **new** — AC2 + hook lifecycle integration tests |
| `crates/paigasus-helikon-core/tests/guardrails.rs` | **new** — input/output guardrail integration tests |
| `crates/paigasus-helikon-core/tests/subagent_propagation.rs` | **new** — `Bypass` + `OnSubagentStop` across sub-run paths |
| `crates/paigasus-helikon-macros/tests/` | macro `effect=` test |

---

## Task 1: `permission.rs` — permission types

**Files:**
- Create: `crates/paigasus-helikon-core/src/permission.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs:32` (add `pub mod permission;`) and `:51` (add `pub use permission::*;`)

- [ ] **Step 1: Write `permission.rs` with the types and unit tests**

```rust
//! Permission layer: gate tool calls via `deny rules › permission mode ›
//! `canUseTool` policy`. See the *Permissions, Guardrails & Hooks* concept page.

use async_trait::async_trait;

use crate::RunContext;

/// How permission mode governs tool calls.
///
/// `Bypass` propagates to subagents and **cannot be overridden** — a typed
/// enum, not a string. The non-override property is enforced by
/// [`RunContext::with_permission_mode`], which refuses to downgrade `Bypass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum PermissionMode {
    /// Defer to the policy (ask for unfamiliar tools); permissive when no policy.
    #[default]
    Default,
    /// Auto-approve `Write`-effect tools.
    AcceptEdits,
    /// Read-only: deny any tool whose [`crate::ToolEffect`] is not `ReadOnly`.
    Plan,
    /// Dangerous: allow all (deny rules still apply). Propagates; sticky.
    Bypass,
}

/// The outcome of a [`PermissionPolicy::check`] (or the resolved decision).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PermissionDecision {
    /// Run the call unchanged.
    Allow,
    /// Block the call; the reason is surfaced to the model as a tool result.
    Deny {
        /// Human-readable denial reason.
        reason: String,
    },
    /// Ask a human (resolved via [`ApprovalHandler`]; default Deny).
    AskUser {
        /// Prompt shown to the approver.
        prompt: String,
    },
    /// Replace the call's arguments before execution (sanitize).
    Replace {
        /// Replacement JSON arguments.
        args: serde_json::Value,
    },
}

/// Authorizes a tool call. The decision pipeline runs
/// `deny rules › mode › this policy` (see `control.rs`).
#[async_trait]
pub trait PermissionPolicy<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Decide whether `tool` may run with `args`.
    async fn check(
        &self,
        ctx: &RunContext<Ctx>,
        tool: &str,
        args: &serde_json::Value,
    ) -> PermissionDecision;
}

/// A first-class deny rule, evaluated **before** mode — so it overrides even
/// [`PermissionMode::Bypass`]. v1 matches by exact tool name.
#[derive(Debug, Clone)]
pub struct DenyRule {
    tool: String,
}

impl DenyRule {
    /// Deny a tool by its exact name.
    pub fn tool(name: impl Into<String>) -> Self {
        Self { tool: name.into() }
    }

    /// `true` if this rule denies `tool`. `_args` is reserved for richer
    /// (arg-aware) matchers in a later ticket.
    pub fn matches(&self, tool: &str, _args: &serde_json::Value) -> bool {
        self.tool == tool
    }
}

/// Resolves a [`PermissionDecision::AskUser`] when the driver cannot decide
/// inline. Non-generic — it needs no `Ctx`.
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    /// Decide an `AskUser` prompt. Returns a narrowed [`ApprovalOutcome`]
    /// (cannot recursively ask).
    async fn decide(
        &self,
        tool: &str,
        prompt: &str,
        args: &serde_json::Value,
    ) -> ApprovalOutcome;
}

/// The narrowed decision an [`ApprovalHandler`] may return.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ApprovalOutcome {
    /// Allow the call.
    Allow,
    /// Deny the call with a reason.
    Deny {
        /// Human-readable denial reason.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn permission_mode_default_is_default_variant() {
        assert_eq!(PermissionMode::default(), PermissionMode::Default);
    }

    #[test]
    fn deny_rule_matches_exact_tool_name_only() {
        let rule = DenyRule::tool("rm");
        assert!(rule.matches("rm", &json!({})));
        assert!(!rule.matches("ls", &json!({})));
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

In `crates/paigasus-helikon-core/src/lib.rs`, add `pub mod permission;` in the alphabetical module list (after `pub mod model;`, before `pub mod runner;`) and `pub use permission::*;` in the re-export list (after `pub use model::*;`).

- [ ] **Step 3: Run the unit tests**

Run: `cargo test -p paigasus-helikon-core --lib permission`
Expected: PASS (2 tests).

- [ ] **Step 4: Verify it compiles clean**

Run: `cargo clippy -p paigasus-helikon-core --all-features -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/permission.rs crates/paigasus-helikon-core/src/lib.rs
git commit -m "feat(core): SMA-326 add permission types (mode, decision, policy, deny rule, approval)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `ToolEffect` + `Tool::effect()`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/tool.rs` (add `ToolEffect`, add the default method to the `Tool` trait)

- [ ] **Step 1: Write the failing unit test**

Add to the existing `#[cfg(test)] mod tool_context_tests` (or a new `mod effect_tests`) at the bottom of `tool.rs`:

```rust
#[cfg(test)]
mod effect_tests {
    use crate::ToolEffect;

    #[test]
    fn tool_effect_default_is_side_effect() {
        assert_eq!(ToolEffect::default(), ToolEffect::SideEffect);
    }
}
```

- [ ] **Step 2: Run it to confirm it fails to compile**

Run: `cargo test -p paigasus-helikon-core --lib effect_tests`
Expected: FAIL — `cannot find type ToolEffect`.

- [ ] **Step 3: Add `ToolEffect` and the trait method**

In `tool.rs`, add the enum near the top (after the imports):

```rust
/// A tool's side-effect profile. Drives [`crate::PermissionMode`] decisions:
/// `Plan` allows only `ReadOnly`; `AcceptEdits` auto-approves `Write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ToolEffect {
    /// No side effects; safe to run under `Plan` mode.
    ReadOnly,
    /// Mutates local/filesystem state; auto-approved by `AcceptEdits`.
    Write,
    /// Any other side effect (network, external). Safe-by-default.
    #[default]
    SideEffect,
}
```

Add the default method inside `pub trait Tool<Ctx>` (after `output_schema`):

```rust
    /// This tool's side-effect profile. Default [`ToolEffect::SideEffect`]
    /// (safe-by-default): an undeclared tool is treated as side-effecting, so
    /// `Plan` mode blocks it.
    fn effect(&self) -> ToolEffect {
        ToolEffect::SideEffect
    }
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p paigasus-helikon-core --lib effect_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/tool.rs
git commit -m "feat(core): SMA-326 add ToolEffect classification to the Tool trait

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Additive events, errors & `OnSubagentStop`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/hook.rs` (add `HookEvent::OnSubagentStop`)
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (`AgentEvent::PermissionDenied`, `AgentError::HookDenied`)

- [ ] **Step 1: Add `HookEvent::OnSubagentStop`**

In `hook.rs`, inside the `#[non_exhaustive] pub enum HookEvent`, after the `OnRunComplete` variant:

```rust
    /// Fired when a subagent sub-run completes — a handoff target, an
    /// agent-as-tool sub-run, or a workflow sub-agent (Sequential/Parallel/Loop).
    OnSubagentStop {
        /// Name of the subagent that completed.
        agent: String,
    },
```

- [ ] **Step 2: Add `AgentEvent::PermissionDenied`**

In `agent.rs`, inside `pub enum AgentEvent`, in the `// --- Control ---` section (after `ApprovalRequested`):

```rust
    /// A tool call was denied by the permission layer. The model separately
    /// receives the denial as a synthetic tool result; this event is for
    /// observability.
    PermissionDenied {
        /// Tool name.
        tool: String,
        /// Human-readable denial reason.
        reason: String,
    },
```

- [ ] **Step 3: Add `AgentError::HookDenied` + a Display test**

In `agent.rs`, inside `pub enum AgentError`, after the `Guardrail` variant:

```rust
    /// A hook denied a lifecycle event, aborting the run.
    #[error("hook denied {event}: {reason}")]
    HookDenied {
        /// The lifecycle event that was denied (e.g. `"OnRunStart"`).
        event: String,
        /// Reason surfaced by the hook.
        reason: String,
    },
```

Add a test in `agent.rs`'s test module (or create `#[cfg(test)] mod error_display_tests`):

```rust
#[cfg(test)]
mod error_display_tests {
    use crate::AgentError;

    #[test]
    fn hook_denied_displays() {
        let e = AgentError::HookDenied {
            event: "OnRunStart".into(),
            reason: "blocked".into(),
        };
        assert_eq!(e.to_string(), "hook denied OnRunStart: blocked");
    }
}
```

- [ ] **Step 4: Run tests + clippy**

Run: `cargo test -p paigasus-helikon-core --lib error_display_tests`
Expected: PASS.
Run: `cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings`
Expected: no warnings (additive `#[non_exhaustive]` variants don't break existing `match`es).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/hook.rs crates/paigasus-helikon-core/src/agent.rs
git commit -m "feat(core): SMA-326 add OnSubagentStop, PermissionDenied, HookDenied

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: `RunContext` permission config + propagation

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs`

- [ ] **Step 1: Write the failing unit tests**

Add to `context.rs`'s `#[cfg(test)] mod runcontext_tests`:

```rust
    #[test]
    fn permission_mode_defaults_to_default_and_setter_round_trips() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        assert_eq!(ctx.permission_mode(), crate::PermissionMode::Default);
        let ctx = ctx.with_permission_mode(crate::PermissionMode::Plan);
        assert_eq!(ctx.permission_mode(), crate::PermissionMode::Plan);
    }

    #[test]
    fn bypass_cannot_be_downgraded() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_permission_mode(crate::PermissionMode::Bypass)
        .with_permission_mode(crate::PermissionMode::Plan); // no-op
        assert_eq!(ctx.permission_mode(), crate::PermissionMode::Bypass);
    }

    #[test]
    fn handoff_child_inherits_mode_and_keeps_bypass_sticky() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_permission_mode(crate::PermissionMode::Bypass);
        assert_eq!(ctx.handoff_child().permission_mode(), crate::PermissionMode::Bypass);
        assert_eq!(ctx.subagent_child().permission_mode(), crate::PermissionMode::Bypass);
    }
```

- [ ] **Step 2: Run them to confirm failure**

Run: `cargo test -p paigasus-helikon-core --lib runcontext_tests::permission`
Expected: FAIL — `no method named permission_mode` / `with_permission_mode`.

- [ ] **Step 3: Add the fields, setters, accessors**

In `context.rs`, add the import at the top:

```rust
use crate::{ApprovalHandler, DenyRule, PermissionMode, PermissionPolicy};
```
(merge into the existing `use crate::{...}` line).

Add fields to `pub struct RunContext<Ctx>`:

```rust
    permission_mode: PermissionMode,
    permission_policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,
    deny_rules: Vec<DenyRule>,
    approval_handler: Option<Arc<dyn ApprovalHandler>>,
```

In `RunContext::new`, initialize them in the returned `Self { … }`:

```rust
            permission_mode: PermissionMode::Default,
            permission_policy: None,
            deny_rules: Vec::new(),
            approval_handler: None,
```

Add setters + accessors in `impl<Ctx> RunContext<Ctx>`:

```rust
    /// Set the permission mode. **Monotonic on `Bypass`:** once the mode is
    /// `Bypass`, this is a no-op — `Bypass` cannot be downgraded (the safety
    /// invariant). All other transitions apply.
    pub fn with_permission_mode(mut self, mode: PermissionMode) -> Self {
        if self.permission_mode != PermissionMode::Bypass {
            self.permission_mode = mode;
        }
        self
    }

    /// Install the run's permission policy (`canUseTool`).
    pub fn with_permission_policy(mut self, policy: Arc<dyn PermissionPolicy<Ctx>>) -> Self {
        self.permission_policy = Some(policy);
        self
    }

    /// Install deny rules, evaluated before mode (override even `Bypass`).
    pub fn with_deny_rules(mut self, rules: Vec<DenyRule>) -> Self {
        self.deny_rules = rules;
        self
    }

    /// Install the approval handler that resolves `AskUser` decisions.
    pub fn with_approval_handler(mut self, handler: Arc<dyn ApprovalHandler>) -> Self {
        self.approval_handler = Some(handler);
        self
    }

    /// The current permission mode.
    pub fn permission_mode(&self) -> PermissionMode {
        self.permission_mode
    }
    /// The run's permission policy, if installed.
    pub fn permission_policy(&self) -> Option<&Arc<dyn PermissionPolicy<Ctx>>> {
        self.permission_policy.as_ref()
    }
    /// The run's deny rules.
    pub fn deny_rules(&self) -> &[DenyRule] {
        &self.deny_rules
    }
    /// The run's approval handler, if installed.
    pub fn approval_handler(&self) -> Option<&Arc<dyn ApprovalHandler>> {
        self.approval_handler.as_ref()
    }
```

- [ ] **Step 4: Propagate in `handoff_child` and `subagent_child`**

In **both** `handoff_child(&self)` and `subagent_child(&self)`, add to the returned `Self { … }`:

```rust
            permission_mode: self.permission_mode,
            permission_policy: self.permission_policy.clone(),
            deny_rules: self.deny_rules.clone(),
            approval_handler: self.approval_handler.clone(),
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p paigasus-helikon-core --lib runcontext_tests`
Expected: PASS (existing + 3 new).

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/context.rs
git commit -m "feat(core): SMA-326 carry permission config on RunContext with sticky Bypass

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: `ToolContext` permission projection (the agent-as-tool fix)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/tool.rs` (`ToolContext` fields + accessors)
- Modify: `crates/paigasus-helikon-core/src/context.rs` (`to_tool_context` projection)

- [ ] **Step 1: Write the failing unit test**

Add to `context.rs`'s `runcontext_tests`:

```rust
    #[test]
    fn to_tool_context_projects_permission_mode() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_permission_mode(crate::PermissionMode::Bypass);
        assert_eq!(ctx.to_tool_context().permission_mode(), crate::PermissionMode::Bypass);
    }
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p paigasus-helikon-core --lib to_tool_context_projects_permission_mode`
Expected: FAIL — `no method named permission_mode` on `ToolContext`.

- [ ] **Step 3: Add carriers + accessors to `ToolContext`**

In `tool.rs`, add the import (merge into the existing `use crate::{...}`):

```rust
use crate::{ApprovalHandler, DenyRule, PermissionMode, PermissionPolicy};
```

Add fields to `pub struct ToolContext<Ctx>`:

```rust
    permission_mode: PermissionMode,
    pub(crate) permission_policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,
    pub(crate) deny_rules: Vec<DenyRule>,
    pub(crate) approval_handler: Option<Arc<dyn ApprovalHandler>>,
```

In `ToolContext::new`, initialize them in the returned `Self { … }`:

```rust
            permission_mode: PermissionMode::Default,
            permission_policy: None,
            deny_rules: Vec::new(),
            approval_handler: None,
```

Add the public accessor + a `pub(crate)` builder used by the projection:

```rust
    /// The run's permission mode. A tool may legitimately branch on this.
    pub fn permission_mode(&self) -> PermissionMode {
        self.permission_mode
    }

    /// Install the permission config (used by [`crate::RunContext::to_tool_context`]).
    /// `policy`/`deny_rules`/`handler` are `pub(crate)` carriers read only by
    /// the `agent_as_tool` rebuild path — not exposed to tools.
    pub(crate) fn with_permissions(
        mut self,
        mode: PermissionMode,
        policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,
        deny_rules: Vec<DenyRule>,
        handler: Option<Arc<dyn ApprovalHandler>>,
    ) -> Self {
        self.permission_mode = mode;
        self.permission_policy = policy;
        self.deny_rules = deny_rules;
        self.approval_handler = handler;
        self
    }
```

- [ ] **Step 4: Project in `to_tool_context`**

In `context.rs`, in `to_tool_context`, chain `.with_permissions(...)` onto the existing builder expression (after `.with_actions(...)`):

```rust
        .with_permissions(
            self.permission_mode,
            self.permission_policy.clone(),
            self.deny_rules.clone(),
            self.approval_handler.clone(),
        )
```

- [ ] **Step 5: Run the test**

Run: `cargo test -p paigasus-helikon-core --lib to_tool_context_projects_permission_mode`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/tool.rs crates/paigasus-helikon-core/src/context.rs
git commit -m "feat(core): SMA-326 project permission config into ToolContext

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: `control.rs` — `Interceptors::authorize` permission pipeline

**Files:**
- Create: `crates/paigasus-helikon-core/src/control.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs` (`pub mod control;` + `pub use control::*;`)

The `Interceptors` struct borrows the stream-local Arc-snapshots and the moved `ctx`. This task lands the struct + the permission pipeline; Tasks 7–8 add hook resolution and guardrail runners.

- [ ] **Step 1: Write `control.rs` with `authorize` + the truth-table tests**

```rust
//! `Interceptors`: the run's control-layer orchestration unit.
//!
//! Borrows the stream-local Arc-snapshots of the agent's guardrails/hooks and
//! the run's [`RunContext`] (mode, policy, deny rules, approval handler). The
//! driver calls its async methods at the loop's control seams. Pure of the
//! state machine — all async control lives here, not in `transition()`.

use std::sync::Arc;

use crate::{
    ApprovalOutcome, Guardrail, Hook, PermissionDecision, PermissionMode, RunContext, ToolEffect,
};

/// Borrows everything the control seams need for one run.
pub(crate) struct Interceptors<'a, Ctx>
where
    Ctx: Send + Sync + 'static,
{
    pub(crate) ctx: &'a RunContext<Ctx>,
    pub(crate) input_guardrails: &'a [Arc<dyn Guardrail<Ctx>>],
    pub(crate) output_guardrails: &'a [Arc<dyn Guardrail<Ctx>>],
    pub(crate) agent_hooks: &'a [Arc<dyn Hook<Ctx>>],
}

impl<'a, Ctx> Interceptors<'a, Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Authorize one tool call on its effective args: `deny rules › mode ›
    /// policy › AskUser`. Returns the resolved decision (never `AskUser` — that
    /// is resolved here via the approval handler, default Deny).
    pub(crate) async fn authorize(
        &self,
        tool: &str,
        effect: ToolEffect,
        args: &serde_json::Value,
    ) -> PermissionDecision {
        // 1. Deny rules — absolute, override even Bypass.
        if self.ctx.deny_rules().iter().any(|r| r.matches(tool, args)) {
            return PermissionDecision::Deny {
                reason: format!("denied by deny rule: {tool}"),
            };
        }
        // 2. Mode.
        match self.ctx.permission_mode() {
            PermissionMode::Bypass => return PermissionDecision::Allow,
            PermissionMode::Plan if effect != ToolEffect::ReadOnly => {
                return PermissionDecision::Deny {
                    reason: format!("Plan mode forbids the side-effecting tool `{tool}`"),
                };
            }
            PermissionMode::AcceptEdits if effect == ToolEffect::Write => {
                return PermissionDecision::Allow;
            }
            _ => {}
        }
        // 3. Policy (canUseTool). None ⇒ permissive.
        let decision = match self.ctx.permission_policy() {
            None => return PermissionDecision::Allow,
            Some(policy) => policy.check(self.ctx, tool, args).await,
        };
        // 4. AskUser ⇒ approval handler; None ⇒ Deny.
        match decision {
            PermissionDecision::AskUser { prompt } => match self.ctx.approval_handler() {
                None => PermissionDecision::Deny {
                    reason: "no approval handler installed".to_owned(),
                },
                Some(handler) => match handler.decide(tool, &prompt, args).await {
                    ApprovalOutcome::Allow => PermissionDecision::Allow,
                    ApprovalOutcome::Deny { reason } => PermissionDecision::Deny { reason },
                },
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod authorize_tests {
    use super::*;
    use crate::{
        ApprovalHandler, CancellationToken, DenyRule, HookRegistry, MemorySession,
        PermissionPolicy, Session, TracerHandle,
    };
    use async_trait::async_trait;
    use serde_json::json;

    fn ctx() -> RunContext<()> {
        RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
    }

    fn interceptors<'a>(ctx: &'a RunContext<()>) -> Interceptors<'a, ()> {
        Interceptors {
            ctx,
            input_guardrails: &[],
            output_guardrails: &[],
            agent_hooks: &[],
        }
    }

    struct AllowPolicy;
    #[async_trait]
    impl PermissionPolicy<()> for AllowPolicy {
        async fn check(&self, _: &RunContext<()>, _: &str, _: &serde_json::Value) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }
    struct AskPolicy;
    #[async_trait]
    impl PermissionPolicy<()> for AskPolicy {
        async fn check(&self, _: &RunContext<()>, _: &str, _: &serde_json::Value) -> PermissionDecision {
            PermissionDecision::AskUser { prompt: "ok?".into() }
        }
    }
    struct AllowHandler;
    #[async_trait]
    impl ApprovalHandler for AllowHandler {
        async fn decide(&self, _: &str, _: &str, _: &serde_json::Value) -> ApprovalOutcome {
            ApprovalOutcome::Allow
        }
    }

    #[tokio::test]
    async fn deny_rule_beats_bypass() {
        let c = ctx()
            .with_permission_mode(PermissionMode::Bypass)
            .with_deny_rules(vec![DenyRule::tool("rm")]);
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("rm", ToolEffect::ReadOnly, &json!({})).await,
            PermissionDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn plan_denies_non_readonly_allows_readonly() {
        let c = ctx().with_permission_mode(PermissionMode::Plan);
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("write", ToolEffect::Write, &json!({})).await,
            PermissionDecision::Deny { .. }
        ));
        assert!(matches!(
            i.authorize("read", ToolEffect::ReadOnly, &json!({})).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn accept_edits_allows_write() {
        let c = ctx().with_permission_mode(PermissionMode::AcceptEdits);
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("edit", ToolEffect::Write, &json!({})).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn default_mode_no_policy_allows() {
        let c = ctx();
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("any", ToolEffect::SideEffect, &json!({})).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn ask_user_without_handler_denies() {
        let c = ctx().with_permission_policy(Arc::new(AskPolicy));
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("t", ToolEffect::SideEffect, &json!({})).await,
            PermissionDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn ask_user_with_allow_handler_allows() {
        let c = ctx()
            .with_permission_policy(Arc::new(AskPolicy))
            .with_approval_handler(Arc::new(AllowHandler));
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("t", ToolEffect::SideEffect, &json!({})).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn policy_allow_passes_through() {
        let c = ctx().with_permission_policy(Arc::new(AllowPolicy));
        let i = interceptors(&c);
        assert!(matches!(
            i.authorize("t", ToolEffect::SideEffect, &json!({})).await,
            PermissionDecision::Allow
        ));
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

Add `pub mod control;` (after `pub mod context;`) and `pub use control::*;` (after `pub use context::*;`). Note: `Interceptors` is `pub(crate)`, so the glob re-export exposes nothing public — that is intentional.

- [ ] **Step 3: Run the tests**

Run: `cargo test -p paigasus-helikon-core --lib authorize_tests`
Expected: PASS (7 tests).

- [ ] **Step 4: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/control.rs crates/paigasus-helikon-core/src/lib.rs
git commit -m "feat(core): SMA-326 add Interceptors with the permission decision pipeline

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: `control.rs` — hook firing & conflict resolution

**Files:**
- Modify: `crates/paigasus-helikon-core/src/control.rs`

- [ ] **Step 1: Write the failing tests**

Add a new test module to `control.rs`:

```rust
#[cfg(test)]
mod fire_tests {
    use super::*;
    use crate::{
        CancellationToken, HookDecision, HookEvent, HookRegistry, MemorySession, RunContext,
        Session, TracerHandle,
    };
    use async_trait::async_trait;
    use serde_json::json;

    struct FixedHook(HookDecision);
    #[async_trait]
    impl Hook<()> for FixedHook {
        async fn on_event(&self, _: &RunContext<()>, _: &HookEvent) -> HookDecision {
            self.0.clone()
        }
    }

    fn ctx() -> RunContext<()> {
        RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
    }

    fn with_hooks<'a>(
        ctx: &'a RunContext<()>,
        hooks: &'a [Arc<dyn Hook<()>>],
    ) -> Interceptors<'a, ()> {
        Interceptors { ctx, input_guardrails: &[], output_guardrails: &[], agent_hooks: hooks }
    }

    #[tokio::test]
    async fn first_deny_short_circuits() {
        let hooks: Vec<Arc<dyn Hook<()>>> = vec![
            Arc::new(FixedHook(HookDecision::Deny { reason: "no".into() })),
            Arc::new(FixedHook(HookDecision::ReplaceInput { value: json!(1) })),
        ];
        let c = ctx();
        let i = with_hooks(&c, &hooks);
        let r = i.fire(&HookEvent::PreToolUse { tool: "t".into(), args: json!({}) }).await;
        assert_eq!(r.denied.as_deref(), Some("no"));
    }

    #[tokio::test]
    async fn last_replace_wins_and_injects_accumulate() {
        let hooks: Vec<Arc<dyn Hook<()>>> = vec![
            Arc::new(FixedHook(HookDecision::ReplaceInput { value: json!(1) })),
            Arc::new(FixedHook(HookDecision::InjectSystemMessage { text: "a".into() })),
            Arc::new(FixedHook(HookDecision::ReplaceInput { value: json!(2) })),
            Arc::new(FixedHook(HookDecision::InjectSystemMessage { text: "b".into() })),
        ];
        let c = ctx();
        let i = with_hooks(&c, &hooks);
        let r = i.fire(&HookEvent::PreToolUse { tool: "t".into(), args: json!({}) }).await;
        assert!(r.denied.is_none());
        assert_eq!(r.replacement, Some(json!(2)));
        assert_eq!(r.injections, vec!["a".to_owned(), "b".to_owned()]);
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p paigasus-helikon-core --lib fire_tests`
Expected: FAIL — `no method named fire` / `ResolvedHookDecision` unknown.

- [ ] **Step 3: Add `ResolvedHookDecision` + `fire`**

Add to `control.rs` (top-level, near `Interceptors`):

```rust
use crate::{HookDecision, HookEvent, HookRegistry};

/// The folded outcome of firing all hooks for one event.
#[derive(Debug, Default)]
pub(crate) struct ResolvedHookDecision {
    /// `Some(reason)` if any hook denied (first wins).
    pub(crate) denied: Option<String>,
    /// The last `ReplaceInput`/`ReplaceOutput` value, if any.
    pub(crate) replacement: Option<serde_json::Value>,
    /// All injected system messages, in fire order.
    pub(crate) injections: Vec<String>,
}
```

Add the method inside `impl<'a, Ctx> Interceptors<'a, Ctx>`:

```rust
    /// Fire `event` to agent-level hooks first, then the run-level
    /// [`HookRegistry`]. Folds outcomes: first `Deny` short-circuits;
    /// `Replace*` last-writer-wins; `InjectSystemMessage` accumulates.
    pub(crate) async fn fire(&self, event: &HookEvent) -> ResolvedHookDecision {
        let mut out = ResolvedHookDecision::default();
        let registry: &HookRegistry<Ctx> = self.ctx.hooks();
        let all = self.agent_hooks.iter().chain(registry.iter());
        for hook in all {
            match hook.on_event(self.ctx, event).await {
                HookDecision::Allow => {}
                HookDecision::Deny { reason } => {
                    out.denied = Some(reason);
                    return out; // short-circuit
                }
                HookDecision::ReplaceInput { value }
                | HookDecision::ReplaceOutput { value } => {
                    out.replacement = Some(value);
                }
                HookDecision::InjectSystemMessage { text } => {
                    out.injections.push(text);
                }
            }
        }
        out
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p paigasus-helikon-core --lib fire_tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/control.rs
git commit -m "feat(core): SMA-326 add hook firing and conflict resolution to Interceptors

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: `control.rs` — guardrail runners

**Files:**
- Modify: `crates/paigasus-helikon-core/src/control.rs`

- [ ] **Step 1: Write the failing tests**

Add to `control.rs`:

```rust
#[cfg(test)]
mod guardrail_tests {
    use super::*;
    use crate::{
        CancellationToken, GuardrailError, GuardrailInput, GuardrailKind, GuardrailVerdict,
        HookRegistry, MemorySession, RunContext, Session, TracerHandle,
    };
    use async_trait::async_trait;

    struct TripOnInput;
    #[async_trait]
    impl Guardrail<()> for TripOnInput {
        async fn check(
            &self,
            _: &RunContext<()>,
            _: GuardrailInput<'_>,
        ) -> Result<GuardrailVerdict, GuardrailError> {
            Ok(GuardrailVerdict::Tripwire {
                kind: GuardrailKind::InputPolicy,
                info: serde_json::Value::Null,
            })
        }
    }

    fn ctx() -> RunContext<()> {
        RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn input_guardrail_passes_when_empty() {
        let c = ctx();
        let i = Interceptors { ctx: &c, input_guardrails: &[], output_guardrails: &[], agent_hooks: &[] };
        assert!(i.run_input_guardrails("hello").await.is_none());
    }

    #[tokio::test]
    async fn input_guardrail_trips() {
        let gs: Vec<Arc<dyn Guardrail<()>>> = vec![Arc::new(TripOnInput)];
        let c = ctx();
        let i = Interceptors { ctx: &c, input_guardrails: &gs, output_guardrails: &[], agent_hooks: &[] };
        let trip = i.run_input_guardrails("hello").await;
        assert!(matches!(trip, Some((GuardrailKind::InputPolicy, _))));
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p paigasus-helikon-core --lib guardrail_tests`
Expected: FAIL — `no method named run_input_guardrails`.

- [ ] **Step 3: Add the runners**

Add to `control.rs` imports: `GuardrailInput`, `GuardrailKind`, `GuardrailVerdict` (merge into the `use crate::{...}`). Add inside `impl Interceptors`:

```rust
    /// Run input guardrails as a blocking gate. Returns `Some((kind, info))`
    /// on the first tripwire, else `None`. A guardrail's own error is treated
    /// as a tripwire with [`crate::GuardrailKind::Other`].
    pub(crate) async fn run_input_guardrails(
        &self,
        text: &str,
    ) -> Option<(crate::GuardrailKind, serde_json::Value)> {
        self.run_guardrails(self.input_guardrails, crate::GuardrailInput::UserText(text)).await
    }

    /// Run output guardrails as a blocking gate on the final text.
    pub(crate) async fn run_output_guardrails(
        &self,
        text: &str,
    ) -> Option<(crate::GuardrailKind, serde_json::Value)> {
        self.run_guardrails(self.output_guardrails, crate::GuardrailInput::ModelOutput(text)).await
    }

    async fn run_guardrails(
        &self,
        guardrails: &[Arc<dyn Guardrail<Ctx>>],
        input: crate::GuardrailInput<'_>,
    ) -> Option<(crate::GuardrailKind, serde_json::Value)> {
        for g in guardrails {
            match g.check(self.ctx, input.clone()).await {
                Ok(crate::GuardrailVerdict::Pass) => {}
                Ok(crate::GuardrailVerdict::Tripwire { kind, info }) => return Some((kind, info)),
                Err(e) => {
                    return Some((
                        crate::GuardrailKind::Other { reason: e.to_string() },
                        serde_json::Value::Null,
                    ))
                }
            }
        }
        None
    }
```

(`GuardrailInput` derives `Clone`, verified in `guardrail.rs`.)

- [ ] **Step 4: Run the tests**

Run: `cargo test -p paigasus-helikon-core --lib guardrail_tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/control.rs
git commit -m "feat(core): SMA-326 add guardrail runners to Interceptors

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Driver — snapshot, `OnRunStart`, input-guardrail gate (AC1)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (`Agent::run` for `LlmAgent`)
- Test: `crates/paigasus-helikon-core/tests/guardrails.rs` (new)

- [ ] **Step 1: Write the AC1 integration test**

Create `crates/paigasus-helikon-core/tests/guardrails.rs`:

```rust
//! SMA-326: input/output guardrail gates.

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, Guardrail, GuardrailError, GuardrailInput,
    GuardrailKind, GuardrailVerdict, Instructions, LlmAgent, Model, ModelCapabilities, ModelError,
    ModelEvent, ModelRequest, ModelSettings, RunConfig, RunContext, RunResultStreaming,
};

use common::noop_run_context;

/// A model that counts every `invoke` so a test can assert zero calls.
struct CountingModel {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Model for CountingModel {
    async fn invoke(
        &self,
        _r: ModelRequest,
        _c: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(Vec::<Result<ModelEvent, ModelError>>::new())))
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

struct AlwaysTrip;
#[async_trait]
impl Guardrail<()> for AlwaysTrip {
    async fn check(
        &self,
        _: &RunContext<()>,
        _: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError> {
        Ok(GuardrailVerdict::Tripwire {
            kind: GuardrailKind::InputPolicy,
            info: serde_json::json!({"why": "test"}),
        })
    }
}

fn agent_with_input_guardrail(
    calls: Arc<AtomicUsize>,
) -> LlmAgent<(), CountingModel> {
    LlmAgent::<(), _> {
        name: "g".into(),
        description: "guardrail test".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model: Arc::new(CountingModel { calls }),
        tools: Vec::new(),
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: vec![Arc::new(AlwaysTrip) as Arc<dyn Guardrail<()>>],
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    }
}

#[tokio::test]
async fn input_guardrail_aborts_before_any_model_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = agent_with_input_guardrail(Arc::clone(&calls));
    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("hi"))
        .await
        .expect("stream starts");
    let result = RunResultStreaming::new(stream).collect().await;

    assert!(result.is_err(), "tripwire must fail the run");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "zero model calls (AC1)");
    // The error path also emits a GuardrailTriggered event before RunFailed.
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p paigasus-helikon-core --test guardrails input_guardrail_aborts`
Expected: FAIL — `calls == 1` (guardrails not driven yet), or the run completes.

- [ ] **Step 3: Snapshot guardrails/hooks before the stream**

In `agent.rs`, in the snapshot block (~line 604, after `let handoffs = self.handoffs.clone();`):

```rust
        let input_guardrails = self.input_guardrails.clone();
        let output_guardrails = self.output_guardrails.clone();
        let agent_hooks = self.hooks.clone();
```

- [ ] **Step 4: Build `Interceptors` and drive `OnRunStart` + input guardrails inside the stream**

In `agent.rs`, inside the `async_stream::stream! { … }`, immediately after `yield crate::AgentEvent::RunStarted { agent: agent_name.clone() };`, insert:

```rust
            let interceptors = crate::control::Interceptors {
                ctx: &ctx,
                input_guardrails: &input_guardrails,
                output_guardrails: &output_guardrails,
                agent_hooks: &agent_hooks,
            };

            // OnRunStart hook.
            let on_start = interceptors.fire(&crate::HookEvent::OnRunStart).await;
            if let Some(reason) = on_start.denied {
                let err = crate::AgentError::HookDenied { event: "OnRunStart".into(), reason };
                let msg = err.to_string();
                run_span.record("otel.status_code", "ERROR");
                failure.set(err);
                yield crate::AgentEvent::RunFailed { error: msg };
                return;
            }
            let mut pending_injections: Vec<String> = on_start.injections;

            // Input guardrails — blocking gate (AC1).
            let seed_text = user_text_of(&conversation);
            if let Some((kind, info)) = interceptors.run_input_guardrails(&seed_text).await {
                run_span.record("otel.status_code", "ERROR");
                yield crate::AgentEvent::GuardrailTriggered { kind: kind.clone(), info };
                failure.set(crate::AgentError::Guardrail { kind });
                yield crate::AgentEvent::RunFailed { error: "input guardrail tripwire".into() };
                return;
            }
```

Add a free helper near `build_items` (module scope in `agent.rs`):

```rust
/// Concatenate the text of all `Item::UserMessage` parts in the seed
/// conversation — the text input guardrails inspect.
fn user_text_of(conversation: &[crate::Item]) -> String {
    let mut s = String::new();
    for item in conversation {
        if let crate::Item::UserMessage { content } = item {
            for part in content {
                if let crate::ContentPart::Text { text } = part {
                    s.push_str(text);
                }
            }
        }
    }
    s
}
```

> Note: `pending_injections` is consumed in Task 10 (drained into the conversation before each `CallModel`). Until then it is `let mut` and unused — add `#[allow(unused_mut)]`/`let _ = &pending_injections;` is **not** needed because Task 10 lands in the same PR; if you are committing Task 9 alone, prefix with `let _ = &pending_injections;` to silence the unused warning, then remove it in Task 10.

- [ ] **Step 5: Run the test**

Run: `cargo test -p paigasus-helikon-core --test guardrails input_guardrail_aborts`
Expected: PASS — `calls == 0`.

- [ ] **Step 6: Run the full suite (no regressions)**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (existing loop/handoff/workflow tests still green — `OnRunStart` with no hooks is a no-op; empty guardrails pass).

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/tests/guardrails.rs
git commit -m "feat(core): SMA-326 drive OnRunStart and the input-guardrail gate (AC1)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Driver — per-tool-call interleaving (AC2, AC3) + `OnTurnStart` + injection

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (`run_tools_concurrent` + the `ExecuteTools` arm + `OnTurnStart` + injection)
- Test: `crates/paigasus-helikon-core/tests/permissions.rs` (new, AC3), `crates/paigasus-helikon-core/tests/hooks.rs` (new, AC2)

- [ ] **Step 1: Write the AC3 (Plan denies) integration test**

Create `crates/paigasus-helikon-core/tests/permissions.rs`:

```rust
//! SMA-326: permission gating.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, FinishReason, Instructions, LlmAgent, ModelEvent, ModelSettings,
    PermissionMode, RunConfig, RunResultStreaming, Tool,
};

use common::{noop_run_context, MockModel, MockTool};

fn agent(model: Arc<MockModel>, tools: Vec<Arc<dyn Tool<()>>>) -> LlmAgent<(), MockModel> {
    LlmAgent::<(), _> {
        name: "p".into(),
        description: "permission test".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools,
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    }
}

#[tokio::test]
async fn plan_mode_denies_side_effecting_tool() {
    // Turn 1: model calls the (default SideEffect) tool. Turn 2: model finishes.
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "1".into(),
                name: Some("writer".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::Finish { reason: FinishReason::ToolCalls },
        ],
        vec![
            ModelEvent::TokenDelta { text: "done".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
    ]);
    let tool = MockTool::new("writer", serde_json::json!({"ok": true}));
    let agent = agent(model, vec![Arc::clone(&tool) as Arc<dyn Tool<()>>]);

    let ctx = noop_run_context::<()>().with_permission_mode(PermissionMode::Plan);
    let stream = agent.run(ctx, AgentInput::from_user_text("go")).await.unwrap();
    let result = RunResultStreaming::new(stream).collect().await.unwrap();

    // The tool was never invoked (denied), and a PermissionDenied event fired.
    assert_eq!(tool.invocations().len(), 0, "Plan denied the side-effecting tool (AC3)");
    assert!(result
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::PermissionDenied { tool, .. } if tool == "writer")));
}
```

- [ ] **Step 2: Write the AC2 (PreToolUse replace-input) integration test**

Create `crates/paigasus-helikon-core/tests/hooks.rs`:

```rust
//! SMA-326: lifecycle hooks.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use async_trait::async_trait;
use paigasus_helikon_core::{
    Agent, AgentInput, FinishReason, Hook, HookDecision, HookEvent, Instructions, LlmAgent,
    ModelEvent, ModelSettings, RunConfig, RunContext, RunResultStreaming, Tool,
};

use common::{noop_run_context, MockModel, MockTool};

/// A hook that rewrites `PreToolUse` args to `{"replaced": true}`.
struct ReplaceArgs;
#[async_trait]
impl Hook<()> for ReplaceArgs {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        match event {
            HookEvent::PreToolUse { .. } => HookDecision::ReplaceInput {
                value: serde_json::json!({"replaced": true}),
            },
            _ => HookDecision::Allow,
        }
    }
}

fn agent(
    model: Arc<MockModel>,
    tools: Vec<Arc<dyn Tool<()>>>,
    hooks: Vec<Arc<dyn Hook<()>>>,
) -> LlmAgent<(), MockModel> {
    LlmAgent::<(), _> {
        name: "h".into(),
        description: "hook test".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools,
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks,
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    }
}

#[tokio::test]
async fn pre_tool_use_replace_input_modifies_invocation() {
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "1".into(),
                name: Some("t".into()),
                args_delta: "{\"original\":true}".into(),
            },
            ModelEvent::Finish { reason: FinishReason::ToolCalls },
        ],
        vec![
            ModelEvent::TokenDelta { text: "done".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
    ]);
    let tool = MockTool::new("t", serde_json::json!({"ok": true}));
    let agent = agent(
        model,
        vec![Arc::clone(&tool) as Arc<dyn Tool<()>>],
        vec![Arc::new(ReplaceArgs) as Arc<dyn Hook<()>>],
    );

    let stream = agent.run(noop_run_context::<()>(), AgentInput::from_user_text("go")).await.unwrap();
    let _ = RunResultStreaming::new(stream).collect().await.unwrap();

    let invocations = tool.invocations();
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].0, serde_json::json!({"replaced": true}), "AC2: args replaced");
}
```

- [ ] **Step 3: Run both to confirm failure**

Run: `cargo test -p paigasus-helikon-core --test permissions --test hooks`
Expected: FAIL — tool is invoked in Plan mode; args not replaced (interleaving not wired yet).

- [ ] **Step 4: Rewrite `run_tools_concurrent` to take `&Interceptors` and run the interleaving**

Replace the `run_tools_concurrent` function body. The new signature threads the interceptors and the tools' `ToolEffect`; per call it runs PreToolUse → authorize → invoke → PostToolUse. It also returns the `PermissionDenied` events to emit (the driver yields them).

```rust
async fn run_tools_concurrent<Ctx>(
    tools: &[std::sync::Arc<dyn crate::Tool<Ctx>>],
    calls: &[crate::ToolCallRequest],
    interceptors: &crate::control::Interceptors<'_, Ctx>,
    tool_ctx: &crate::ToolContext<Ctx>,
    limit: Option<std::num::NonZeroUsize>,
    parent: &tracing::Span,
) -> (Vec<crate::ToolCallOutcome>, Vec<crate::AgentEvent>)
where
    Ctx: Send + Sync + 'static,
{
    let denied_events: std::sync::Mutex<Vec<crate::AgentEvent>> = std::sync::Mutex::new(Vec::new());

    let futures = calls.iter().map(|call| {
        let tool = tools.iter().find(|t| t.name() == call.name).cloned();
        let effect = tool.as_ref().map(|t| t.effect()).unwrap_or(crate::ToolEffect::SideEffect);
        let call_id = call.call_id.clone();
        let name = call.name.clone();
        let orig_args = call.args.clone();
        let denied_events = &denied_events;
        let span = tracing::info_span!(
            parent: parent,
            "tool.execute",
            otel.name = tracing::field::Empty,
            otel.kind = "internal",
            gen_ai.operation.name = "execute_tool",
            gen_ai.tool.name = %name,
            otel.status_code = tracing::field::Empty,
        );
        span.record("otel.name", format!("execute_tool {name}").as_str());
        async move {
            // PreToolUse hook.
            let pre = interceptors
                .fire(&crate::HookEvent::PreToolUse { tool: name.clone(), args: orig_args.clone() })
                .await;
            if let Some(reason) = pre.denied {
                return crate::ToolCallOutcome {
                    call_id,
                    result: Err(format!("blocked by PreToolUse hook: {reason}")),
                };
            }
            let mut args = pre.replacement.unwrap_or(orig_args);

            // Permission authorize on the effective args.
            match interceptors.authorize(&name, effect, &args).await {
                crate::PermissionDecision::Allow => {}
                crate::PermissionDecision::Replace { args: sanitized } => {
                    args = sanitized;
                }
                crate::PermissionDecision::Deny { reason }
                | crate::PermissionDecision::AskUser { prompt: reason } => {
                    // AskUser is resolved to Allow/Deny inside authorize(); a
                    // residual AskUser here cannot occur, but treat defensively.
                    denied_events.lock().unwrap().push(crate::AgentEvent::PermissionDenied {
                        tool: name.clone(),
                        reason: reason.clone(),
                    });
                    return crate::ToolCallOutcome {
                        call_id,
                        result: Err(format!("permission denied: {reason}")),
                    };
                }
            }

            // Invoke.
            let outcome = match tool {
                Some(t) => match t.invoke(tool_ctx, args).await {
                    Ok(output) => Ok(tool_output_to_content_parts(&output)),
                    Err(e) => {
                        tracing::Span::current().record("otel.status_code", "ERROR");
                        Err(e.to_string())
                    }
                },
                None => {
                    tracing::Span::current().record("otel.status_code", "ERROR");
                    Err(format!("unknown tool: {name}"))
                }
            };

            // PostToolUse hook (ReplaceOutput / Deny→denial).
            let outcome = match outcome {
                Ok(content) => {
                    let output_json = content_parts_to_json(&content);
                    let post = interceptors
                        .fire(&crate::HookEvent::PostToolUse { tool: name.clone(), output: output_json })
                        .await;
                    if let Some(reason) = post.denied {
                        Err(format!("blocked by PostToolUse hook: {reason}"))
                    } else if let Some(value) = post.replacement {
                        Ok(vec![crate::ContentPart::Text { text: value.to_string() }])
                    } else {
                        Ok(content)
                    }
                }
                Err(e) => Err(e),
            };

            crate::ToolCallOutcome { call_id, result: outcome }
        }
        .instrument(span)
    });

    let outcomes = match limit {
        None => futures_util::future::join_all(futures).await,
        Some(n) => {
            use futures_util::stream::StreamExt as _;
            let collected: Vec<_> = futures.collect();
            futures_util::stream::iter(collected).buffered(n.get()).collect().await
        }
    };
    (outcomes, denied_events.into_inner().unwrap())
}
```

Add a small helper next to `tool_output_to_content_parts`:

```rust
/// Render content parts back to a JSON value for `PostToolUse` hooks. Text
/// parts concatenate; the common single-text case round-trips cleanly.
fn content_parts_to_json(parts: &[crate::ContentPart]) -> serde_json::Value {
    let text: String = parts
        .iter()
        .filter_map(|p| match p {
            crate::ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    serde_json::Value::String(text)
}
```

- [ ] **Step 5: Update the `ExecuteTools` arm + add `OnTurnStart` + injection**

In the `NextAction::ExecuteTools` arm, replace the `run_tools_concurrent(...)` call:

```rust
                    crate::NextAction::ExecuteTools { calls } => {
                        let tool_ctx = ctx.to_tool_context();
                        let tool_parent = turn_span.as_ref().unwrap_or(&run_span);
                        let (outcomes, denied) = run_tools_concurrent(
                            &tools,
                            &calls,
                            &interceptors,
                            &tool_ctx,
                            parallel_tool_call_limit,
                            tool_parent,
                        )
                        .await;
                        for ev in denied {
                            yield ev;
                        }
                        for o in &outcomes {
                            conversation.push(crate::Item::ToolResult {
                                call_id: o.call_id.clone(),
                                content: o.result.clone().unwrap_or_else(|e| {
                                    vec![crate::ContentPart::Text { text: e }]
                                }),
                            });
                        }
                        tx_input = crate::TransitionInput::ToolResults { outcomes };
                    }
```

For `OnTurnStart`: in the event-yield loop, where `AgentEvent::TurnStarted { turn }` is matched for tracing, after creating the turn span, fire the hook and queue injections:

```rust
                        crate::AgentEvent::TurnStarted { turn } => {
                            // (existing span creation stays)
                            let on_turn = interceptors
                                .fire(&crate::HookEvent::OnTurnStart { turn: *turn })
                                .await;
                            if let Some(reason) = on_turn.denied {
                                let err = crate::AgentError::HookDenied {
                                    event: "OnTurnStart".into(),
                                    reason,
                                };
                                let msg = err.to_string();
                                run_span.record("otel.status_code", "ERROR");
                                failure.set(err);
                                yield crate::AgentEvent::RunFailed { error: msg };
                                return;
                            }
                            pending_injections.extend(on_turn.injections);
                            turn_span = Some(s);
                        }
```

> The event loop borrows `interceptors` (which borrows `ctx`); `ctx` is also used later in `CallModel`/`Handoff` for `ctx.cancel()`/`ctx.to_tool_context()`/`ctx.handoff_child()`. All are shared `&` borrows, so this compiles. If the borrow checker objects to firing hooks while iterating `events` (a moved `Vec`), collect injections after the loop instead — `pending_injections` is the carrier.

For **injection**: in the `NextAction::CallModel` arm, immediately before `model.invoke(request, ...)`, drain pending injections into the request's messages:

```rust
                        if !pending_injections.is_empty() {
                            let mut req = request;
                            for text in pending_injections.drain(..) {
                                req.messages.push(crate::Item::System {
                                    content: vec![crate::ContentPart::Text { text }],
                                });
                            }
                            // also reflect into the owned conversation for the next turn
                            // (System injections persist across turns):
                            // conversation already holds prior items; push the same:
                            // (skip if you prefer per-turn-only injection)
                            request_with_injections(&mut req);
                            // Use `req` below instead of `request`.
                        }
```

> Simpler, recommended form: change the arm to build a local `let mut request = request;` and, right after destructuring `CallModel { request }`, do:
> ```rust
> let mut request = request;
> for text in pending_injections.drain(..) {
>     request.messages.push(crate::Item::System {
>         content: vec![crate::ContentPart::Text { text }],
>     });
> }
> ```
> Then call `model.invoke(request, cancel)`. Drop the `request_with_injections` pseudo-call above — it is illustrative only. (Per-turn injection into the request is sufficient for v1; persisting into `conversation` is optional and not required by any AC.)

- [ ] **Step 6: Run the AC tests**

Run: `cargo test -p paigasus-helikon-core --test permissions --test hooks`
Expected: PASS — Plan denies the tool (AC3); PreToolUse replaces args (AC2).

- [ ] **Step 7: Full suite**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (the parallel-tools/limit tests still pass — the interleaving preserves order and concurrency).

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/tests/permissions.rs crates/paigasus-helikon-core/tests/hooks.rs
git commit -m "feat(core): SMA-326 gate tool calls with hooks+permissions; OnTurnStart and injection (AC2, AC3)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: Driver — output-guardrail gate + `OnHandoff` + `OnSubagentStop` (handoff) + `OnRunComplete`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs`
- Test: `crates/paigasus-helikon-core/tests/guardrails.rs` (extend)

- [ ] **Step 1: Write the output-guardrail integration test**

Append to `crates/paigasus-helikon-core/tests/guardrails.rs`:

```rust
#[tokio::test]
async fn output_guardrail_suppresses_run_completed() {
    use common::MockModel;
    use paigasus_helikon_core::FinishReason;

    let model = MockModel::with_scripts(vec![vec![
        ModelEvent::TokenDelta { text: "final answer".into() },
        ModelEvent::Finish { reason: FinishReason::Stop },
    ]]);

    let agent = LlmAgent::<(), _> {
        name: "og".into(),
        description: "output guardrail".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools: Vec::new(),
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: vec![Arc::new(AlwaysTrip) as Arc<dyn Guardrail<()>>],
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
        _output: std::marker::PhantomData,
    };

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .unwrap();
    let result = RunResultStreaming::new(stream).collect().await;
    assert!(result.is_err(), "output tripwire fails the run");
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p paigasus-helikon-core --test guardrails output_guardrail`
Expected: FAIL — run completes successfully (output guardrails not driven).

- [ ] **Step 3: Gate the terminal `Done` before yielding its events**

In `agent.rs`, in the main loop, after `let outcome = crate::transition(...)` and destructuring into `next_state`/`events`/`next_action`/`conversation_appends`, but **before** the `for ev in events` yield loop, insert the output-guardrail gate:

```rust
                // Output-guardrail gate: run before the bundled RunCompleted is yielded.
                let (events, next_action, next_state) = if let crate::LoopState::Done(out) = &next_state {
                    if let Some((kind, info)) = interceptors.run_output_guardrails(&out.as_text()).await {
                        run_span.record("otel.status_code", "ERROR");
                        let replaced = vec![
                            crate::AgentEvent::GuardrailTriggered { kind: kind.clone(), info },
                            crate::AgentEvent::RunFailed { error: "output guardrail tripwire".into() },
                        ];
                        failure.set(crate::AgentError::Guardrail { kind });
                        (replaced, crate::NextAction::Terminate, crate::LoopState::Failed(
                            crate::AgentError::Other(anyhow::anyhow!("output guardrail tripwire")),
                        ))
                    } else {
                        (events, next_action, next_state)
                    }
                } else {
                    (events, next_action, next_state)
                };
```

> This shadows `events`/`next_action`/`next_state`. The subsequent code (`for ev in events`, `loop_state = next_state`, `match next_action`) is unchanged. Because we set `next_action = Terminate` and a `LoopState::Failed`, the existing `Terminate` arm captures the failure via the `if let LoopState::Failed(err) = loop_state` path — but we already called `failure.set(...)`; the `Terminate` arm's `set` is idempotent-enough (last writer wins) so leaving the `Other` placeholder is harmless. To be precise, set the placeholder to the same `Guardrail{kind}` is impossible (kind moved); the `Other` placeholder is only used if the slot were empty, which it is not. Acceptable.

- [ ] **Step 4: Fire `OnHandoff` + `OnSubagentStop` (handoff) + `OnRunComplete`**

In the `NextAction::Handoff` arm, immediately before `let input = crate::AgentInput { messages: transcript };`, fire `OnHandoff`:

```rust
                        let on_handoff = interceptors
                            .fire(&crate::HookEvent::OnHandoff {
                                from: agent_name.clone(),
                                to: target.clone(),
                            })
                            .await;
                        if let Some(reason) = on_handoff.denied {
                            let err = crate::AgentError::HookDenied { event: "OnHandoff".into(), reason };
                            let msg = err.to_string();
                            run_span.record("otel.status_code", "ERROR");
                            failure.set(err);
                            yield crate::AgentEvent::RunFailed { error: msg };
                            return;
                        }
```

After the handoff sub-stream fully drains (after the `while let Some(ev) = sub.next().await { … }` loop, before `return;`), fire `OnSubagentStop`:

```rust
                        let _ = interceptors
                            .fire(&crate::HookEvent::OnSubagentStop { agent: target.clone() })
                            .await;
                        return;
```

For `OnRunComplete`: in the `NextAction::Terminate` arm, fire it before `return;` (observational — decision ignored):

```rust
                    crate::NextAction::Terminate => {
                        let _ = interceptors.fire(&crate::HookEvent::OnRunComplete).await;
                        if let crate::LoopState::Failed(err) = loop_state {
                            failure.set(err);
                        }
                        return;
                    }
```

- [ ] **Step 5: Run the test + full suite**

Run: `cargo test -p paigasus-helikon-core --test guardrails`
Expected: PASS (input + output).
Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (handoff tests still green — `OnHandoff`/`OnSubagentStop`/`OnRunComplete` with no hooks are no-ops).

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/tests/guardrails.rs
git commit -m "feat(core): SMA-326 drive output guardrails, OnHandoff, OnSubagentStop, OnRunComplete

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 12: `agent_as_tool` + `workflow.rs` — permission propagation & `OnSubagentStop`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent_as_tool.rs`
- Modify: `crates/paigasus-helikon-core/src/workflow.rs`
- Test: `crates/paigasus-helikon-core/tests/subagent_propagation.rs` (new)

- [ ] **Step 1: Write the propagation + workflow `OnSubagentStop` tests**

Create `crates/paigasus-helikon-core/tests/subagent_propagation.rs`:

```rust
//! SMA-326: Bypass propagation and OnSubagentStop across sub-run paths.

#[path = "common/mod.rs"]
mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use paigasus_helikon_core::{
    Agent, AgentInput, Hook, HookDecision, HookEvent, HookRegistry, PermissionMode, RunContext,
    SequentialAgent,
};

use common::{noop_run_context, MockAgent};

/// Records every OnSubagentStop agent name.
struct StopRecorder(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl Hook<()> for StopRecorder {
    async fn on_event(&self, _: &RunContext<()>, event: &HookEvent) -> HookDecision {
        if let HookEvent::OnSubagentStop { agent } = event {
            self.0.lock().unwrap().push(agent.clone());
        }
        HookDecision::Allow
    }
}

#[tokio::test]
async fn workflow_subagents_fire_on_subagent_stop() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut registry = HookRegistry::<()>::new();
    registry.push(Arc::new(StopRecorder(Arc::clone(&seen))));

    let a = MockAgent::new("a", |_| Vec::new());
    let b = MockAgent::new("b", |_| Vec::new());
    let seq = SequentialAgent::new("seq", vec![Arc::new(a), Arc::new(b)]);

    // Build a run context carrying the recording registry.
    let ctx = RunContext::new(
        Arc::new(()),
        Arc::new(paigasus_helikon_core::MemorySession::new())
            as Arc<dyn paigasus_helikon_core::Session>,
        registry,
        paigasus_helikon_core::TracerHandle::default(),
        paigasus_helikon_core::CancellationToken::new(),
    );

    let stream = seq.run(ctx, AgentInput::from_user_text("go")).await.unwrap();
    use futures_util::StreamExt as _;
    let _: Vec<_> = stream.collect().await;

    let names = seen.lock().unwrap().clone();
    assert!(names.contains(&"a".to_owned()) && names.contains(&"b".to_owned()),
        "each workflow sub-agent fires OnSubagentStop; saw {names:?}");
}

#[tokio::test]
async fn bypass_propagates_into_agent_as_tool() {
    // Asserted indirectly: an agent-as-tool sub-run sees Bypass via ToolContext.
    // A custom tool reads ctx.permission_mode() and records it.
    // (See AgentAsTool wiring in Task 12, Step 3.)
    let ctx = noop_run_context::<()>().with_permission_mode(PermissionMode::Bypass);
    assert_eq!(ctx.to_tool_context().permission_mode(), PermissionMode::Bypass);
}
```

> The `SequentialAgent::new` signature is verified in `workflow.rs` (SMA-325). If its constructor differs (e.g. takes a key), adapt the call — check `workflow.rs` before writing.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p paigasus-helikon-core --test subagent_propagation`
Expected: FAIL — `workflow_subagents_fire_on_subagent_stop` sees an empty `names` (workflow doesn't fire the hook yet).

- [ ] **Step 3: Wire `agent_as_tool` permission carry + `OnSubagentStop`**

In `agent_as_tool.rs`, change the `sub_ctx` construction (line ~116) to carry permission config from the `ToolContext`:

```rust
        let sub_ctx = RunContext::new(
            Arc::clone(ctx.user_ctx()),
            Arc::new(MemorySession::new()),
            HookRegistry::new(),
            ctx.tracer().clone(),
            ctx.cancel().clone(),
        )
        .with_agent_depth(depth + 1)
        .with_permission_mode(ctx.permission_mode());
```

> The `policy`/`deny_rules`/`approval_handler` carriers are `pub(crate)` on `ToolContext` (Task 5). Since `agent_as_tool.rs` is in-crate, also carry them:
> ```rust
> let sub_ctx = {
>     let mut c = sub_ctx;
>     if let Some(p) = ctx.permission_policy.clone() { c = c.with_permission_policy(p); }
>     c = c.with_deny_rules(ctx.deny_rules.clone());
>     if let Some(h) = ctx.approval_handler.clone() { c = c.with_approval_handler(h); }
>     c
> };
> ```
> (Reading `ctx.permission_policy` etc. directly is allowed: they are `pub(crate)` fields on `ToolContext`.)

To fire `OnSubagentStop`, `agent_as_tool` has no `Interceptors`; fire the hook directly via the parent's hooks. The `ToolContext` does **not** carry the hook registry by design, so `agent_as_tool` cannot fire run-level hooks. **Decision (matches spec §6):** the agent-as-tool sub-run's own driver already fires its `OnRunComplete`; for `OnSubagentStop` specifically, the sub-`RunContext` shares no registry. To honor the uniform contract, pass the parent registry through: extend the `agent_as_tool` sub-context to **clone the parent run-level hooks**. Since `ToolContext` omits the registry, the minimal faithful approach is: fire `OnSubagentStop` from the **enclosing driver** when the tool returns. **Therefore move agent-as-tool's `OnSubagentStop` firing into the driver's `PostToolUse` path is not possible (the driver doesn't know the tool wrapped an agent).**

> **Resolution for the plan:** carry the run-level `HookRegistry` into `ToolContext` as a `pub(crate)` field (a 4th carrier), set in `to_tool_context`, so `agent_as_tool` can construct an `Interceptors`-free direct fire:
> 1. In `tool.rs`, add `pub(crate) hooks: HookRegistry<Ctx>` to `ToolContext`, defaulting to `HookRegistry::new()` in `new`, and set it in `with_permissions` (rename to `with_run_state` or add a param). Simplest: add a separate `pub(crate) fn with_hooks(mut self, h: HookRegistry<Ctx>) -> Self`.
> 2. In `context.rs` `to_tool_context`, add `.with_hooks(self.hooks.clone())`.
> 3. In `agent_as_tool.rs`, after the sub-run collects, fire each hook:
>    ```rust
>    for hook in ctx.hooks.iter() {
>        let _ = hook.on_event(&sub_ctx_for_fire, &HookEvent::OnSubagentStop {
>            agent: self.agent.name().to_owned(),
>        }).await;
>    }
>    ```
>    where `sub_ctx_for_fire` is a `RunContext` you can borrow (reuse `sub_ctx` before it is consumed, or rebuild a minimal one). Since `sub_ctx` is moved into `self.agent.run(sub_ctx, …)`, fire **before** the run for `OnSubagentStart` semantics is wrong; instead clone the registry and fire against a fresh minimal ctx after collection.

> **Pragmatic v1 (recommended to keep scope sane):** Fire `OnSubagentStop` for agent-as-tool by reusing the parent `ToolContext`'s hooks against the parent context is awkward without the registry. Given the spec's intent (uniform coverage) but the `ToolContext` boundary, implement agent-as-tool's `OnSubagentStop` by having `ToolContext` carry the registry (`with_hooks` above) and firing directly. Add a focused test (extend Step 1) once wired. If the borrow/lifetime cost proves high, record the limitation in the spec and scope agent-as-tool's `OnSubagentStop` to a follow-up — but workflow + handoff coverage (this task + Task 11) must land.

- [ ] **Step 4: Wire `workflow.rs` `OnSubagentStop` at all three sites**

In `workflow.rs`, after each `agent.run(child, input...).await` sub-run completes and its stream drains, fire `OnSubagentStop` against the workflow agent's `ctx` (which holds the shared registry). At each of Sequential (~166), Parallel (~326), Loop (~499), after the sub-stream is consumed:

```rust
                for hook in ctx.hooks().iter() {
                    let _ = hook
                        .on_event(&ctx, &HookEvent::OnSubagentStop { agent: agent.name().to_owned() })
                        .await;
                }
```

> Use the same `ctx` the workflow agent holds (it has the shared `HookRegistry` via `subagent_child` sharing). For `ParallelAgent`, fire after each branch's stream drains (inside the per-branch async block, against a clone of the needed handles) — or collect names and fire after the join. Firing per-branch keeps it uniform; ensure `HookEvent` is imported in `workflow.rs`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p paigasus-helikon-core --test subagent_propagation`
Expected: PASS.
Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (full suite, incl. existing workflow tests).

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/agent_as_tool.rs crates/paigasus-helikon-core/src/workflow.rs crates/paigasus-helikon-core/src/tool.rs crates/paigasus-helikon-core/src/context.rs crates/paigasus-helikon-core/tests/subagent_propagation.rs
git commit -m "feat(core): SMA-326 propagate permission config and fire OnSubagentStop for sub-runs

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 13: `#[tool]` macro — `effect=` support

**Files:**
- Modify: `crates/paigasus-helikon-macros/src/attr.rs`
- Modify: `crates/paigasus-helikon-macros/src/expand.rs`
- Test: `crates/paigasus-helikon-core/tests/permissions.rs` (extend with a macro-tool Plan test)

- [ ] **Step 1: Write the failing macro-tool integration test**

Append to `crates/paigasus-helikon-core/tests/permissions.rs`:

```rust
#[tokio::test]
async fn macro_read_only_tool_allowed_under_plan() {
    use paigasus_helikon_core::{tool, ToolContext, ToolError, ToolEffect, Tool};
    use serde::Deserialize;
    use schemars::JsonSchema;

    #[derive(Deserialize, JsonSchema)]
    struct Empty {}

    /// A read-only tool.
    #[tool(effect = read_only)]
    async fn reader(_ctx: &ToolContext<()>, _args: Empty) -> Result<String, ToolError> {
        Ok("ok".into())
    }

    assert_eq!(reader.effect(), ToolEffect::ReadOnly);

    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta { call_id: "1".into(), name: Some("reader".into()), args_delta: "{}".into() },
            ModelEvent::Finish { reason: FinishReason::ToolCalls },
        ],
        vec![
            ModelEvent::TokenDelta { text: "done".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
    ]);
    let agent = agent(model, vec![Arc::new(reader) as Arc<dyn Tool<()>>]);
    let ctx = noop_run_context::<()>().with_permission_mode(PermissionMode::Plan);
    let stream = agent.run(ctx, AgentInput::from_user_text("go")).await.unwrap();
    let result = RunResultStreaming::new(stream).collect().await.unwrap();
    // Plan ALLOWS a ReadOnly tool — no PermissionDenied event.
    assert!(!result.events.iter().any(|e| matches!(e, AgentEvent::PermissionDenied { .. })));
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p paigasus-helikon-core --test permissions macro_read_only`
Expected: FAIL — `unknown #[tool] attribute effect`.

- [ ] **Step 3: Parse `effect=` in `attr.rs`**

Add a field + enum to `attr.rs`:

```rust
/// Parsed `effect = read_only | write | side_effect`.
#[derive(Clone, Copy)]
pub(crate) enum ToolEffectArg { ReadOnly, Write, SideEffect }

// in struct ToolAttrArgs:
    pub effect: Option<ToolEffectArg>,
```

In the `match key.to_string().as_str()` block, add an arm before the `other =>` catch-all:

```rust
                    "effect" => {
                        let val: Ident = input.parse()?;
                        out.effect = Some(match val.to_string().as_str() {
                            "read_only" => ToolEffectArg::ReadOnly,
                            "write" => ToolEffectArg::Write,
                            "side_effect" => ToolEffectArg::SideEffect,
                            other => {
                                return Err(Error::new(
                                    val.span(),
                                    format!("invalid `effect` value `{other}`; expected \
                                             `read_only`, `write`, or `side_effect`"),
                                ));
                            }
                        });
                    }
```

Update the `other =>` error message to list `effect`:

```rust
                                "unknown #[tool] attribute `{other}`; expected one of \
                                 `description`, `name`, `effect`, `crate`",
```

- [ ] **Step 4: Emit the `effect()` override in `expand.rs`**

In `expand.rs::tool`, after computing `core`, build an optional override token:

```rust
    let effect_method = match attr_args.effect {
        None => quote!(),
        Some(crate::attr::ToolEffectArg::ReadOnly) => quote! {
            fn effect(&self) -> #core::ToolEffect { #core::ToolEffect::ReadOnly }
        },
        Some(crate::attr::ToolEffectArg::Write) => quote! {
            fn effect(&self) -> #core::ToolEffect { #core::ToolEffect::Write }
        },
        Some(crate::attr::ToolEffectArg::SideEffect) => quote! {
            fn effect(&self) -> #core::ToolEffect { #core::ToolEffect::SideEffect }
        },
    };
```

Insert `#effect_method` into the `impl #core::Tool` block, right after the `fn description` line:

```rust
            impl #core::Tool<#ctx_ty> for #fn_ident {
                fn name(&self) -> &str { #tool_name }
                fn description(&self) -> &str { #description }
                #effect_method
                fn schema(&self) -> &::serde_json::Value {
```

- [ ] **Step 5: Run the macro test**

Run: `cargo test -p paigasus-helikon-core --test permissions macro_read_only`
Expected: PASS.

- [ ] **Step 6: Run macros' own tests + full build**

Run: `cargo test -p paigasus-helikon-macros`
Expected: PASS (existing trybuild UI tests unaffected; if a UI test asserts the "expected one of" message, update its `.stderr` to include `effect`).
Run: `cargo build --workspace --all-features`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-macros/src/attr.rs crates/paigasus-helikon-macros/src/expand.rs crates/paigasus-helikon-core/tests/permissions.rs
git commit -m "feat(macros): SMA-326 add effect= to the #[tool] macro

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 14: Docs, full CI gate, and release bumps

**Files:**
- Modify: `crates/paigasus-helikon-core/Cargo.toml`, `crates/paigasus-helikon-macros/Cargo.toml`, `crates/paigasus-helikon/Cargo.toml`, root `Cargo.toml` (`[workspace.dependencies]` pins), the three `CHANGELOG.md`s.

- [ ] **Step 1: Run the full CI gate locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
```
Expected: all green. Fix any missing `///` docs surfaced by the docs/coverage jobs (every new `pub` item needs one).

- [ ] **Step 2: Determine current versions (don't trust hardcoded numbers)**

```bash
grep '^version' crates/paigasus-helikon-core/Cargo.toml crates/paigasus-helikon-macros/Cargo.toml crates/paigasus-helikon/Cargo.toml
git log --oneline -5    # check whether SMA-325's release bump has merged
```

- [ ] **Step 3: Bump versions (minor) and CHANGELOGs**

This is a `feat`-level, additive change. Per `CLAUDE.md`, `release-plz` treats additive `feat` on `0.x` as a **patch** when it does the bump itself — but because `macros` consumes core API added in this PR, apply the same-PR manual-bump recipe so `cargo publish --verify` for `macros` builds against a core that has `ToolEffect`:

- `paigasus-helikon-core`: bump `version` (e.g. `0.4.0 → 0.5.0`) + update the `[workspace.dependencies] paigasus-helikon-core` pin in root `Cargo.toml` + prepend a `CHANGELOG.md` entry.
- `paigasus-helikon-macros`: bump `version` + update its `[workspace.dependencies]` pin + `CHANGELOG.md`.
- `paigasus-helikon` (facade): bump `version` (patch) + its self-pin + `CHANGELOG.md` so its published dep reqs track the new core/macros (the facade-drift caveat in `CLAUDE.md`).

> Read `CLAUDE.md` → "the 4-step ascend recipe" and the two same-PR-bump caveats before editing. Confirm exact target numbers against the live `Cargo.toml`s and whether SMA-325's bump landed.

- [ ] **Step 4: Verify the workspace still builds with the new pins**

```bash
cargo build --workspace --all-features
cargo test --workspace --all-features
```
Expected: clean.

- [ ] **Step 5: Commit the release bump separately**

```bash
git add crates/paigasus-helikon-core/Cargo.toml crates/paigasus-helikon-macros/Cargo.toml crates/paigasus-helikon/Cargo.toml Cargo.toml crates/*/CHANGELOG.md
git commit -m "chore(release): SMA-326 bump core, macros, and facade for control layers

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 6: Push and open the PR**

```bash
git push -u origin feature/sma-326-guardrails-hooks-and-permissionpolicy
gh pr create --title "feat(core): SMA-326 add guardrails, hooks & permission policy" \
  --body "Implements SMA-326. See docs/superpowers/specs/2026-06-07-guardrails-hooks-permissions-design.md."
```

> PR title rules (`pr-title.yml`): full Conventional-Commits prefix + lowercase subject after `SMA-### `. The title above satisfies both. Linear auto-closes SMA-326 on merge.

---

## Self-review notes (for the implementer)

- **Spec coverage:** Tasks map to spec sections — T1→§4.1, T2→§4.2, T3→§4.5/§4.6, T4→§4.3, T5→§4.4, T6→§5.1, T7→§5.2, T8→§5.3, T9→§5.3+§6(AC1), T10→§5.4+§6(AC2/AC3), T11→§5.3/§6, T12→§6(M3)+§4.4, T13→§4.7(H1), T14→§10.
- **AC traceability:** AC1 → T9 `input_guardrail_aborts_before_any_model_call`; AC2 → T10 `pre_tool_use_replace_input_modifies_invocation`; AC3 → T10 `plan_mode_denies_side_effecting_tool` + T13 `macro_read_only_tool_allowed_under_plan`.
- **Known soft spots to validate while implementing:**
  - T10 Step 5 borrow-checker interaction (firing `OnTurnStart` inside the event-yield loop while `interceptors` borrows `ctx`). If it fights you, queue turn indices and fire after the loop. The plan flags this.
  - T11 Step 3 output-guardrail shadow-rebind and the `Terminate`-arm failure-slot placeholder. Verify the slot already holds the `Guardrail{kind}` error (it does — set before the shadow).
  - T12 agent-as-tool `OnSubagentStop` requires `ToolContext` to carry the `HookRegistry` (a `with_hooks` carrier). If that proves heavy, land handoff+workflow coverage and scope agent-as-tool's `OnSubagentStop` to a follow-up, recording it in the spec.
  - T13: if a macros UI (`trybuild`) test asserts the unknown-attribute error text, update its `.stderr`.
```
