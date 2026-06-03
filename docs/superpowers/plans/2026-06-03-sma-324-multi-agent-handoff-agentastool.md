# Multi-agent: Handoff + `AgentAsTool` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the first multi-agent primitives to `paigasus-helikon-core` — `Handoff<Ctx>` (transfer the conversation to another agent via injected `transfer_to_<name>` tools) and `AgentAsTool<Ctx>` (wrap any agent as a callable tool) — both composing through the existing `Agent<Ctx>` trait.

**Architecture:** Handoff is **nested delegation**: the pure `transition` state machine routes a `transfer_to_*` tool call to `LoopState::ApplyingHandoff`, and the `LlmAgent::run` driver runs the *target's* `Agent::run` with a threaded transcript, forwarding its events. `AgentAsTool::invoke` builds an **isolated** sub-`RunContext` (fresh `MemorySession`, empty hooks) from the `ToolContext` and returns the sub-agent's `final_output` as `ToolOutput`. A unified `agent_depth`/`max_agent_depth` counter bounds nesting across both mechanisms.

**Tech Stack:** Rust, `async-trait`, `async-stream`, `tokio`, `futures`, `serde_json`. Tests use the existing `MockModel` scripted-events harness (`crates/paigasus-helikon-core/tests/common/mod.rs`).

**Design reference:** `docs/superpowers/specs/2026-06-03-sma-324-multi-agent-handoff-agentastool-design.md` (read it first; this plan implements it section-by-section).

**Breaking change:** This is a breaking change to `core`'s public API (retyped `LlmAgent.handoffs`, new fields on `LoopState::ApplyingHandoff` / `TransitionCtx`, changed `ToolContext::new` signature). Per `CLAUDE.md`, do **not** hand-bump versions — release-plz proposes `core 0.3.0 → 0.4.0`. The squashed PR title must be `feat(core)!: SMA-324 …` (see Task 9). Per-commit titles below use `feat(core): …` (no `!`), which `convco` accepts.

---

## File Structure

**New files (core):**
- `crates/paigasus-helikon-core/src/handoff.rs` — `Handoff<Ctx>` wrapper + `HandoffDef` (pure data the state machine consumes) + the `slug` / `transfer_to_*` naming.
- `crates/paigasus-helikon-core/src/agent_as_tool.rs` — `AgentAsTool<Ctx>` adapter (`impl Tool<Ctx>`).

**Modified files (core):**
- `src/runner.rs` — `RunConfig.max_agent_depth` + builder.
- `src/agent.rs` — `AgentError::MaxAgentDepthExceeded`; retype `LlmAgent.handoffs`; the driver's handoff delegation + collision check.
- `src/context.rs` — `RunContext.agent_depth` + `handoff_child` + `with_agent_depth`; `HookRegistry: Clone`; `to_tool_context` projection.
- `src/tool.rs` — `ToolContext` gains `agent_depth` + `max_agent_depth` (and `new` signature).
- `src/loop_state.rs` — `ApplyingHandoff.usage`; `NextAction::Handoff`; `TransitionCtx.handoffs`; the routing branch + transcript/tool-merge helpers.
- `src/agent_builder.rs` — builder `.handoff` / `.shared_handoff` / `.handoffs` retyped to `Handoff<Ctx>`.
- `src/lib.rs` — `pub mod handoff; pub mod agent_as_tool;` + re-exports.

**New tests (core):**
- `crates/paigasus-helikon-core/tests/handoff.rs` — 3-agent triage routing, collisions, depth guard.
- `crates/paigasus-helikon-core/tests/agent_as_tool.rs` — round-trip, isolation, failure, depth.
- additions to `crates/paigasus-helikon-core/tests/transition_unit.rs`.

**New example (facade):**
- `crates/paigasus-helikon/examples/multi_agent_triage.rs` + a `[[example]]` entry in `crates/paigasus-helikon/Cargo.toml`.

---

## Task 1: `RunConfig.max_agent_depth` + `AgentError::MaxAgentDepthExceeded`

Additive scaffolding (both enums/structs already `#[non_exhaustive]`), compilable on its own.

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs` (the `RunConfig` struct, its `Default`, and `runconfig_tests`)
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (the `AgentError` enum)

- [ ] **Step 1: Add the `AgentError` variant**

In `src/agent.rs`, inside `pub enum AgentError`, add this variant just before `Other`:

```rust
    /// A handoff chain or `AgentAsTool` nesting exceeded
    /// [`crate::RunConfig::max_agent_depth`].
    #[error("agent nesting depth ({depth}) exceeded max ({max})")]
    MaxAgentDepthExceeded {
        /// The depth that would have been entered.
        depth: u32,
        /// The configured maximum.
        max: u32,
    },
```

- [ ] **Step 2: Add the `RunConfig` field + builder + default**

In `src/runner.rs`, add the field to `pub struct RunConfig` (after `parallel_tool_call_limit`):

```rust
    /// `[driver-scoped]` Maximum agent-nesting depth across **both** handoff
    /// chains and `AgentAsTool` sub-runs. Each nested agent run increments the
    /// depth; exceeding this fails with
    /// [`crate::AgentError::MaxAgentDepthExceeded`]. Default `8`.
    pub max_agent_depth: u32,
```

Update `impl Default for RunConfig` to add `max_agent_depth: 8,`. Then add a builder method inside `impl RunConfig` (next to `with_parallel_tool_call_limit`):

```rust
    /// Set the maximum agent-nesting depth (handoff + agent-as-tool). Honored by the core loop driver.
    pub fn with_max_agent_depth(mut self, depth: u32) -> Self {
        self.max_agent_depth = depth;
        self
    }
```

- [ ] **Step 3: Extend the existing `runconfig_tests`**

In `src/runner.rs`'s `mod runconfig_tests`, add to `run_config_defaults_and_builders` after the existing default asserts:

```rust
        assert_eq!(c.max_agent_depth, 8);
```

and after the builder chain, extend it / add an assertion:

```rust
        let c = RunConfig::new().with_max_agent_depth(3);
        assert_eq!(c.max_agent_depth, 3);
```

- [ ] **Step 4: Build + test**

Run: `cargo test -p paigasus-helikon-core --lib runconfig_tests`
Expected: PASS (and the crate compiles).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/runner.rs crates/paigasus-helikon-core/src/agent.rs
git commit -m "feat(core): SMA-324 add max_agent_depth + MaxAgentDepthExceeded"
```

---

## Task 2: `RunContext` depth + `handoff_child` + `HookRegistry: Clone`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs` (`RunContext`, `HookRegistry`)

- [ ] **Step 1: Write the failing test**

In `src/context.rs`'s `mod runcontext_tests`, add:

```rust
    #[test]
    fn handoff_child_increments_depth_and_shares_failure_slot() {
        use crate::AgentError;
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        assert_eq!(ctx.agent_depth(), 0);

        let child = ctx.handoff_child();
        assert_eq!(child.agent_depth(), 1);
        assert_eq!(child.handoff_child().agent_depth(), 2);

        // The child shares the parent's failure slot (so a failing target
        // reaches the parent's boundary).
        child.failure_handle().set(AgentError::MaxTurnsExceeded(2));
        assert!(matches!(
            ctx.failure_handle().take(),
            Some(AgentError::MaxTurnsExceeded(2))
        ));
    }

    #[test]
    fn with_agent_depth_sets_depth() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_agent_depth(5);
        assert_eq!(ctx.agent_depth(), 5);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p paigasus-helikon-core --lib runcontext_tests`
Expected: FAIL — `no method named agent_depth` / `handoff_child` / `with_agent_depth`.

- [ ] **Step 3: Add `Clone` for `HookRegistry`**

In `src/context.rs`, after the `impl<Ctx> HookRegistry<Ctx>` block, add:

```rust
impl<Ctx> Clone for HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            hooks: self.hooks.clone(),
        }
    }
}
```

- [ ] **Step 4: Add the `agent_depth` field + methods to `RunContext`**

Add the field to `pub struct RunContext` (after `failure`):

```rust
    /// Agent-nesting depth: 0 for a top-level run, incremented by
    /// [`RunContext::handoff_child`] and by `AgentAsTool` for each nested
    /// agent run. Bounded by [`crate::RunConfig::max_agent_depth`].
    agent_depth: u32,
```

In `RunContext::new`, set `agent_depth: 0,` in the constructed `Self { … }`.

Add these methods inside `impl<Ctx> RunContext<Ctx>` (e.g. after `failure_handle`):

```rust
    /// Nesting depth that produced this context (0 at top level).
    pub fn agent_depth(&self) -> u32 {
        self.agent_depth
    }

    /// Stamp an explicit nesting depth. Used by `AgentAsTool` when it builds
    /// the isolated sub-context for its wrapped agent.
    pub fn with_agent_depth(mut self, depth: u32) -> Self {
        self.agent_depth = depth;
        self
    }

    /// A context for a handed-off sub-run. A handoff *continues the same
    /// logical run*, so the child **shares** session, hooks, cancel token,
    /// failure slot, and run config — with `agent_depth` incremented by one.
    /// (Distinct from `AgentAsTool`, which builds an isolated context.)
    pub fn handoff_child(&self) -> Self {
        Self {
            user_ctx: Arc::clone(&self.user_ctx),
            session: Arc::clone(&self.session),
            hooks: self.hooks.clone(),
            tracer: self.tracer.clone(),
            cancel: self.cancel.clone(),
            run_config: self.run_config.clone(),
            failure: self.failure.clone(),
            agent_depth: self.agent_depth + 1,
        }
    }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p paigasus-helikon-core --lib runcontext_tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/context.rs
git commit -m "feat(core): SMA-324 add RunContext agent_depth + handoff_child"
```

---

## Task 3: `ToolContext` depth scalars + `to_tool_context` projection

`ToolContext::new` gains two `u32` params (a deliberate breaking change). Only in-crate caller is `to_tool_context`.

**Files:**
- Modify: `crates/paigasus-helikon-core/src/tool.rs` (`ToolContext`)
- Modify: `crates/paigasus-helikon-core/src/context.rs` (`to_tool_context`)

- [ ] **Step 1: Find every `ToolContext::new` caller**

Run: `grep -rn "ToolContext::new" crates/`
Expected: one production caller — `src/context.rs::to_tool_context`. (Test files use `&ToolContext` but do not construct it; if any other constructor turns up, update it in Step 4.)

- [ ] **Step 2: Add the fields + getters to `ToolContext`**

In `src/tool.rs`, add to `pub struct ToolContext` (after `cancel`):

```rust
    agent_depth: u32,
    max_agent_depth: u32,
```

Change `pub fn new` to take the two extra params and store them:

```rust
    /// Construct a new [`ToolContext`].
    pub fn new(
        user_ctx: Arc<Ctx>,
        tracer: TracerHandle,
        cancel: CancellationToken,
        agent_depth: u32,
        max_agent_depth: u32,
    ) -> Self {
        Self {
            user_ctx,
            tracer,
            cancel,
            agent_depth,
            max_agent_depth,
        }
    }
```

Add getters inside `impl<Ctx> ToolContext<Ctx>` (after `cancel`):

```rust
    /// Current agent-nesting depth (handoff + agent-as-tool). `AgentAsTool`
    /// reads this to bound recursion.
    pub fn agent_depth(&self) -> u32 {
        self.agent_depth
    }
    /// The configured maximum agent-nesting depth (from `RunConfig`, or the
    /// default when no runner installed a config).
    pub fn max_agent_depth(&self) -> u32 {
        self.max_agent_depth
    }
```

- [ ] **Step 3: Project the scalars in `to_tool_context`**

In `src/context.rs`, replace the body of `to_tool_context`:

```rust
    pub fn to_tool_context(&self) -> ToolContext<Ctx> {
        let max_agent_depth = self
            .run_config
            .as_ref()
            .map(|c| c.max_agent_depth)
            .unwrap_or_else(|| RunConfig::default().max_agent_depth);
        ToolContext::new(
            Arc::clone(&self.user_ctx),
            self.tracer.clone(),
            self.cancel.child_token(),
            self.agent_depth,
            max_agent_depth,
        )
    }
```

(`RunConfig` is already imported in `context.rs`.)

- [ ] **Step 4: Update any other `ToolContext::new` callers found in Step 1**

If Step 1 found callers beyond `to_tool_context`, add `, 0, RunConfig::default().max_agent_depth` (or appropriate values) to each.

- [ ] **Step 5: Build the crate**

Run: `cargo build -p paigasus-helikon-core`
Expected: compiles clean.

- [ ] **Step 6: Add a projection test**

In `src/context.rs`'s `mod runcontext_tests`, add:

```rust
    #[test]
    fn to_tool_context_projects_depth_and_max() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_agent_depth(2)
        .with_run_config(RunConfig::new().with_max_agent_depth(5));

        let tc = ctx.to_tool_context();
        assert_eq!(tc.agent_depth(), 2);
        assert_eq!(tc.max_agent_depth(), 5);
    }
```

Run: `cargo test -p paigasus-helikon-core --lib runcontext_tests`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-core/src/tool.rs crates/paigasus-helikon-core/src/context.rs
git commit -m "feat(core): SMA-324 thread agent_depth into ToolContext"
```

---

## Task 4: `Handoff<Ctx>` + `HandoffDef` (new `handoff.rs`)

**Files:**
- Create: `crates/paigasus-helikon-core/src/handoff.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs`

- [ ] **Step 1: Create `src/handoff.rs`**

```rust
//! Handoff carrier types.
//!
//! A [`Handoff`] is a candidate agent an [`crate::LlmAgent`] may transfer the
//! conversation to. When the agent's `handoffs` list is non-empty, the loop
//! injects a synthetic `transfer_to_<slug>` tool per handoff; a model call to
//! one switches the active agent (see the agent-loop driver).

use std::sync::Arc;

use crate::Agent;

/// A candidate agent the conversation may be transferred to.
///
/// Constructed via [`Handoff::to`] (owned agent) or [`Handoff::shared`]
/// (pre-wrapped trait object). This is intentionally a thin wrapper around
/// `Arc<dyn Agent<Ctx>>`; it is the named home for future per-edge config
/// (tool-name override, transcript input-filter).
pub struct Handoff<Ctx> {
    agent: Arc<dyn Agent<Ctx>>,
}

impl<Ctx> Clone for Handoff<Ctx> {
    fn clone(&self) -> Self {
        Self {
            agent: Arc::clone(&self.agent),
        }
    }
}

impl<Ctx> Handoff<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Transfer target from an owned agent (wrapped in `Arc`).
    pub fn to(agent: impl Agent<Ctx> + 'static) -> Self {
        Self {
            agent: Arc::new(agent),
        }
    }

    /// Transfer target from a pre-wrapped trait object.
    pub fn shared(agent: Arc<dyn Agent<Ctx>>) -> Self {
        Self { agent }
    }

    /// The target agent.
    pub fn agent(&self) -> &Arc<dyn Agent<Ctx>> {
        &self.agent
    }

    /// Project the pure-data [`HandoffDef`] the state machine consumes.
    pub fn to_def(&self) -> HandoffDef {
        HandoffDef {
            tool_name: format!("transfer_to_{}", slug(self.agent.name())),
            target: self.agent.name().to_owned(),
            description: self.agent.description().to_owned(),
        }
    }
}

/// Pure-data description of one injected `transfer_to_*` tool.
///
/// Built by the agent-loop driver from each [`Handoff`] before the run, and
/// passed into [`crate::TransitionCtx`] so the pure state machine can both
/// advertise the transfer tools and recognize a call to one.
#[derive(Debug, Clone)]
pub struct HandoffDef {
    /// The synthetic tool name the model sees, `transfer_to_<slug>`.
    pub tool_name: String,
    /// The **real** target agent name (used in events and target lookup).
    pub target: String,
    /// The target agent's description (shown to the model).
    pub description: String,
}

/// Lowercase `name`, collapsing every run of non-`[a-z0-9_]` to a single `_`,
/// with leading/trailing `_` trimmed. `"Investing specialist"` →
/// `"investing_specialist"`.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_underscore = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    out.trim_matches('_').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentEvent, AgentInput, RunContext};
    use async_trait::async_trait;
    use futures_core::stream::BoxStream;

    struct NamedAgent {
        name: String,
        description: String,
    }

    #[async_trait]
    impl Agent<()> for NamedAgent {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            &self.description
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, crate::AgentError> {
            Ok(Box::pin(futures_util::stream::empty()))
        }
    }

    #[test]
    fn slug_sanitizes_names() {
        assert_eq!(slug("Investing specialist"), "investing_specialist");
        assert_eq!(slug("AML cytogenetics"), "aml_cytogenetics");
        assert_eq!(slug("budgeting"), "budgeting");
        assert_eq!(slug("  weird !! name  "), "weird_name");
    }

    #[test]
    fn to_def_derives_tool_name_target_and_description() {
        let h = Handoff::to(NamedAgent {
            name: "Investing specialist".to_owned(),
            description: "Handles investing questions.".to_owned(),
        });
        let def = h.to_def();
        assert_eq!(def.tool_name, "transfer_to_investing_specialist");
        assert_eq!(def.target, "Investing specialist");
        assert_eq!(def.description, "Handles investing questions.");
    }
}
```

- [ ] **Step 2: Wire the module in `lib.rs`**

In `src/lib.rs`, add `pub mod handoff;` to the module list (alphabetically near `guardrail`/`hook`) and `pub use handoff::*;` to the re-export list.

- [ ] **Step 3: Run the tests**

Run: `cargo test -p paigasus-helikon-core --lib handoff::`
Expected: PASS (both tests).

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/handoff.rs crates/paigasus-helikon-core/src/lib.rs
git commit -m "feat(core): SMA-324 add Handoff + HandoffDef carrier types"
```

---

## Task 5: Retype `LlmAgent.handoffs` to `Vec<Handoff<Ctx>>`

The field is currently `Vec<Arc<dyn Agent<Ctx>>>` and unused by the driver, so retyping it touches only the struct, the builder, and the builder's typestate transitions.

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (`LlmAgent.handoffs`)
- Modify: `crates/paigasus-helikon-core/src/agent_builder.rs` (field + `.handoff` / `.shared_handoff` / `.handoffs` + every struct copy)

- [ ] **Step 1: Retype the struct field**

In `src/agent.rs`, change the `LlmAgent` field:

```rust
    /// Candidate agents this one may hand off to, with the conversation
    /// transferred. Driven by the agent loop (SMA-324).
    pub handoffs: Vec<Handoff<Ctx>>,
```

Add `Handoff` to the `use crate::{…}` import at the top of `agent.rs` (the list that already pulls in `Item`, `ModelError`, etc.).

- [ ] **Step 2: Retype the builder field + setters**

In `src/agent_builder.rs`, change the `LlmAgentBuilder` field:

```rust
    handoffs: Vec<crate::Handoff<Ctx>>,
```

Replace the three handoff setters in the any-state `impl` block:

```rust
    /// Append a handoff candidate (owned agent — wrapped in `Handoff::to`).
    pub fn handoff(mut self, h: impl crate::Agent<Ctx> + 'static) -> Self {
        self.handoffs.push(crate::Handoff::to(h));
        self
    }

    /// Append a pre-wrapped handoff candidate.
    pub fn shared_handoff(mut self, h: std::sync::Arc<dyn crate::Agent<Ctx>>) -> Self {
        self.handoffs.push(crate::Handoff::shared(h));
        self
    }

    /// Replace the handoff candidate list with `Handoff` values.
    pub fn handoffs<I>(mut self, h: I) -> Self
    where
        I: IntoIterator<Item = crate::Handoff<Ctx>>,
    {
        self.handoffs = h.into_iter().collect();
        self
    }
```

- [ ] **Step 3: Confirm the type flows through every typestate copy**

The field name `handoffs` is unchanged, so each `LlmAgentBuilder { … handoffs: self.handoffs … }` struct copy (in `__new`, `.name`, `.shared_model`, `.output_type`, `.build`) keeps compiling unchanged. No edits needed there — but build to confirm.

Run: `cargo build -p paigasus-helikon-core`
Expected: compiles clean.

- [ ] **Step 4: Add a builder test for wrapping**

In `src/agent_builder.rs`'s `mod tests`, add (reusing the existing `StubModel`):

```rust
    #[test]
    fn handoff_setters_wrap_agents() {
        let sub = LlmAgent::builder::<()>().name("sub").model(StubModel).build();
        let agent = LlmAgent::builder::<()>()
            .name("parent")
            .model(StubModel)
            .handoff(sub)
            .build();
        assert_eq!(agent.handoffs.len(), 1);
        assert_eq!(agent.handoffs[0].agent().name(), "sub");
    }
```

(`LlmAgent: Agent<()>` so it is a valid handoff target.)

Run: `cargo test -p paigasus-helikon-core --lib agent_builder`
Expected: PASS (and existing `build_with_required_only_uses_defaults`, which asserts `agent.handoffs.is_empty()`, still passes).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/src/agent_builder.rs
git commit -m "feat(core): SMA-324 retype LlmAgent.handoffs to Vec<Handoff>"
```

---

## Task 6: Drive the handoff — state-machine routing + driver delegation

The core of the feature. Adds the routing branch to the pure `transition`, threads handoff defs through `TransitionCtx`, and implements nested delegation in the `LlmAgent::run` driver (collision check, depth guard, event forwarding, usage chain-sum).

**Files:**
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs` (`ApplyingHandoff`, `NextAction`, `TransitionCtx`, `transition`, helpers)
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (the `run` driver: snapshots, collision check, `TransitionCtx` construction, `NextAction::Handoff` arm)
- Test: `crates/paigasus-helikon-core/tests/transition_unit.rs` (existing — update constructions + add routing tests)
- Test: `crates/paigasus-helikon-core/tests/handoff.rs` (new — end-to-end)

- [ ] **Step 1: Write the failing pure-routing test**

First locate the existing `TransitionCtx { … }` constructions in `tests/transition_unit.rs` and add `handoffs: &[]` to each (the field lands in Step 3). Then add a routing test. Append to `tests/transition_unit.rs`:

```rust
#[test]
fn model_response_with_transfer_call_routes_to_applying_handoff() {
    use paigasus_helikon_core::{
        transition, AgentEvent, ContentPart, HandoffDef, Item, LoopState, ModelSettings,
        NextAction, TokenUsage, TransitionCtx, TransitionInput,
    };

    let defs = vec![HandoffDef {
        tool_name: "transfer_to_budgeting_specialist".to_owned(),
        target: "budgeting specialist".to_owned(),
        description: "Handles budgeting.".to_owned(),
    }];
    // Conversation as the driver would have accumulated it: system + user +
    // assistant text + the transfer tool call.
    let conversation = vec![
        Item::System {
            content: vec![ContentPart::Text { text: "sys".to_owned() }],
        },
        Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "help me budget".to_owned(),
            }],
        },
        Item::AssistantMessage {
            content: vec![ContentPart::Text {
                text: "routing".to_owned(),
            }],
            agent: Some("triage".to_owned()),
        },
        Item::ToolCall {
            call_id: "c1".to_owned(),
            name: "transfer_to_budgeting_specialist".to_owned(),
            args: serde_json::json!({}),
        },
    ];
    let settings = ModelSettings::default();
    let ctx = TransitionCtx {
        tools: &[],
        model_settings: &settings,
        max_turns: 16,
        conversation: &conversation,
        output: None,
        handoffs: &defs,
    };
    let state = LoopState::CallingModel {
        turn: 0,
        usage: TokenUsage::default(),
    };
    let input = TransitionInput::ModelResponse {
        items: vec![
            Item::AssistantMessage {
                content: vec![ContentPart::Text {
                    text: "routing".to_owned(),
                }],
                agent: Some("triage".to_owned()),
            },
            Item::ToolCall {
                call_id: "c1".to_owned(),
                name: "transfer_to_budgeting_specialist".to_owned(),
                args: serde_json::json!({}),
            },
        ],
        usage: TokenUsage::default(),
        finish_reason: paigasus_helikon_core::FinishReason::ToolCalls,
    };

    let outcome = transition(&state, input, &ctx);
    assert!(matches!(outcome.next_action, NextAction::Handoff));
    match outcome.next_state {
        LoopState::ApplyingHandoff {
            target, transcript, ..
        } => {
            assert_eq!(target, "budgeting specialist");
            // System + tool call stripped; ends with the transfer note.
            assert!(!transcript
                .iter()
                .any(|i| matches!(i, Item::System { .. } | Item::ToolCall { .. })));
            match transcript.last() {
                Some(Item::UserMessage { content }) => {
                    let text = match &content[0] {
                        ContentPart::Text { text } => text.as_str(),
                        _ => "",
                    };
                    assert!(text.to_lowercase().contains("transferred"));
                }
                other => panic!("expected trailing transfer note, got {other:?}"),
            }
        }
        other => panic!("expected ApplyingHandoff, got {other:?}"),
    }
    assert!(outcome
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolCallItem { .. })));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p paigasus-helikon-core --test transition_unit model_response_with_transfer_call_routes_to_applying_handoff`
Expected: FAIL — `TransitionCtx` has no field `handoffs` (and routing not implemented).

- [ ] **Step 3: Extend the `loop_state.rs` types**

In `src/loop_state.rs`:

(a) Add `usage` to the `ApplyingHandoff` variant:

```rust
    /// Handing off to another agent; carries the threaded transcript and the
    /// cumulative usage of all turns completed before the handoff (SMA-324).
    ApplyingHandoff {
        /// Name of the target agent.
        target: String,
        /// Conversation transcript to hand off.
        transcript: Vec<Item>,
        /// Cumulative usage of turns completed before the handoff.
        usage: TokenUsage,
    },
```

(b) Add the `Handoff` variant to `pub enum NextAction` (after `Terminate`):

```rust
    /// Delegate to a handoff target. The driver reads the target, transcript,
    /// and pre-handoff usage from the [`LoopState::ApplyingHandoff`] state.
    Handoff,
```

(c) Add the field to `pub struct TransitionCtx<'a>` (after `output`):

```rust
    /// Synthetic transfer-tool descriptors, one per handoff candidate. Empty
    /// when the agent has no handoffs.
    pub handoffs: &'a [crate::HandoffDef],
```

- [ ] **Step 4: Add the helper functions**

In `src/loop_state.rs`, near the other free helpers (e.g. after `last_assistant_content`), add:

```rust
/// The synthetic `ToolDef` the model sees for one handoff (no arguments — the
/// conversation is the payload).
fn handoff_tool_def(def: &crate::HandoffDef) -> ToolDef {
    ToolDef {
        name: def.tool_name.clone(),
        description: def.description.clone(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    }
}

/// Real tools + synthetic transfer tools, for an unconstrained model turn.
fn turn_tools(ctx: &TransitionCtx<'_>) -> Vec<ToolDef> {
    let mut tools = ctx.tools.to_vec();
    tools.extend(ctx.handoffs.iter().map(handoff_tool_def));
    tools
}

/// Thread the parent transcript for a handoff target: drop the leading
/// `System` and **all** tool calls/results (they reference tools the target
/// does not define), keep user + assistant-text items, and append a transfer
/// note so the target has routing context and the transcript is never empty.
fn thread_handoff_transcript(conversation: &[Item]) -> Vec<Item> {
    let mut out: Vec<Item> = conversation
        .iter()
        .filter(|i| {
            !matches!(
                i,
                Item::System { .. } | Item::ToolCall { .. } | Item::ToolResult { .. }
            )
        })
        .cloned()
        .collect();
    out.push(Item::UserMessage {
        content: vec![ContentPart::Text {
            text: "You are now handling a transferred conversation. \
                   Continue assisting the user."
                .to_owned(),
        }],
    });
    out
}
```

- [ ] **Step 5: Merge handoff tools into the unconstrained model requests**

In `transition`, in the **Start `_ =>`** branch and the **`ExecutingTools` + `ToolResults`** branch, change `tools: ctx.tools.to_vec()` to `tools: turn_tools(ctx)`. Leave the constrained finalizing/repair requests (`tools: Vec::new()`) and the constrained Start sub-branch unchanged.

- [ ] **Step 6: Add the routing logic to the tool-call arm**

In `transition`, the arm
`(LoopState::CallingModel { turn, usage: prior }, TransitionInput::ModelResponse { items, usage, .. }) if items.iter().any(|i| matches!(i, Item::ToolCall { .. })) =>`
gets a handoff check at the **top of its body**, before the existing `ExecutingTools` logic:

```rust
            // Handoff takes precedence over regular tool calls (first wins).
            if let Some(target) = items.iter().find_map(|i| match i {
                Item::ToolCall { name, .. }
                    if ctx.handoffs.iter().any(|h| &h.tool_name == name) =>
                {
                    ctx.handoffs
                        .iter()
                        .find(|h| &h.tool_name == name)
                        .map(|h| h.target.clone())
                }
                _ => None,
            }) {
                let total = accumulate(*prior, usage);
                let mut events: Vec<AgentEvent> = Vec::new();
                for item in &items {
                    match item {
                        Item::AssistantMessage { .. } => {
                            events.push(AgentEvent::MessageOutput { item: item.clone() });
                        }
                        Item::ToolCall { name, .. }
                            if ctx.handoffs.iter().any(|h| &h.tool_name == name) =>
                        {
                            events.push(AgentEvent::ToolCallItem { item: item.clone() });
                        }
                        _ => {}
                    }
                }
                return TransitionOutcome {
                    next_state: LoopState::ApplyingHandoff {
                        target,
                        transcript: thread_handoff_transcript(ctx.conversation),
                        usage: total,
                    },
                    events,
                    next_action: NextAction::Handoff,
                    conversation_appends: Vec::new(),
                };
            }
            // (existing ExecutingTools logic continues unchanged below)
```

- [ ] **Step 7: Remove the `not_implemented("handoff")` arm**

Delete the line `(LoopState::ApplyingHandoff { .. }, _) => not_implemented("handoff"),` from `transition`'s match. (`Compacting` / `NeedsApproval` arms stay.) `ApplyingHandoff` is now only ever produced and consumed by the driver, never fed back into `transition`.

- [ ] **Step 8: Run the pure-routing test**

Run: `cargo test -p paigasus-helikon-core --test transition_unit model_response_with_transfer_call_routes_to_applying_handoff`
Expected: PASS. Then `cargo build -p paigasus-helikon-core` — note it now **fails** in `agent.rs`: the driver's `match next_action` is non-exhaustive (missing `Handoff`) and `TransitionCtx { … }` lacks `handoffs`. Fixed next.

- [ ] **Step 9: Wire the driver — snapshots + collision check + TransitionCtx**

In `src/agent.rs`'s `impl Agent for LlmAgent`'s `run`, alongside the existing snapshots (near `let tools = self.tools.clone();`), add:

```rust
        let handoffs = self.handoffs.clone();
        let max_agent_depth = effective_config.max_agent_depth;
```

Inside the `async_stream::stream! { … }`, right after `yield crate::AgentEvent::RunStarted { … };`, add the def build + collision check:

```rust
            // SMA-324: synthetic transfer tools; fail fast on name collisions.
            let handoff_defs: Vec<crate::HandoffDef> =
                handoffs.iter().map(|h| h.to_def()).collect();
            {
                let real: std::collections::HashSet<&str> =
                    tool_defs.iter().map(|t| t.name.as_str()).collect();
                let mut seen = std::collections::HashSet::new();
                for d in &handoff_defs {
                    if !seen.insert(d.tool_name.as_str()) || real.contains(d.tool_name.as_str())
                    {
                        let err = crate::AgentError::Other(anyhow::anyhow!(
                            "handoff transfer-tool name collision: '{}'",
                            d.tool_name
                        ));
                        let msg = err.to_string();
                        failure.set(err);
                        yield crate::AgentEvent::RunFailed { error: msg };
                        return;
                    }
                }
            }
```

In the loop, add `handoffs: &handoff_defs,` to the `crate::TransitionCtx { … }` construction.

- [ ] **Step 10: Wire the driver — the `NextAction::Handoff` arm**

In the `match next_action { … }`, add this arm (after `Terminate`):

```rust
                    crate::NextAction::Handoff => {
                        let (target, transcript, parent_usage) = match loop_state {
                            crate::LoopState::ApplyingHandoff {
                                target,
                                transcript,
                                usage,
                            } => (target, transcript, usage),
                            // NextAction::Handoff is only produced alongside
                            // ApplyingHandoff.
                            _ => return,
                        };

                        // Depth guard (shared with AgentAsTool).
                        let child = ctx.handoff_child();
                        if child.agent_depth() > max_agent_depth {
                            let err = crate::AgentError::MaxAgentDepthExceeded {
                                depth: child.agent_depth(),
                                max: max_agent_depth,
                            };
                            let msg = err.to_string();
                            run_span.record("otel.status_code", "ERROR");
                            failure.set(err);
                            yield crate::AgentEvent::RunFailed { error: msg };
                            return;
                        }

                        // Resolve the target (clone the Arc so no borrow crosses .await).
                        let Some(target_agent) = handoffs
                            .iter()
                            .find(|h| h.agent().name() == target)
                            .map(|h| std::sync::Arc::clone(h.agent()))
                        else {
                            let err = crate::AgentError::Other(anyhow::anyhow!(
                                "unknown handoff target: {target}"
                            ));
                            let msg = err.to_string();
                            run_span.record("otel.status_code", "ERROR");
                            failure.set(err);
                            yield crate::AgentEvent::RunFailed { error: msg };
                            return;
                        };

                        yield crate::AgentEvent::HandoffItem {
                            from: agent_name.clone(),
                            to: target.clone(),
                        };
                        yield crate::AgentEvent::AgentUpdated {
                            agent: target.clone(),
                        };

                        let input = crate::AgentInput { messages: transcript };
                        let mut sub = match target_agent.run(child, input).await {
                            Ok(s) => s,
                            Err(e) => {
                                let msg = e.to_string();
                                run_span.record("otel.status_code", "ERROR");
                                failure.set(e);
                                yield crate::AgentEvent::RunFailed { error: msg };
                                return;
                            }
                        };
                        while let Some(ev) = sub.next().await {
                            match ev {
                                // One RunStarted per logical run (we already
                                // signalled the switch via AgentUpdated).
                                crate::AgentEvent::RunStarted { .. } => {}
                                // Sum the parent's pre-handoff usage into the
                                // chain total (SMA-402 "who pays" → across chain).
                                crate::AgentEvent::RunCompleted { usage } => {
                                    let mut total = parent_usage;
                                    total.add(usage);
                                    yield crate::AgentEvent::RunCompleted { usage: total };
                                }
                                other => yield other,
                            }
                        }
                        return;
                    }
```

(`futures_util::stream::StreamExt` is already imported in `run`, so `sub.next()` works.)

- [ ] **Step 11: Build + run the unit suite**

Run: `cargo test -p paigasus-helikon-core --test transition_unit`
Expected: PASS (all, including the new routing test and the updated constructions).

- [ ] **Step 12: Write the end-to-end triage test**

Create `crates/paigasus-helikon-core/tests/handoff.rs`:

```rust
//! SMA-324 — end-to-end handoff: 3-agent finance triage, collisions, depth guard.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::MockModel;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, FinishReason, HookRegistry, LlmAgent,
    MemorySession, ModelEvent, RunContext, RunResultStreaming, Session, TracerHandle,
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

// A model turn that emits one tool call (the transfer), then finishes.
fn transfer_turn(tool: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            call_id: "c1".to_owned(),
            name: Some(tool.to_owned()),
            args_delta: "{}".to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]
}

// A model turn that emits final text.
fn text_turn(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta {
            text: text.to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

#[tokio::test]
async fn triage_routes_to_budgeting_not_investing() {
    let budgeting = LlmAgent::builder::<()>()
        .name("budgeting specialist")
        .description("Handles budgeting questions.")
        .model(MockModel::with_scripts(vec![text_turn("Cut dining by $60.")]))
        .build();
    // Investing must NOT run → give it no scripts; running it would error.
    let investing = LlmAgent::builder::<()>()
        .name("investing specialist")
        .description("Handles investing questions.")
        .model(MockModel::with_scripts(vec![]))
        .build();

    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .model(MockModel::with_scripts(vec![transfer_turn(
            "transfer_to_budgeting_specialist",
        )]))
        .handoff(budgeting)
        .handoff(investing)
        .build();

    let stream = triage
        .run(ctx(), AgentInput::from_user_text("How do I budget?"))
        .await
        .expect("run starts");
    let result = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect("run completes");

    assert_eq!(result.final_output, "Cut dining by $60.");

    let starts = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::RunStarted { .. }))
        .count();
    assert_eq!(starts, 1, "exactly one RunStarted across the chain");

    assert!(result.events.iter().any(|e| matches!(
        e,
        AgentEvent::HandoffItem { from, to }
            if from == "triage" && to == "budgeting specialist"
    )));
    assert!(result.events.iter().any(|e| matches!(
        e,
        AgentEvent::AgentUpdated { agent } if agent == "budgeting specialist"
    )));
}

#[tokio::test]
async fn slug_collision_between_handoffs_fails_fast() {
    // Two distinct names that slug to the same transfer tool.
    let a = LlmAgent::builder::<()>()
        .name("Budgeting Specialist")
        .model(MockModel::with_scripts(vec![]))
        .build();
    let b = LlmAgent::builder::<()>()
        .name("budgeting specialist")
        .model(MockModel::with_scripts(vec![]))
        .build();
    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .model(MockModel::with_scripts(vec![text_turn("hi")]))
        .handoff(a)
        .handoff(b)
        .build();

    let stream = triage
        .run(ctx(), AgentInput::from_user_text("x"))
        .await
        .expect("run starts");
    let err = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect_err("collision fails the run");
    assert!(err.to_string().contains("collision"), "got: {err}");
}

#[tokio::test]
async fn handoff_cycle_hits_depth_guard() {
    // A hands off to B; B hands back to A; bounded by max_agent_depth.
    // Build B first (hands to "a"), then A (hands to B). A transfers to B,
    // B transfers back to A by name — but A is not in B's handoffs, so model
    // forwarding stops; instead test the cap directly with a low max.
    let b = LlmAgent::builder::<()>()
        .name("b")
        .model(MockModel::with_scripts(vec![
            transfer_turn("transfer_to_b"), // B re-emits a transfer to itself's name? see note
        ]))
        .build();
    let a = LlmAgent::builder::<()>()
        .name("a")
        .model(MockModel::with_scripts(vec![transfer_turn("transfer_to_b")]))
        .handoff(b)
        .max_turns(8)
        .build();

    // Run with max_agent_depth = 1 so a single hop is allowed but a second is not.
    let run_ctx = ctx();
    let stream = a
        .run(
            run_ctx,
            AgentInput::from_user_text("loop"),
        )
        .await
        .expect("run starts");
    // With a depth cap installed via RunConfig the second hop fails. Use the
    // RunConfig path: rebuild via a config-bearing context.
    let _ = stream; // placeholder; replaced below
}
```

> **Note for the implementer:** the cycle test needs a `RunConfig` with a low `max_agent_depth` installed on the context. Replace the body of `handoff_cycle_hits_depth_guard` with the version in Step 13 once the config-on-context path is confirmed; the sketch above documents intent.

- [ ] **Step 13: Finalize the depth-guard test**

Replace `handoff_cycle_hits_depth_guard` with a deterministic two-agent mutual handoff bounded by `max_agent_depth = 1`. The cap is read by the driver from `effective_config`, which comes from `ctx.run_config()` when set:

```rust
#[tokio::test]
async fn handoff_cycle_hits_depth_guard() {
    use paigasus_helikon_core::RunConfig;

    // `a` transfers to `b`; `b` transfers back to `a`. Each agent lists the
    // other as a handoff so the model's transfer call always resolves.
    let b = LlmAgent::builder::<()>()
        .name("b")
        .model(MockModel::with_scripts(vec![transfer_turn("transfer_to_a")]))
        .build();
    let a_for_b = LlmAgent::builder::<()>()
        .name("a")
        .model(MockModel::with_scripts(vec![transfer_turn("transfer_to_b")]))
        .build();
    // b lists a as a handoff
    let b = LlmAgent::builder::<()>()
        .name("b")
        .description("b")
        .model(MockModel::with_scripts(vec![transfer_turn("transfer_to_a")]))
        .handoff(a_for_b)
        .build();
    let a = LlmAgent::builder::<()>()
        .name("a")
        .description("a")
        .model(MockModel::with_scripts(vec![transfer_turn("transfer_to_b")]))
        .handoff(b)
        .build();

    // Install a RunConfig with max_agent_depth = 1: a→b is depth 1 (allowed),
    // b→a would be depth 2 (rejected).
    let run_ctx = ctx().with_run_config(RunConfig::new().with_max_agent_depth(1));
    let stream = a
        .run(run_ctx, AgentInput::from_user_text("loop"))
        .await
        .expect("run starts");
    let err = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect_err("depth guard trips");
    assert!(
        err.to_string().contains("nesting depth"),
        "expected depth error, got: {err}"
    );
}
```

- [ ] **Step 14: Run the handoff integration suite**

Run: `cargo test -p paigasus-helikon-core --test handoff`
Expected: PASS (all three: routing, collision, depth).

- [ ] **Step 15: Run the whole core test suite (regression)**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS — the existing loop/usage/structured-output tests are unaffected (handoff routing only fires with non-empty handoffs).

- [ ] **Step 16: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/tests/transition_unit.rs crates/paigasus-helikon-core/tests/handoff.rs
git commit -m "feat(core): SMA-324 drive handoff via nested delegation"
```

---

## Task 7: `AgentAsTool<Ctx>` (new `agent_as_tool.rs`)

**Files:**
- Create: `crates/paigasus-helikon-core/src/agent_as_tool.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Test: `crates/paigasus-helikon-core/tests/agent_as_tool.rs` (new)

- [ ] **Step 1: Create `src/agent_as_tool.rs`**

```rust
//! [`AgentAsTool`] — expose any [`crate::Agent`] as a [`crate::Tool`].
//!
//! The parent agent calls the wrapped agent like any tool, gets its
//! `final_output` back as a [`crate::ToolOutput`], and keeps reasoning. The
//! sub-run is **isolated**: a fresh in-memory session and empty hooks, so its
//! internal turns never touch the parent's session log.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::{
    Agent, AgentInput, HookRegistry, MemorySession, RunContext, RunError, RunResultStreaming,
    Tool, ToolContext, ToolError, ToolOutput,
};

/// Adapter exposing an [`Agent`] as a [`Tool`].
pub struct AgentAsTool<Ctx> {
    agent: Arc<dyn Agent<Ctx>>,
    name: String,
    description: String,
    schema: Value,
}

impl<Ctx> AgentAsTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Wrap an owned agent. The tool name and description default to the
    /// agent's own; the argument schema is a single string field `input`.
    pub fn new(agent: impl Agent<Ctx> + 'static) -> Self {
        Self::shared(Arc::new(agent))
    }

    /// Wrap a pre-wrapped agent.
    pub fn shared(agent: Arc<dyn Agent<Ctx>>) -> Self {
        let name = agent.name().to_owned();
        let description = agent.description().to_owned();
        Self {
            agent,
            name,
            description,
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "The request to pass to the wrapped agent."
                    }
                },
                "required": ["input"],
                "additionalProperties": false
            }),
        }
    }

    /// Override the tool name (default: the agent's name).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Override the tool description (default: the agent's description).
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for AgentAsTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> &Value {
        &self.schema
    }

    async fn invoke(&self, ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let input_text = args
            .get("input")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs {
                schema_errors: vec!["expected a string field `input`".to_owned()],
            })?;

        // Bound nesting with the same counter the handoff path uses.
        let depth = ctx.agent_depth();
        let max = ctx.max_agent_depth();
        if depth + 1 > max {
            return Err(ToolError::Other(anyhow::Error::from(
                crate::AgentError::MaxAgentDepthExceeded {
                    depth: depth + 1,
                    max,
                },
            )));
        }

        // Isolated sub-context: fresh session + empty hooks; inherit user_ctx,
        // tracer, and the child cancel token; stamp the incremented depth.
        let sub_ctx = RunContext::new(
            Arc::clone(ctx.user_ctx()),
            Arc::new(MemorySession::new()),
            HookRegistry::new(),
            ctx.tracer().clone(),
            ctx.cancel().clone(),
        )
        .with_agent_depth(depth + 1);

        let failure = sub_ctx.failure_handle();
        let stream = self
            .agent
            .run(sub_ctx, AgentInput::from_user_text(input_text))
            .await
            .map_err(|e| ToolError::Other(anyhow::Error::from(e)))?;

        let result = RunResultStreaming::with_failure(stream, failure)
            .collect()
            .await
            .map_err(|e| match e {
                RunError::Agent(a) => ToolError::Other(anyhow::Error::from(a)),
                other => ToolError::Other(anyhow::Error::from(other)),
            })?;

        Ok(ToolOutput::new(Value::String(result.final_output)))
    }
}
```

- [ ] **Step 2: Wire the module in `lib.rs`**

In `src/lib.rs`, add `pub mod agent_as_tool;` and `pub use agent_as_tool::*;`.

- [ ] **Step 3: Build**

Run: `cargo build -p paigasus-helikon-core`
Expected: compiles clean.

- [ ] **Step 4: Write the round-trip + isolation + depth tests**

Create `crates/paigasus-helikon-core/tests/agent_as_tool.rs`:

```rust
//! SMA-324 — AgentAsTool: round-trip, isolation, depth guard.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use common::MockModel;
use paigasus_helikon_core::{
    Agent, AgentAsTool, AgentInput, CancellationToken, FinishReason, HookRegistry, LlmAgent,
    MemorySession, ModelEvent, RunContext, RunResultStreaming, Session, ToolContext, TracerHandle,
};

fn text_turn(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta {
            text: text.to_owned(),
        },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

#[tokio::test]
async fn agent_as_tool_round_trips_final_output() {
    let sub = LlmAgent::builder::<()>()
        .name("calculator")
        .description("Answers arithmetic.")
        .model(MockModel::with_scripts(vec![text_turn("42")]))
        .build();
    let tool = AgentAsTool::new(sub);

    let tc: ToolContext<()> = ToolContext::new(
        Arc::new(()),
        TracerHandle::default(),
        CancellationToken::new(),
        0,
        8,
    );
    let out = tool
        .invoke(&tc, serde_json::json!({ "input": "what is 6*7?" }))
        .await
        .expect("invoke ok");
    assert_eq!(out.content, serde_json::Value::String("42".to_owned()));
}

#[tokio::test]
async fn agent_as_tool_isolates_session() {
    // The parent's session must contain no sub-agent turns. We assert via the
    // tool path: the sub-run uses a fresh MemorySession, so the parent session
    // we pass in stays empty.
    let parent_session = Arc::new(MemorySession::new());
    let sub = LlmAgent::builder::<()>()
        .name("sub")
        .model(MockModel::with_scripts(vec![text_turn("done")]))
        .build();
    let tool = AgentAsTool::new(sub);

    let run_ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        parent_session.clone() as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );
    let tc = run_ctx.to_tool_context();
    let _ = tool
        .invoke(&tc, serde_json::json!({ "input": "go" }))
        .await
        .expect("invoke ok");

    let events = parent_session.events(None).await.expect("events");
    assert!(events.is_empty(), "sub-agent turns must not touch parent session");
}

#[tokio::test]
async fn agent_as_tool_depth_guard_trips_on_cycle() {
    // Depth already at the max → invoke must refuse without running the agent.
    let sub = LlmAgent::builder::<()>()
        .name("sub")
        .model(MockModel::with_scripts(vec![])) // would error if it ran
        .build();
    let tool = AgentAsTool::new(sub);

    // agent_depth == max_agent_depth → depth+1 > max → reject.
    let tc: ToolContext<()> = ToolContext::new(
        Arc::new(()),
        TracerHandle::default(),
        CancellationToken::new(),
        8,
        8,
    );
    let err = tool
        .invoke(&tc, serde_json::json!({ "input": "go" }))
        .await
        .expect_err("depth guard refuses");
    assert!(err.to_string().contains("nesting depth"), "got: {err}");
}
```

- [ ] **Step 5: Run the suite**

Run: `cargo test -p paigasus-helikon-core --test agent_as_tool`
Expected: PASS (all three).

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent_as_tool.rs crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/tests/agent_as_tool.rs
git commit -m "feat(core): SMA-324 add AgentAsTool adapter"
```

---

## Task 8: Facade example `multi_agent_triage.rs`

**Files:**
- Create: `crates/paigasus-helikon/examples/multi_agent_triage.rs`
- Modify: `crates/paigasus-helikon/Cargo.toml` (add an `[[example]]` entry)

- [ ] **Step 1: Create the example**

```rust
//! Multi-agent handoff example (SMA-324): a personal-finance triage agent that
//! routes the conversation to a budgeting specialist or an investing
//! specialist via `Handoff`.
//!
//! ```text
//! OPENAI_API_KEY=sk-… cargo run -p paigasus-helikon \
//!     --features openai --example multi_agent_triage
//! ```

use std::sync::Arc;

use paigasus_helikon::core::{
    Agent, AgentInput, CancellationToken, Handoff, HookRegistry, LlmAgent, MemorySession,
    RunContext, RunResultStreaming, TracerHandle,
};
use paigasus_helikon::openai::OpenAiModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let budgeting = LlmAgent::builder::<()>()
        .name("budgeting specialist")
        .description("Answers questions about monthly budgets and cutting spending.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("You are a budgeting specialist. Give concrete, friendly advice.")
        .build();

    let investing = LlmAgent::builder::<()>()
        .name("investing specialist")
        .description("Answers questions about investing, portfolios, and retirement.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("You are an investing specialist. Give concrete, prudent advice.")
        .build();

    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions(
            "Classify the user's personal-finance question and transfer to the right \
             specialist. Do not answer yourself — always hand off.",
        )
        .handoffs([Handoff::to(budgeting), Handoff::to(investing)])
        .build();

    let ctx: RunContext<()> = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::<()>::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    );

    let input = AgentInput::from_user_text("How should I start investing $5,000?");

    // With handoffs the terminal agent is dynamic, so consume as a string
    // (see the spec's post-handoff output-type contract).
    let stream = triage.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream).collect().await?;

    println!("{}", result.final_output);
    Ok(())
}
```

- [ ] **Step 2: Register the example**

In `crates/paigasus-helikon/Cargo.toml`, add alongside the other `[[example]]` entries:

```toml
[[example]]
name = "multi_agent_triage"
required-features = ["openai"]
```

- [ ] **Step 3: Compile the example (no API call)**

Run: `cargo build -p paigasus-helikon --features openai --example multi_agent_triage`
Expected: compiles clean.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon/examples/multi_agent_triage.rs crates/paigasus-helikon/Cargo.toml
git commit -m "docs(facade): SMA-324 add multi-agent triage example"
```

---

## Task 9: Full CI gate + release-mechanics handoff

No code; verify every CI gate locally and confirm the breaking-change signalling.

**Files:** none (verification + PR metadata).

- [ ] **Step 1: Format**

Run: `cargo fmt --all -- --check`
Expected: no diff. (If it complains, run `cargo fmt --all` and amend the relevant commit.)

- [ ] **Step 2: Clippy (the gate that catches the most)**

Run: `cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: no warnings. Common fixes: an unused `import`, a `needless_borrow`, or a `collapsible_if` in the new code.

- [ ] **Step 3: Full workspace test**

Run: `cargo test --workspace --all-features`
Expected: PASS, including `tests/handoff.rs`, `tests/agent_as_tool.rs`, and the updated `transition_unit`.

- [ ] **Step 4: Docs (with `-D warnings`)**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
Expected: clean. Every new public item (`Handoff`, `Handoff::{to,shared,agent,to_def}`, `HandoffDef` + fields, `AgentAsTool` + methods, `RunConfig::max_agent_depth`/`with_max_agent_depth`, `RunContext::{agent_depth,with_agent_depth,handoff_child}`, `ToolContext::{agent_depth,max_agent_depth}`, `AgentError::MaxAgentDepthExceeded`, `NextAction::Handoff`, `LoopState::ApplyingHandoff.usage`, `TransitionCtx.handoffs`) must carry a `///` — fix any `missing_docs` failures.

- [ ] **Step 5: Doc coverage**

Run: `DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh`
Expected: at/above threshold. (Requires `rustup toolchain install nightly-2026-05-01`.)

- [ ] **Step 6: Push the branch**

```bash
git push -u origin feature/sma-324-multi-agent-handoff-agentastool
```

(The local `pre-push` hook re-runs fmt + clippy + `convco`. If commits signed via the 1Password SSH key fail with "failed to fill whole buffer", unlock the vault and retry — do not bypass signing.)

- [ ] **Step 7: Open the PR with a breaking-marked title**

The squashed-commit title gates the release bump and must be a breaking-marked Conventional Commit with a lowercase subject after the issue prefix:

```bash
gh pr create \
  --title "feat(core)!: SMA-324 add multi-agent handoff + AgentAsTool" \
  --body "$(cat <<'EOF'
Implements SMA-324 per docs/superpowers/specs/2026-06-03-sma-324-multi-agent-handoff-agentastool-design.md.

- `Handoff<Ctx>` + injected `transfer_to_*` tools; nested-delegation driver.
- `AgentAsTool<Ctx>` adapter with isolated sub-context.
- Unified `agent_depth`/`max_agent_depth` recursion bound.

Breaking (core public API): `LlmAgent.handoffs` retyped to `Vec<Handoff>`;
`LoopState::ApplyingHandoff` gains `usage`; `TransitionCtx` gains `handoffs`;
`ToolContext::new` gains two depth params. release-plz: core 0.3.0 → 0.4.0.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 8: Confirm the release plan**

After CI is green, confirm release-plz's release PR proposes `paigasus-helikon-core 0.4.0` (breaking → minor on `0.x`) and a facade cascade bump. Do **not** hand-edit versions or `CHANGELOG`s — release-plz owns them.

---

## Self-Review

**Spec coverage** (each spec section → task):
- §3.1 `Handoff`/`HandoffDef`/slug + collision → Task 4 (types/slug) + Task 6 Step 9 (collision, both classes).
- §3.2 `LlmAgent.handoffs` retype + builder → Task 5.
- §3.3 routing branch, tool-merge, precedence, output_type/handoffs note → Task 6 Steps 3–7 (provider/output notes are doc-only, captured in code comments + the spec).
- §3.4 nested delegation, transcript threading, RunStarted suppression, usage chain-sum, failure propagation → Task 6 Steps 4, 10.
- §3.5 `AgentAsTool` (schema, isolated ctx, depth guard, `collect` reuse, round-trip) → Task 7.
- §3.6 unified `agent_depth`/`max_agent_depth`/`MaxAgentDepthExceeded`/`handoff_child`/`with_agent_depth`/`ToolContext` scalars → Tasks 1, 2, 3 (+ used in 6, 7).
- §4 module surface + lib exports → Tasks 4, 7 Step 2.
- §5 breaking change / `feat(core)!:` → Task 9 Step 7.
- §7 tests (transition_unit, handoff.rs, agent_as_tool.rs) + facade example → Tasks 6, 7, 8.

**Placeholder scan:** Task 6 Step 12 contains a deliberately-sketched test body with a `> Note` that Step 13 replaces with the final version — this is intentional (it shows the intent, then the working code) and the working test is complete in Step 13. No other placeholders; every code step shows full code.

**Type consistency:** `Handoff::to_def() -> HandoffDef`; `HandoffDef { tool_name, target, description }` used identically in `handoff_tool_def`, the routing `find_map`, the driver collision check, and the unit test. `agent_depth()` / `max_agent_depth()` / `with_agent_depth()` / `handoff_child()` names match across `RunContext`, `ToolContext`, the driver, and `AgentAsTool`. `MaxAgentDepthExceeded { depth, max }` constructed identically in the driver and in `AgentAsTool`. `NextAction::Handoff` (unit) is produced in `transition` and matched in the driver. `ApplyingHandoff { target, transcript, usage }` constructed in `transition`, destructured in the driver.
