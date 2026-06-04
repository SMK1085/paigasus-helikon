# SMA-325 Workflow Agents Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add three deterministic orchestrators — `SequentialAgent`, `ParallelAgent`, `LoopAgent` — that implement the existing `Agent<Ctx>` trait, coordinating sub-agents through a new run-scoped `SessionState` KV and a tool-driven `escalate` side-channel.

**Architecture:** A new run-scoped `SessionState` (in-memory KV) and `ActionsHandle` (escalate signal) ride on `RunContext`, mirroring the existing `FailureSlot` pattern, and project into `ToolContext`. Each workflow agent's `run` builds an `async_stream` that derives a child context per sub-agent (`subagent_child()`), drives the child's event stream with the established handoff-driver merge convention (swallow inner `RunStarted`, fold `RunCompleted.usage`, pass through the rest), auto-writes each child's final text to `state[key]`, and emits one outer `RunStarted`/`RunCompleted`. Concurrency in `ParallelAgent` uses `futures::stream::select_all` (core has no tokio runtime).

**Tech Stack:** Rust, `async-trait`, `async-stream`, `futures-util`, `serde_json`, `tokio` (dev-only, tests).

**Spec:** `docs/superpowers/specs/2026-06-04-sma-325-workflow-agents-design.md`

---

## File Structure

**Create:**
- `crates/paigasus-helikon-core/src/state.rs` — `SessionState`, `EventActions`, `ActionsHandle`.
- `crates/paigasus-helikon-core/src/workflow.rs` — `SequentialAgent`, `ParallelAgent`, `LoopAgent` + private helpers.
- `crates/paigasus-helikon-core/tests/workflow_sequential.rs`
- `crates/paigasus-helikon-core/tests/workflow_parallel.rs`
- `crates/paigasus-helikon-core/tests/workflow_loop.rs`
- `crates/paigasus-helikon-core/tests/workflow_pipeline.rs`

**Modify:**
- `crates/paigasus-helikon-core/src/lib.rs` — declare + re-export `state` and `workflow`.
- `crates/paigasus-helikon-core/src/agent.rs` — add `AgentError::MaxIterationsExceeded`.
- `crates/paigasus-helikon-core/src/tool.rs` — `ToolContext` gains `state`/`actions` fields, accessors, `with_state`/`with_actions`.
- `crates/paigasus-helikon-core/src/context.rs` — `RunContext` gains `state`/`actions`, accessors, `subagent_child()`; update `to_tool_context()` + `handoff_child()`.
- `crates/paigasus-helikon-core/tests/common/mod.rs` — add `MockAgent`, `EscalatingTool`, event helpers.

**Release:** `core` is already published; do **not** manually bump the version or edit CHANGELOG/workspace pins — release-plz does it after merge (non-breaking `feat` → patch on 0.x; cascades the facade). Commit types: `feat(core): SMA-325 …` (impl) / `test(core): SMA-325 …` (test-only). Commits are signed via 1Password — if a commit fails with "failed to fill whole buffer", the vault is locked; ask the user to unlock, then retry.

---

## Task 1: `SessionState` — run-scoped KV

**Files:**
- Create: `crates/paigasus-helikon-core/src/state.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs`

- [ ] **Step 1: Create `state.rs` with the failing test only**

Create `crates/paigasus-helikon-core/src/state.rs` with this content:

```rust
//! Run-scoped, in-memory coordination state for workflow agents (SMA-325).
//!
//! [`SessionState`] is a key→JSON scratchpad shared across the sub-agents of a
//! single run; [`ActionsHandle`] is a control side-channel a tool uses to signal
//! the enclosing driver (today: `escalate`). Both mirror the [`crate::FailureSlot`]
//! pattern: an `Arc<Mutex<…>>` carried on [`crate::RunContext`], projected into
//! [`crate::ToolContext`], written inside, read after the stream drains. Neither
//! is persisted to the `Session` log.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A run-scoped, in-memory key→JSON store shared across a run's sub-agents.
///
/// Cloning shares the underlying store (it is an `Arc` handle). `ParallelAgent`
/// branches write **disjoint** keys, so the brief per-write lock never contends
/// meaningfully. **Not** persisted to the `Session` event log.
#[derive(Clone, Default)]
pub struct SessionState(Arc<Mutex<HashMap<String, serde_json::Value>>>);

impl SessionState {
    /// Construct an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read a value by key, cloned out of the store.
    pub fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }

    /// Insert or overwrite a value.
    pub fn set(&self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.into(), value.into());
    }

    /// `true` if the key is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(key)
    }

    /// Every key currently in the store, in arbitrary order.
    pub fn keys(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::SessionState;
    use serde_json::json;

    #[test]
    fn set_get_roundtrip() {
        let s = SessionState::new();
        assert!(s.get("k").is_none());
        s.set("k", "v");
        assert_eq!(s.get("k"), Some(json!("v")));
        assert!(s.contains_key("k"));
    }

    #[test]
    fn clone_shares_store() {
        let a = SessionState::new();
        let b = a.clone();
        b.set("x", 1);
        assert_eq!(a.get("x"), Some(json!(1)));
    }

    #[test]
    fn keys_lists_all() {
        let s = SessionState::new();
        s.set("a", 1);
        s.set("b", 2);
        let mut k = s.keys();
        k.sort();
        assert_eq!(k, vec!["a".to_owned(), "b".to_owned()]);
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

In `crates/paigasus-helikon-core/src/lib.rs`, add `pub mod state;` to the module list (alphabetical, after `pub mod session;`) and `pub use state::*;` to the re-export list (after `pub use session::*;`):

```rust
pub mod state;
```
```rust
pub use state::*;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p paigasus-helikon-core --lib state::tests`
Expected: PASS (3 tests).

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/state.rs crates/paigasus-helikon-core/src/lib.rs
git commit -m "feat(core): SMA-325 add run-scoped SessionState"
```

---

## Task 2: `EventActions` / `ActionsHandle` — escalate side-channel

**Files:**
- Modify: `crates/paigasus-helikon-core/src/state.rs`

- [ ] **Step 1: Append the failing test**

Add to the `#[cfg(test)] mod tests` block in `state.rs`:

```rust
    use super::ActionsHandle;

    #[test]
    fn escalate_sets_flag() {
        let a = ActionsHandle::new();
        assert!(!a.is_escalated());
        a.escalate();
        assert!(a.is_escalated());
    }

    #[test]
    fn actions_clone_shares_slot() {
        let a = ActionsHandle::new();
        let b = a.clone();
        b.escalate();
        assert!(a.is_escalated(), "a clone observes the escalate");
    }

    #[test]
    fn snapshot_reflects_escalate() {
        let a = ActionsHandle::new();
        a.escalate();
        assert!(a.snapshot().escalate);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-core --lib state::tests`
Expected: FAIL — `cannot find type ActionsHandle`.

- [ ] **Step 3: Implement `EventActions` + `ActionsHandle`**

Add to `state.rs` (after the `SessionState` impl, before the test module):

```rust
/// Control signals a tool can raise to the enclosing driver.
///
/// The faithful port of ADK's `EventActions`. Today it carries one signal,
/// `escalate`; `#[non_exhaustive]` so it can grow (`skip_summarization`,
/// `transfer_to_agent`, …) without a breaking change.
#[non_exhaustive]
#[derive(Debug, Default, Clone)]
pub struct EventActions {
    /// Request that the enclosing `LoopAgent` stop iterating.
    pub escalate: bool,
}

/// Cloneable handle a tool uses to raise [`EventActions`] signals.
///
/// `LoopAgent` reads [`ActionsHandle::is_escalated`] after a sub-agent run
/// drains — the same write-inside / read-after-drain discipline as
/// [`crate::FailureSlot`].
#[derive(Clone, Default)]
pub struct ActionsHandle(Arc<Mutex<EventActions>>);

impl ActionsHandle {
    /// Construct a handle with no signals raised.
    pub fn new() -> Self {
        Self::default()
    }

    /// Raise the `escalate` signal.
    pub fn escalate(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).escalate = true;
    }

    /// `true` once any holder of this handle has called [`ActionsHandle::escalate`].
    pub fn is_escalated(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).escalate
    }

    /// Clone the current [`EventActions`] out for inspection.
    pub fn snapshot(&self) -> EventActions {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p paigasus-helikon-core --lib state::tests`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/state.rs
git commit -m "feat(core): SMA-325 add EventActions escalate side-channel"
```

---

## Task 3: `AgentError::MaxIterationsExceeded`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs`

- [ ] **Step 1: Add the failing test**

In `crates/paigasus-helikon-core/src/agent.rs`, inside the existing `#[cfg(test)] mod failure_slot_tests` block, add:

```rust
    #[test]
    fn max_iterations_exceeded_displays() {
        assert_eq!(
            AgentError::MaxIterationsExceeded { max: 3 }.to_string(),
            "max iterations (3) exceeded"
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-core --lib failure_slot_tests::max_iterations_exceeded_displays`
Expected: FAIL — `no variant named MaxIterationsExceeded`.

- [ ] **Step 3: Add the variant**

In `agent.rs`, in the `pub enum AgentError` block, add this variant immediately after `MaxTurnsExceeded`:

```rust
    /// New in SMA-325: a [`crate::LoopAgent`] ran `max_iterations` without a
    /// sub-agent escalating.
    #[error("max iterations ({max}) exceeded")]
    MaxIterationsExceeded {
        /// The configured iteration budget.
        max: u32,
    },
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p paigasus-helikon-core --lib failure_slot_tests::max_iterations_exceeded_displays`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs
git commit -m "feat(core): SMA-325 add AgentError::MaxIterationsExceeded"
```

---

## Task 4: `ToolContext` gains `state` + `actions`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/tool.rs`

- [ ] **Step 1: Add the failing test**

Add to `tool.rs` a new test module at the end of the file:

```rust
#[cfg(test)]
mod tool_context_tests {
    use super::ToolContext;
    use crate::{ActionsHandle, CancellationToken, SessionState, TracerHandle};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn state_and_actions_default_empty() {
        let tc: ToolContext<()> = ToolContext::new(
            Arc::new(()),
            TracerHandle::default(),
            CancellationToken::new(),
            0,
            8,
        );
        assert!(tc.state().get("x").is_none());
        assert!(!tc.actions().is_escalated());
    }

    #[test]
    fn with_state_and_with_actions_project_handles() {
        let state = SessionState::new();
        state.set("k", "v");
        let actions = ActionsHandle::new();
        let tc: ToolContext<()> = ToolContext::new(
            Arc::new(()),
            TracerHandle::default(),
            CancellationToken::new(),
            0,
            8,
        )
        .with_state(state.clone())
        .with_actions(actions.clone());

        assert_eq!(tc.state().get("k"), Some(json!("v")));
        tc.actions().escalate();
        assert!(actions.is_escalated(), "escalate flows to the shared handle");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-core --lib tool_context_tests`
Expected: FAIL — no `state`/`actions`/`with_state`/`with_actions`.

- [ ] **Step 3: Add fields, accessors, and builders**

In `tool.rs`:

(a) Extend the `use` at the top:
```rust
use crate::{ActionsHandle, CancellationToken, SessionState, TracerHandle};
```

(b) Add two fields to `struct ToolContext<Ctx>` (after `max_agent_depth`):
```rust
    state: SessionState,
    actions: ActionsHandle,
```

(c) In `ToolContext::new`, initialize the new fields to empty (keep the 5-arg signature):
```rust
        Self {
            user_ctx,
            tracer,
            cancel,
            agent_depth,
            max_agent_depth,
            state: SessionState::new(),
            actions: ActionsHandle::new(),
        }
```

(d) Add accessors and consuming builders inside `impl<Ctx> ToolContext<Ctx>` (after `max_agent_depth`):
```rust
    /// Borrow the run-scoped [`SessionState`] shared across sub-agents.
    pub fn state(&self) -> &SessionState {
        &self.state
    }
    /// Borrow the [`ActionsHandle`]. A tool calls `ctx.actions().escalate()`
    /// to stop an enclosing [`crate::LoopAgent`].
    pub fn actions(&self) -> &ActionsHandle {
        &self.actions
    }
    /// Install the shared [`SessionState`] (used by
    /// [`crate::RunContext::to_tool_context`]).
    pub fn with_state(mut self, state: SessionState) -> Self {
        self.state = state;
        self
    }
    /// Install the [`ActionsHandle`] (used by
    /// [`crate::RunContext::to_tool_context`]).
    pub fn with_actions(mut self, actions: ActionsHandle) -> Self {
        self.actions = actions;
        self
    }
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p paigasus-helikon-core --lib tool_context_tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/tool.rs
git commit -m "feat(core): SMA-325 project state and actions into ToolContext"
```

---

## Task 5: `RunContext` gains `state`, `actions`, `subagent_child`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs`

- [ ] **Step 1: Add the failing tests**

Add to the existing `#[cfg(test)] mod runcontext_tests` block in `context.rs`:

```rust
    #[test]
    fn to_tool_context_projects_state_and_actions() {
        use serde_json::json;
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        ctx.state().set("k", "v");
        let tc = ctx.to_tool_context();
        assert_eq!(tc.state().get("k"), Some(json!("v")));
        tc.actions().escalate();
        assert!(ctx.actions().is_escalated(), "tool escalate reaches the run");
    }

    #[test]
    fn subagent_child_shares_state_fresh_actions_increments_depth() {
        use serde_json::json;
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        ctx.state().set("k", "v");
        ctx.actions().escalate();

        let child = ctx.subagent_child();
        assert_eq!(child.agent_depth(), 1);
        assert_eq!(child.state().get("k"), Some(json!("v")), "state is shared");
        assert!(!child.actions().is_escalated(), "actions slot is fresh");

        child.state().set("k2", "v2");
        assert_eq!(ctx.state().get("k2"), Some(json!("v2")), "shared store");

        // Fresh failure slot: a child failure does not preemptively fill the parent's.
        use crate::AgentError;
        child.failure_handle().set(AgentError::MaxTurnsExceeded(1));
        assert!(ctx.failure_handle().take().is_none(), "fresh failure slot");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-core --lib runcontext_tests`
Expected: FAIL — no `state`/`actions`/`subagent_child`.

- [ ] **Step 3: Wire `RunContext`**

In `context.rs`:

(a) Extend the top `use`:
```rust
use crate::{ActionsHandle, FailureSlot, Hook, RunConfig, Session, SessionState, ToolContext};
```

(b) Add two fields to `struct RunContext<Ctx>` (after `agent_depth`):
```rust
    /// Run-scoped coordination KV shared across sub-agents (SMA-325). Shared by
    /// `subagent_child` / `handoff_child`; **not** projected as isolated.
    state: SessionState,
    /// Control side-channel a tool writes (e.g. `escalate`). A **fresh** handle
    /// per `subagent_child`, so a `LoopAgent` reads only the current sub-run's signal.
    actions: ActionsHandle,
```

(c) In `RunContext::new`, initialize them (after `agent_depth: 0,`):
```rust
            state: SessionState::new(),
            actions: ActionsHandle::new(),
```

(d) Add accessors inside `impl<Ctx> RunContext<Ctx>` (after the `cancel` accessor):
```rust
    /// Borrow the run-scoped [`SessionState`] shared across sub-agents.
    pub fn state(&self) -> &SessionState {
        &self.state
    }
    /// Borrow the [`ActionsHandle`] for this (sub-)run.
    pub fn actions(&self) -> &ActionsHandle {
        &self.actions
    }
```

(e) Add `state`/`actions` to the `handoff_child` struct literal (a handoff continues the same logical run, so both are shared):
```rust
            state: self.state.clone(),
            actions: self.actions.clone(),
```

(f) Add the `subagent_child` method inside `impl<Ctx> RunContext<Ctx>` (after `handoff_child`):
```rust
    /// A context for one sub-agent of a workflow agent (`SequentialAgent`,
    /// `ParallelAgent`, `LoopAgent`). **Shares** the run-scoped `state`, session,
    /// cancel token, tracer, user context, and run config; gets a **fresh**
    /// `FailureSlot` and a **fresh** `ActionsHandle` (so the workflow agent reads
    /// only this sub-run's failure / escalate); `agent_depth` incremented by one.
    pub fn subagent_child(&self) -> Self {
        Self {
            user_ctx: Arc::clone(&self.user_ctx),
            session: Arc::clone(&self.session),
            hooks: self.hooks.clone(),
            tracer: self.tracer.clone(),
            cancel: self.cancel.clone(),
            run_config: self.run_config.clone(),
            failure: FailureSlot::new(),
            agent_depth: self.agent_depth.saturating_add(1),
            state: self.state.clone(),
            actions: ActionsHandle::new(),
        }
    }
```

(g) In `to_tool_context`, project the two handles onto the returned `ToolContext` (chain onto the existing `ToolContext::new(...)` expression):
```rust
        ToolContext::new(
            Arc::clone(&self.user_ctx),
            self.tracer.clone(),
            self.cancel.child_token(),
            self.agent_depth,
            max_agent_depth,
        )
        .with_state(self.state.clone())
        .with_actions(self.actions.clone())
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p paigasus-helikon-core --lib runcontext_tests`
Expected: PASS.

- [ ] **Step 5: Run the full lib + existing integration suites (no regressions)**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (existing `agent_as_tool`, `handoff`, etc. unaffected — `RunContext::new` / `ToolContext::new` arities unchanged).

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/context.rs
git commit -m "feat(core): SMA-325 thread state and actions through RunContext"
```

---

## Task 6: `MockAgent` + `EscalatingTool` test helpers

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/common/mod.rs`

- [ ] **Step 1: Extend the `common` imports**

In `crates/paigasus-helikon-core/tests/common/mod.rs`, extend the `use paigasus_helikon_core::{…}` import to add these names (keep existing ones):
```rust
use paigasus_helikon_core::{
    Agent, AgentError, AgentEvent, AgentInput, CancellationToken, ContentPart, ConversationSnapshot,
    HookRegistry, Item, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest, RunContext,
    SequenceId, Session, SessionError, SessionEvent, TokenUsage, Tool, ToolContext, ToolError,
    ToolOutput, TracerHandle,
};
```

- [ ] **Step 2: Append the helpers**

Add at the end of `common/mod.rs`:

```rust
/// Build a `TokenUsage` with `input_tokens == total_tokens == total` (the other
/// fields zero). Constructed via `default()` + field assignment because
/// `TokenUsage` is `#[non_exhaustive]` (no struct-literal construction off-crate).
pub fn usage_total(total: u64) -> TokenUsage {
    let mut u = TokenUsage::default();
    u.input_tokens = total;
    u.total_tokens = total;
    u
}

/// An `AgentEvent::MessageOutput` carrying an assistant text message.
pub fn assistant_msg(agent: &str, text: &str) -> AgentEvent {
    AgentEvent::MessageOutput {
        item: Item::AssistantMessage {
            content: vec![ContentPart::Text { text: text.to_owned() }],
            agent: Some(agent.to_owned()),
        },
    }
}

/// The canonical "ran and finished" event sequence: `RunStarted`, one
/// `MessageOutput` with `text`, then `RunCompleted` carrying `usage_total(total)`.
pub fn msg_and_complete(agent: &str, text: &str, total: u64) -> Vec<AgentEvent> {
    vec![
        AgentEvent::RunStarted { agent: agent.to_owned() },
        assistant_msg(agent, text),
        AgentEvent::RunCompleted { usage: usage_total(total) },
    ]
}

/// A scripted [`Agent`]: its `run` evaluates `behavior(&ctx)` once (which may read
/// `ctx.state()`, call `ctx.actions().escalate()`, or set `ctx.failure_handle()`),
/// then streams the returned events. No model required.
pub struct MockAgent<Ctx> {
    name: String,
    description: String,
    behavior: Arc<dyn Fn(&RunContext<Ctx>) -> Vec<AgentEvent> + Send + Sync>,
}

impl<Ctx> MockAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    pub fn new(
        name: &str,
        behavior: impl Fn(&RunContext<Ctx>) -> Vec<AgentEvent> + Send + Sync + 'static,
    ) -> MockAgent<Ctx> {
        MockAgent {
            name: name.to_owned(),
            description: format!("mock agent {name}"),
            behavior: Arc::new(behavior),
        }
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for MockAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        _input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let events = (self.behavior)(&ctx);
        Ok(Box::pin(stream::iter(events)))
    }
}

/// A [`Tool`] that calls `ctx.actions().escalate()` and returns `{"escalated": true}`.
pub struct EscalatingTool {
    name: String,
    schema: serde_json::Value,
}

impl EscalatingTool {
    pub fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            schema: serde_json::json!({"type": "object"}),
        })
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for EscalatingTool
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "Signals the enclosing loop to stop."
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    async fn invoke(
        &self,
        ctx: &ToolContext<Ctx>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        ctx.actions().escalate();
        Ok(ToolOutput::new(serde_json::json!({"escalated": true})))
    }
}
```

- [ ] **Step 3: Verify the helpers compile**

This module compiles only when referenced by a test binary; Task 7 is the first consumer. For now confirm the crate still builds:
Run: `cargo test -p paigasus-helikon-core --lib`
Expected: PASS (no integration binary references `common` yet, so unused-helper warnings are suppressed by the existing `#![allow(dead_code)]` at the top of `common/mod.rs`).

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/tests/common/mod.rs
git commit -m "test(core): SMA-325 add MockAgent and EscalatingTool helpers"
```

---

## Task 7: `SequentialAgent`

**Files:**
- Create: `crates/paigasus-helikon-core/src/workflow.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Test: `crates/paigasus-helikon-core/tests/workflow_sequential.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/paigasus-helikon-core/tests/workflow_sequential.rs`:

```rust
//! SMA-325 — SequentialAgent: order, state threading, usage, fail-fast.

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use common::{msg_and_complete, MockAgent};
use paigasus_helikon_core::{
    AgentError, AgentEvent, AgentInput, CancellationToken, HookRegistry, MemorySession, RunContext,
    RunError, RunResultStreaming, SequentialAgent, Session, TracerHandle,
};

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
async fn threads_output_via_state() {
    let producer = MockAgent::new("producer", |_| msg_and_complete("producer", "hello", 0));
    let consumer = MockAgent::new("consumer", |ctx| {
        let upstream = ctx
            .state()
            .get("producer")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "MISSING".to_owned());
        msg_and_complete("consumer", &format!("got: {upstream}"), 0)
    });
    let seq = SequentialAgent::new("seq", "produce then consume")
        .then(producer)
        .then(consumer);

    let result = RunResultStreaming::new(seq.run(ctx(), AgentInput::from_user_text("go")).await.unwrap())
        .collect()
        .await
        .unwrap();

    assert_eq!(result.final_output, "got: hello", "A->B threading via state");
}

#[tokio::test]
async fn order_and_usage_and_single_outer_lifecycle() {
    let a = MockAgent::new("a", |_| msg_and_complete("a", "A", 10));
    let b = MockAgent::new("b", |_| msg_and_complete("b", "B", 5));
    let seq = SequentialAgent::new("seq", "").then(a).then(b);

    let result = RunResultStreaming::new(seq.run(ctx(), AgentInput::from_user_text("go")).await.unwrap())
        .collect()
        .await
        .unwrap();

    let updates: Vec<String> = result
        .events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AgentUpdated { agent } => Some(agent.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(updates, vec!["a".to_owned(), "b".to_owned()], "in order");

    let starts = result.events.iter().filter(|e| matches!(e, AgentEvent::RunStarted { .. })).count();
    let completes = result.events.iter().filter(|e| matches!(e, AgentEvent::RunCompleted { .. })).count();
    assert_eq!(starts, 1, "only the outer RunStarted surfaces");
    assert_eq!(completes, 1, "only the outer RunCompleted surfaces");
    assert_eq!(result.usage.total_tokens, 15, "summed across steps");
    assert_eq!(result.final_output, "B", "last step's output");
}

#[tokio::test]
async fn fail_fast_stops_later_steps_and_surfaces_structured_error() {
    let ran_second = Arc::new(AtomicBool::new(false));
    let flag = ran_second.clone();
    let boom = MockAgent::new("boom", |ctx| {
        ctx.failure_handle().set(AgentError::Other(anyhow::anyhow!("kaboom")));
        vec![
            AgentEvent::RunStarted { agent: "boom".to_owned() },
            AgentEvent::RunFailed { error: "kaboom".to_owned() },
        ]
    });
    let never = MockAgent::new("never", move |_| {
        flag.store(true, Ordering::SeqCst);
        msg_and_complete("never", "x", 0)
    });
    let seq = SequentialAgent::new("seq", "").then(boom).then(never);

    let ctx = ctx();
    let failure = ctx.failure_handle();
    let err = RunResultStreaming::with_failure(seq.run(ctx, AgentInput::from_user_text("go")).await.unwrap(), failure)
        .collect()
        .await
        .expect_err("first step fails");

    assert!(matches!(err, RunError::Agent(AgentError::Other(_))), "structured error: {err:?}");
    assert!(!ran_second.load(Ordering::SeqCst), "fail-fast: second step must not run");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-core --test workflow_sequential`
Expected: FAIL to compile — `SequentialAgent` not found.

- [ ] **Step 3: Create `workflow.rs` with shared helpers + `SequentialAgent`**

Create `crates/paigasus-helikon-core/src/workflow.rs`:

```rust
//! Deterministic workflow agents (SMA-325): [`SequentialAgent`],
//! [`ParallelAgent`], [`LoopAgent`].
//!
//! Each implements the same [`crate::Agent`] trait as `LlmAgent` and drives
//! sub-agents, merging their event streams with the handoff-driver convention:
//! swallow each child's `RunStarted`, fold `RunCompleted.usage` into a running
//! total, pass everything else through, and emit one outer `RunStarted` /
//! `RunCompleted`. Sub-agents coordinate through the run-scoped
//! [`crate::SessionState`]; each workflow agent auto-writes a child's final text
//! to `state[key]`.

use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;

use crate::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunConfig, RunContext, TokenUsage,
};

/// Concatenate the `ContentPart::Text` of an `Item::AssistantMessage`.
fn assistant_text(item: &Item) -> Option<String> {
    match item {
        Item::AssistantMessage { content, .. } => Some(
            content
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// The effective `max_agent_depth` for a (sub-)run.
fn max_depth(run_config: Option<&RunConfig>) -> u32 {
    run_config
        .map(|c| c.max_agent_depth)
        .unwrap_or_else(|| RunConfig::default().max_agent_depth)
}

/// Runs sub-agents in order, threading the shared [`crate::SessionState`].
///
/// After each step completes, its final text is written to `state[key]` (key =
/// the agent's name, or an explicit key via [`SequentialAgent::then_keyed`]), so a
/// later step's dynamic `Instructions` closure can read it. Fail-fast on the first
/// step failure. The outer `RunCompleted` carries usage summed across all steps.
pub struct SequentialAgent<Ctx> {
    name: String,
    description: String,
    agents: Vec<(String, Arc<dyn Agent<Ctx>>)>,
}

impl<Ctx> SequentialAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct an empty sequence.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            agents: Vec::new(),
        }
    }

    /// Append a step keyed by the agent's own name.
    pub fn then(mut self, agent: impl Agent<Ctx> + 'static) -> Self {
        let key = agent.name().to_owned();
        self.agents.push((key, Arc::new(agent)));
        self
    }

    /// Append a step with an explicit state key (use when a name would collide).
    pub fn then_keyed(mut self, key: impl Into<String>, agent: impl Agent<Ctx> + 'static) -> Self {
        self.agents.push((key.into(), Arc::new(agent)));
        self
    }

    /// Append a pre-wrapped step keyed by the agent's name.
    pub fn then_shared(mut self, agent: Arc<dyn Agent<Ctx>>) -> Self {
        let key = agent.name().to_owned();
        self.agents.push((key, agent));
        self
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for SequentialAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }

    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let name = self.name.clone();
        let agents = self.agents.clone();

        let stream = async_stream::stream! {
            let parent_failure = ctx.failure_handle();
            yield AgentEvent::RunStarted { agent: name.clone() };

            let max = max_depth(ctx.run_config());
            if ctx.agent_depth() + 1 > max {
                let err = AgentError::MaxAgentDepthExceeded { depth: ctx.agent_depth() + 1, max };
                let msg = err.to_string();
                parent_failure.set(err);
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            let mut total = TokenUsage::default();
            for (key, agent) in &agents {
                let child = ctx.subagent_child();
                let failure = child.failure_handle();
                yield AgentEvent::AgentUpdated { agent: agent.name().to_owned() };

                let mut sub = match agent.run(child, input.clone()).await {
                    Ok(s) => s,
                    Err(e) => {
                        let msg = e.to_string();
                        parent_failure.set(e);
                        yield AgentEvent::RunFailed { error: msg };
                        return;
                    }
                };

                let mut last_text = String::new();
                let mut failed = false;
                while let Some(ev) = sub.next().await {
                    match ev {
                        AgentEvent::RunStarted { .. } => {}
                        AgentEvent::RunCompleted { usage } => total.add(usage),
                        AgentEvent::RunFailed { error } => {
                            failed = true;
                            yield AgentEvent::RunFailed { error };
                        }
                        AgentEvent::MessageOutput { item } => {
                            if let Some(t) = assistant_text(&item) {
                                last_text = t;
                            }
                            yield AgentEvent::MessageOutput { item };
                        }
                        other => yield other,
                    }
                }

                if failed {
                    if let Some(e) = failure.take() {
                        parent_failure.set(e);
                    }
                    return;
                }
                ctx.state().set(key.clone(), last_text);
            }

            yield AgentEvent::RunCompleted { usage: total };
        };

        Ok(Box::pin(stream))
    }
}
```

- [ ] **Step 4: Wire `workflow` into `lib.rs`**

In `lib.rs`, add `pub mod workflow;` (after `pub mod tool;`) and `pub use workflow::*;` (after `pub use tool::*;`).

- [ ] **Step 5: Run the test**

Run: `cargo test -p paigasus-helikon-core --test workflow_sequential`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/workflow.rs crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/tests/workflow_sequential.rs
git commit -m "feat(core): SMA-325 add SequentialAgent"
```

---

## Task 8: `ParallelAgent`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/workflow.rs`
- Test: `crates/paigasus-helikon-core/tests/workflow_parallel.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/paigasus-helikon-core/tests/workflow_parallel.rs`:

```rust
//! SMA-325 — ParallelAgent: concurrent branches, disjoint state keys,
//! deterministic final_output, collect-all failure.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::{msg_and_complete, MockAgent};
use futures_util::StreamExt as _;
use paigasus_helikon_core::{
    AgentError, AgentEvent, AgentInput, CancellationToken, HookRegistry, MemorySession,
    ParallelAgent, RunContext, RunError, RunResultStreaming, Session, TracerHandle,
};
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

#[tokio::test]
async fn writes_disjoint_keys_sums_usage_deterministic_output() {
    let pa = ParallelAgent::new("fetch", "fetch A and B")
        .add(MockAgent::new("fetchA", |_| msg_and_complete("fetchA", "data-A", 3)))
        .add(MockAgent::new("fetchB", |_| msg_and_complete("fetchB", "data-B", 4)));

    let ctx = ctx();
    let state = ctx.state().clone();
    let result = RunResultStreaming::new(pa.run(ctx, AgentInput::from_user_text("go")).await.unwrap())
        .collect()
        .await
        .unwrap();

    assert_eq!(state.get("fetchA"), Some(json!("data-A")));
    assert_eq!(state.get("fetchB"), Some(json!("data-B")));
    assert_eq!(result.usage.total_tokens, 7, "summed across branches");
    assert_eq!(
        result.final_output,
        r#"{"fetchA":"data-A","fetchB":"data-B"}"#,
        "deterministic sorted-key JSON"
    );
}

#[tokio::test]
async fn one_branch_fails_emits_single_aggregate_run_failed() {
    let ok = MockAgent::new("ok", |_| msg_and_complete("ok", "fine", 0));
    let bad = MockAgent::new("bad", |ctx| {
        ctx.failure_handle().set(AgentError::Other(anyhow::anyhow!("nope")));
        vec![
            AgentEvent::RunStarted { agent: "bad".to_owned() },
            AgentEvent::RunFailed { error: "nope".to_owned() },
        ]
    });
    let pa = ParallelAgent::new("p", "").add(ok).add(bad);

    // Drain manually to assert exactly one aggregate RunFailed (child's swallowed).
    let mut stream = pa.run(ctx(), AgentInput::from_user_text("go")).await.unwrap();
    let mut events = Vec::new();
    while let Some(e) = stream.next().await {
        events.push(e);
    }
    let fails = events.iter().filter(|e| matches!(e, AgentEvent::RunFailed { .. })).count();
    assert_eq!(fails, 1, "one aggregate RunFailed; child RunFailed swallowed");

    // And the structured error reaches collect via the failure slot.
    let ctx = ctx();
    let failure = ctx.failure_handle();
    let pa2 = ParallelAgent::new("p", "")
        .add(MockAgent::new("ok", |_| msg_and_complete("ok", "fine", 0)))
        .add(MockAgent::new("bad", |ctx| {
            ctx.failure_handle().set(AgentError::Other(anyhow::anyhow!("nope")));
            vec![
                AgentEvent::RunStarted { agent: "bad".to_owned() },
                AgentEvent::RunFailed { error: "nope".to_owned() },
            ]
        }));
    let err = RunResultStreaming::with_failure(pa2.run(ctx, AgentInput::from_user_text("go")).await.unwrap(), failure)
        .collect()
        .await
        .expect_err("aggregate failure");
    assert!(matches!(err, RunError::Agent(AgentError::Other(_))), "got {err:?}");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-core --test workflow_parallel`
Expected: FAIL to compile — `ParallelAgent` not found.

- [ ] **Step 3: Implement `ParallelAgent`**

Append to `crates/paigasus-helikon-core/src/workflow.rs`:

```rust
/// Runs sub-agents concurrently (cooperative `futures::stream::select_all` — core
/// has no tokio runtime), interleaving their events live. Each branch is keyed;
/// on completion its final text is written to `state[key]` (disjoint keys → safe).
///
/// `final_output` is deterministic: a synthesized terminal `MessageOutput` carrying
/// a sorted-key JSON object `{key: branch_output}` is emitted before the outer
/// `RunCompleted`. Per-branch results are addressed individually via `state[key]`.
/// Failure is **collect-all**: child `RunFailed` events are swallowed, siblings
/// finish, and one aggregate `RunFailed` is emitted.
///
/// Cooperative concurrency suits IO-bound `model.invoke`; a CPU-bound branch would
/// starve siblings between `.await` points. This is not OS-thread parallelism.
pub struct ParallelAgent<Ctx> {
    name: String,
    description: String,
    branches: Vec<(String, Arc<dyn Agent<Ctx>>)>,
}

impl<Ctx> ParallelAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct an empty parallel block.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            branches: Vec::new(),
        }
    }

    /// Add a branch keyed by the agent's own name.
    pub fn add(mut self, agent: impl Agent<Ctx> + 'static) -> Self {
        let key = agent.name().to_owned();
        self.branches.push((key, Arc::new(agent)));
        self
    }

    /// Add a branch with an explicit state key.
    pub fn branch(mut self, key: impl Into<String>, agent: impl Agent<Ctx> + 'static) -> Self {
        self.branches.push((key.into(), Arc::new(agent)));
        self
    }

    /// Add a pre-wrapped branch keyed by the agent's name.
    pub fn add_shared(mut self, agent: Arc<dyn Agent<Ctx>>) -> Self {
        let key = agent.name().to_owned();
        self.branches.push((key, agent));
        self
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for ParallelAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }

    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let name = self.name.clone();
        let branches = self.branches.clone();

        let stream = async_stream::stream! {
            let parent_failure = ctx.failure_handle();
            yield AgentEvent::RunStarted { agent: name.clone() };

            let max = max_depth(ctx.run_config());
            if ctx.agent_depth() + 1 > max {
                let err = AgentError::MaxAgentDepthExceeded { depth: ctx.agent_depth() + 1, max };
                let msg = err.to_string();
                parent_failure.set(err);
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            // Start every branch; tag its stream with the branch index.
            let mut tagged: Vec<BoxStream<'static, (usize, AgentEvent)>> = Vec::new();
            let mut failures: Vec<crate::FailureSlot> = Vec::new();
            for (i, (_key, agent)) in branches.iter().enumerate() {
                let child = ctx.subagent_child();
                failures.push(child.failure_handle());
                yield AgentEvent::AgentUpdated { agent: agent.name().to_owned() };
                match agent.run(child, input.clone()).await {
                    Ok(s) => tagged.push(Box::pin(s.map(move |ev| (i, ev)))),
                    Err(e) => {
                        let msg = e.to_string();
                        parent_failure.set(e);
                        yield AgentEvent::RunFailed { error: msg };
                        return;
                    }
                }
            }

            let mut merged = futures_util::stream::select_all(tagged);
            let mut total = TokenUsage::default();
            let mut finals: Vec<String> = vec![String::new(); branches.len()];
            let mut completed: std::collections::BTreeMap<String, String> =
                std::collections::BTreeMap::new();
            let mut saw_failure = false;

            while let Some((i, ev)) = merged.next().await {
                match ev {
                    AgentEvent::RunStarted { .. } => {}
                    AgentEvent::RunCompleted { usage } => {
                        total.add(usage);
                        let key = branches[i].0.clone();
                        ctx.state().set(key.clone(), finals[i].clone());
                        completed.insert(key, finals[i].clone());
                    }
                    AgentEvent::RunFailed { .. } => saw_failure = true,
                    AgentEvent::MessageOutput { item } => {
                        if let Some(t) = assistant_text(&item) {
                            finals[i] = t;
                        }
                        yield AgentEvent::MessageOutput { item };
                    }
                    other => yield other,
                }
            }

            let mut first_err: Option<AgentError> = None;
            for fh in &failures {
                if let Some(e) = fh.take() {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
            if saw_failure || first_err.is_some() {
                let err = first_err
                    .unwrap_or_else(|| AgentError::Other(anyhow::anyhow!("a parallel branch failed")));
                let msg = err.to_string();
                parent_failure.set(err);
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            let json = serde_json::to_string(&completed).unwrap_or_else(|_| "{}".to_owned());
            yield AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text { text: json }],
                    agent: Some(name.clone()),
                },
            };
            yield AgentEvent::RunCompleted { usage: total };
        };

        Ok(Box::pin(stream))
    }
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p paigasus-helikon-core --test workflow_parallel`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/workflow.rs crates/paigasus-helikon-core/tests/workflow_parallel.rs
git commit -m "feat(core): SMA-325 add ParallelAgent"
```

---

## Task 9: `LoopAgent`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/workflow.rs`
- Test: `crates/paigasus-helikon-core/tests/workflow_loop.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/paigasus-helikon-core/tests/workflow_loop.rs`:

```rust
//! SMA-325 — LoopAgent: escalate exits; exhaustion fails.

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use common::{msg_and_complete, MockAgent};
use paigasus_helikon_core::{
    AgentError, AgentEvent, AgentInput, CancellationToken, HookRegistry, LoopAgent, MemorySession,
    RunContext, RunError, RunResultStreaming, Session, TracerHandle,
};

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
async fn escalate_stops_after_that_iteration() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let worker = MockAgent::new("worker", move |ctx| {
        let n = c.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= 2 {
            ctx.actions().escalate();
        }
        msg_and_complete("worker", &format!("iter {n}"), 0)
    });
    let la = LoopAgent::new("loop", "until escalate", 5).then(worker);

    let result = RunResultStreaming::new(la.run(ctx(), AgentInput::from_user_text("go")).await.unwrap())
        .collect()
        .await
        .unwrap();

    let runs = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::AgentUpdated { agent } if agent == "worker"))
        .count();
    assert_eq!(runs, 2, "escalate on iteration 2 → exactly 2 runs");
    assert!(result.events.iter().any(|e| matches!(e, AgentEvent::RunCompleted { .. })));
    assert!(!result.events.iter().any(|e| matches!(e, AgentEvent::RunFailed { .. })));
}

#[tokio::test]
async fn exhausting_max_iterations_fails() {
    let worker = MockAgent::new("worker", |_| msg_and_complete("worker", "again", 0));
    let la = LoopAgent::new("loop", "never escalates", 3).then(worker);

    let ctx = ctx();
    let failure = ctx.failure_handle();
    let err = RunResultStreaming::with_failure(la.run(ctx, AgentInput::from_user_text("go")).await.unwrap(), failure)
        .collect()
        .await
        .expect_err("exhausted");

    match err {
        RunError::Agent(AgentError::MaxIterationsExceeded { max }) => assert_eq!(max, 3),
        other => panic!("expected MaxIterationsExceeded, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-core --test workflow_loop`
Expected: FAIL to compile — `LoopAgent` not found.

- [ ] **Step 3: Implement `LoopAgent`**

Append to `crates/paigasus-helikon-core/src/workflow.rs`:

```rust
/// Repeats sub-agents (in order) up to `max_iterations`. After each sub-agent
/// completes, its final text is written to `state[key]` and its
/// [`crate::ActionsHandle`] is checked: if a tool escalated, the loop emits
/// `RunCompleted` and stops (success). Exhausting `max_iterations` without an
/// escalate emits `RunFailed` with [`AgentError::MaxIterationsExceeded`].
///
/// Escalate is **iteration-level** — it means "no more iterations," not "stop the
/// current sub-agent now"; the active sub-agent finishes its run first.
pub struct LoopAgent<Ctx> {
    name: String,
    description: String,
    agents: Vec<(String, Arc<dyn Agent<Ctx>>)>,
    max_iterations: u32,
}

impl<Ctx> LoopAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct an empty loop with the given iteration budget.
    pub fn new(name: impl Into<String>, description: impl Into<String>, max_iterations: u32) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            agents: Vec::new(),
            max_iterations,
        }
    }

    /// Append a sub-agent keyed by its own name.
    pub fn then(mut self, agent: impl Agent<Ctx> + 'static) -> Self {
        let key = agent.name().to_owned();
        self.agents.push((key, Arc::new(agent)));
        self
    }

    /// Append a sub-agent with an explicit state key.
    pub fn then_keyed(mut self, key: impl Into<String>, agent: impl Agent<Ctx> + 'static) -> Self {
        self.agents.push((key.into(), Arc::new(agent)));
        self
    }

    /// Append a pre-wrapped sub-agent keyed by its name.
    pub fn then_shared(mut self, agent: Arc<dyn Agent<Ctx>>) -> Self {
        let key = agent.name().to_owned();
        self.agents.push((key, agent));
        self
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for LoopAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }

    async fn run(
        &self,
        ctx: RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        let name = self.name.clone();
        let agents = self.agents.clone();
        let max_iterations = self.max_iterations;

        let stream = async_stream::stream! {
            let parent_failure = ctx.failure_handle();
            yield AgentEvent::RunStarted { agent: name.clone() };

            let max = max_depth(ctx.run_config());
            if ctx.agent_depth() + 1 > max {
                let err = AgentError::MaxAgentDepthExceeded { depth: ctx.agent_depth() + 1, max };
                let msg = err.to_string();
                parent_failure.set(err);
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            let mut total = TokenUsage::default();
            for _iteration in 0..max_iterations {
                for (key, agent) in &agents {
                    let child = ctx.subagent_child();
                    let actions = child.actions().clone();
                    let failure = child.failure_handle();
                    yield AgentEvent::AgentUpdated { agent: agent.name().to_owned() };

                    let mut sub = match agent.run(child, input.clone()).await {
                        Ok(s) => s,
                        Err(e) => {
                            let msg = e.to_string();
                            parent_failure.set(e);
                            yield AgentEvent::RunFailed { error: msg };
                            return;
                        }
                    };

                    let mut last_text = String::new();
                    let mut failed = false;
                    while let Some(ev) = sub.next().await {
                        match ev {
                            AgentEvent::RunStarted { .. } => {}
                            AgentEvent::RunCompleted { usage } => total.add(usage),
                            AgentEvent::RunFailed { error } => {
                                failed = true;
                                yield AgentEvent::RunFailed { error };
                            }
                            AgentEvent::MessageOutput { item } => {
                                if let Some(t) = assistant_text(&item) {
                                    last_text = t;
                                }
                                yield AgentEvent::MessageOutput { item };
                            }
                            other => yield other,
                        }
                    }

                    if failed {
                        if let Some(e) = failure.take() {
                            parent_failure.set(e);
                        }
                        return;
                    }
                    ctx.state().set(key.clone(), last_text);

                    if actions.is_escalated() {
                        yield AgentEvent::RunCompleted { usage: total };
                        return;
                    }
                }
            }

            let err = AgentError::MaxIterationsExceeded { max: max_iterations };
            let msg = err.to_string();
            parent_failure.set(err);
            yield AgentEvent::RunFailed { error: msg };
        };

        Ok(Box::pin(stream))
    }
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p paigasus-helikon-core --test workflow_loop`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/workflow.rs crates/paigasus-helikon-core/tests/workflow_loop.rs
git commit -m "feat(core): SMA-325 add LoopAgent"
```

---

## Task 10: Full-chain escalate test (real `LlmAgent` + tool)

Proves escalate travels tool → `ToolContext` → `LoopAgent` through a real `LlmAgent` driven by the scripted mock `Model`.

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/workflow_loop.rs`

- [ ] **Step 1: Add the failing test**

Append to `tests/workflow_loop.rs`:

(a) Extend the imports at the top to add `FinishReason`, `LlmAgent`, `ModelEvent`, `Tool`, and the `common` items:
```rust
use common::{msg_and_complete, EscalatingTool, MockAgent};
use paigasus_helikon_core::{
    AgentError, AgentEvent, AgentInput, CancellationToken, FinishReason, HookRegistry, LlmAgent,
    LoopAgent, MemorySession, ModelEvent, RunContext, RunError, RunResultStreaming, Session, Tool,
    TracerHandle,
};
```
(b) Add `use common::MockModel;` (it lives in the same `common` module).

(c) Add the test:
```rust
#[tokio::test]
async fn escalate_from_real_tool_stops_the_loop() {
    // The looped agent: turn 1 calls the "done" tool (which escalates); turn 2
    // emits final text.
    let worker = LlmAgent::builder::<()>()
        .name("worker")
        .description("does one unit of work")
        .shared_model(MockModel::with_scripts(vec![
            vec![
                ModelEvent::ToolCallDelta {
                    call_id: "c1".to_owned(),
                    name: Some("done".to_owned()),
                    args_delta: "{}".to_owned(),
                },
                ModelEvent::Finish { reason: FinishReason::ToolCalls },
            ],
            vec![
                ModelEvent::TokenDelta { text: "finished".to_owned() },
                ModelEvent::Finish { reason: FinishReason::Stop },
            ],
        ]))
        .shared_tool(EscalatingTool::new("done") as Arc<dyn Tool<()>>)
        .build();

    let la = LoopAgent::new("refine", "loop until the tool escalates", 5).then(worker);

    let result = RunResultStreaming::new(la.run(ctx(), AgentInput::from_user_text("go")).await.unwrap())
        .collect()
        .await
        .unwrap();

    let worker_runs = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::AgentUpdated { agent } if agent == "worker"))
        .count();
    assert_eq!(worker_runs, 1, "tool escalate stops after the first iteration");
    assert!(result.events.iter().any(|e| matches!(e, AgentEvent::RunCompleted { .. })));
    assert!(!result.events.iter().any(|e| matches!(e, AgentEvent::RunFailed { .. })));
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p paigasus-helikon-core --test workflow_loop escalate_from_real_tool_stops_the_loop`
Expected: PASS (proves the tool → actions → loop chain).

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-core/tests/workflow_loop.rs
git commit -m "test(core): SMA-325 prove tool escalate stops LoopAgent end-to-end"
```

---

## Task 11: Acceptance pipeline + depth-bound tests

**Files:**
- Create: `crates/paigasus-helikon-core/tests/workflow_pipeline.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/paigasus-helikon-core/tests/workflow_pipeline.rs`:

```rust
//! SMA-325 — acceptance criterion 1 (Sequential([Parallel, summarize])) and the
//! agent-nesting depth bound.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::{msg_and_complete, MockAgent};
use paigasus_helikon_core::{
    AgentInput, CancellationToken, HookRegistry, MemorySession, ParallelAgent, RunConfig, RunContext,
    RunResultStreaming, SequentialAgent, Session, TracerHandle,
};
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

#[tokio::test]
async fn sequential_parallel_summarize_pipeline() {
    let fetch = ParallelAgent::new("fetch", "fetch A and B")
        .add(MockAgent::new("fetchA", |_| msg_and_complete("fetchA", "data-A", 0)))
        .add(MockAgent::new("fetchB", |_| msg_and_complete("fetchB", "data-B", 0)));

    let summarize = MockAgent::new("summarize", |ctx| {
        let a = ctx.state().get("fetchA").and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
        let b = ctx.state().get("fetchB").and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
        msg_and_complete("summarize", &format!("A={a};B={b}"), 0)
    });

    let pipeline = SequentialAgent::new("pipeline", "fetch then summarize")
        .then(fetch)
        .then(summarize);

    let ctx = ctx();
    let state = ctx.state().clone();
    let result = RunResultStreaming::new(pipeline.run(ctx, AgentInput::from_user_text("go")).await.unwrap())
        .collect()
        .await
        .unwrap();

    // summarize ran AFTER the parallel block (it observed both keys).
    assert_eq!(result.final_output, "A=data-A;B=data-B");
    assert_eq!(state.get("fetchA"), Some(json!("data-A")));
    assert_eq!(state.get("fetchB"), Some(json!("data-B")));
}

#[tokio::test]
async fn nested_workflow_agents_respect_max_agent_depth() {
    let inner = SequentialAgent::new("inner", "")
        .then(MockAgent::new("leaf", |_| msg_and_complete("leaf", "x", 0)));
    let outer = SequentialAgent::new("outer", "").then(inner);

    // max_agent_depth = 1: outer(0)->inner(1) ok; inner(1)->leaf(2) exceeds.
    let ctx = ctx().with_run_config(RunConfig::new().with_max_agent_depth(1));
    let err = RunResultStreaming::new(outer.run(ctx, AgentInput::from_user_text("go")).await.unwrap())
        .collect()
        .await
        .expect_err("depth exceeded");
    assert!(err.to_string().contains("nesting depth"), "got: {err}");
}
```

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test -p paigasus-helikon-core --test workflow_pipeline`
Expected: PASS (both tests) — all referenced types already exist from Tasks 7–9. (If a fresh checkout, this is the first run; it should pass directly. If it fails, fix the implementation, not the test.)

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-core/tests/workflow_pipeline.rs
git commit -m "test(core): SMA-325 add acceptance pipeline and depth-bound tests"
```

---

## Task 12: Full CI-gate sweep + docs verification

No new code — verify the whole change passes every gate the PR will face.

**Files:** none (verification only).

- [ ] **Step 1: Format**

Run: `cargo fmt --all -- --check`
Expected: clean. If it reports diffs, run `cargo fmt --all` and commit:
```bash
git add -u && git commit -m "style(core): SMA-325 cargo fmt"
```

- [ ] **Step 2: Clippy (all features, all targets, warnings = errors)**

Run: `cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: clean. Fix any lint (a likely one: `clippy::field_reassign_with_default` in `usage_total` — if it fires, prefix the helper with `#[allow(clippy::field_reassign_with_default)]`). Commit fixes:
```bash
git add -u && git commit -m "fix(core): SMA-325 satisfy clippy"
```

- [ ] **Step 3: Full test suite (all features)**

Run: `cargo test --workspace --all-features`
Expected: PASS, including the four new `workflow_*` binaries.

- [ ] **Step 4: Docs build (warnings = errors)**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
Expected: clean — every new `pub` item has a `///` doc. (The known facade↔CLI name-collision warning is pre-existing and unrelated.)

- [ ] **Step 5: Doc coverage gate**

Run:
```bash
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
```
Expected: PASS (≥80%). If a new public item lacks docs, add a `///` line and re-run.

- [ ] **Step 6: Confirm no manual release edits crept in**

Run: `git diff --stat main -- crates/paigasus-helikon-core/Cargo.toml crates/paigasus-helikon-core/CHANGELOG.md Cargo.toml`
Expected: **empty** — the version, CHANGELOG, and workspace pins are untouched; release-plz owns them. If anything shows, revert it.

- [ ] **Step 7: Final commit (if any gate produced fixes) and push**

```bash
git push -u origin feature/sma-325-workflow-agents-sequentialagent-parallelagent-loopagent
```

PR title (the gated squashed-commit title) must be:
`feat(core): SMA-325 add workflow agents (SequentialAgent, ParallelAgent, LoopAgent)`

---

## Self-Review (completed by plan author)

**Spec coverage:**
- §3.1 `SessionState` → Task 1. §3.2 `EventActions`/`ActionsHandle` → Task 2. §3.3 context wiring → Tasks 4 (ToolContext) + 5 (RunContext, `subagent_child`, `to_tool_context`, `handoff_child`). §6 `MaxIterationsExceeded` → Task 3.
- §4 merge convention + §4.1 Sequential → Task 7. §4.2 Parallel (live interleave, disjoint keys, deterministic `final_output`, collect-all) → Task 8. §4.3 Loop (escalate, exhaustion) → Task 9.
- §4.4 acceptance pipeline → Task 11. Full-chain escalate (§4.3 chain claim) → Task 10. Depth bound (§4) → Task 11.
- §5 construction API (`then`/`then_keyed`/`add`/`branch`/`*_shared`) → Tasks 7–9. §7 testing (MockAgent, EscalatingTool, order-normalized parallel assertions) → Tasks 6, 8. §8 exports → Tasks 1, 7. §9 release (no manual bump) → Task 12.
- Out of scope (§10) correctly omitted: no `output_key`, no state persistence, no fail-fast cancellation, no LlmAgent turn-loop short-circuit.

**Placeholder scan:** none — every code step is complete and compilable.

**Type consistency:** `subagent_child`, `state()`, `actions()`, `with_state`/`with_actions`, `assistant_text`, `max_depth`, `MockAgent::new`, `msg_and_complete`, `usage_total`, `EscalatingTool::new`, and the builder methods are used with identical signatures across tasks. `ParallelAgent` final_output JSON uses a `BTreeMap` (sorted keys) → matches the exact-string assertion in Task 8.
