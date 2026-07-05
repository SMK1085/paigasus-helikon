# SMA-333 — Evals + CLI + Swarm/Graph Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement SMA-333 per the approved spec (`docs/superpowers/specs/2026-07-05-sma-333-evals-cli-swarm-graph-design.md`): `SwarmAgent`/`GraphAgent` in core, the `paigasus-helikon-evals` crate (ascending to 0.1.0), and the `helikon` CLI (`repl`, `eval run`, `mcp serve`, ascending to a published 0.1.0 binary crate).

**Architecture:** Swarm = full-mesh `Handoff` wiring through weak member slots + a hop-counting stream wrapper; Graph = dynamic Kahn wavefront over `SelectAll`; evals = `Evaluator` trait + 4 built-ins + promoted `MockModel` with serde mirror types + feature-gated SQLite/Parquet trace sinks; CLI = `LlmAgent<(), CliModel>` built from a TOML/Rhai sidecar with notify-based hot reload.

**Tech Stack:** Rust 2021 (MSRV 1.94), async-stream/futures cooperative streams, sqlx (SQLite), arrow+parquet (opt-in), jsonschema, clap 4 derive, rhai (`sync`+`serde`), notify + notify-debouncer-mini, toml, rmcp via `paigasus-helikon-mcp`.

## Global Constraints

- **Registry-verify rule (spec §2):** `paigasus-helikon-evals` and `paigasus-helikon-cli` must ONLY use core API that exists in published `paigasus-helikon-core 0.5.12` (everything cited in this plan qualifies). They must NOT reference `SwarmAgent`, `GraphAgent`, or `AgentError::MaxHandoffsExceeded` (all new in this PR). Only core's own tests and facade examples may use those.
- MSRV `1.94` — new deps must compile on it (CI matrix tests 1.94; verify locally when picking versions).
- `missing_docs = "warn"` + `RUSTDOCFLAGS="-D warnings"`: **every** `pub` item in core and evals needs a `///` doc comment. The CLI crate has `missing_docs = "allow"` — still write doc comments on its lib items where cheap.
- No `///`-doc intra-doc links from pub items to private items (fails the docs gate) — use prose.
- Workspace inheritance is mandatory: third-party versions ONLY in root `[workspace.dependencies]`; member crates use `dep.workspace = true`.
- Commit format: `<type>(<scope>): SMA-333 <lowercase subject>`. Allowed scopes used here: `core`, `evals`, `cli`, `facade`, `workspace`, `specs`, `docs`, `readme`, `release`, `deps`. Run `cargo fmt --all` before every commit (pre-commit hook is a no-op; pre-push runs fmt+clippy+convco).
- Never run `git checkout`, `git switch`, `git reset`, or any HEAD/branch-moving git command. Work only on the current branch `feature/sma-333-paigasus-helikon-evals-paigasus-helikon-cli-swarmgraph`. Run cargo/test commands in the foreground and do not end a task with builds still running.
- The exact CI gates (run before declaring any task complete that touches Rust): `cargo fmt --all -- --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, `cargo test --workspace --all-features` (or the task's narrower test command during TDD loops, with the full gate at group ends).
- Domain for examples/fixtures: **personal finance** (triage/budgeting/investing), never medical.

---

## Task Group A — Core: SwarmAgent + GraphAgent

### Task 1: `AgentError::MaxHandoffsExceeded` + shared helper visibility

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (the `AgentError` enum, near line 1240)
- Modify: `crates/paigasus-helikon-core/src/workflow.rs:25` (`assistant_text` visibility) and `workflow.rs:42-77` (`max_depth`, `workflow_run_span` visibility)
- Test: `crates/paigasus-helikon-core/tests/swarm.rs` (created here with the first failing test)

**Interfaces:**
- Produces: `AgentError::MaxHandoffsExceeded { limit: u32 }` (display: `max handoffs (N) exceeded`); `pub(crate) fn assistant_text(&Item) -> Option<String>`, `pub(crate) fn max_depth(Option<&RunConfig>) -> u32`, `pub(crate) fn workflow_run_span(&str, &TracerHandle) -> tracing::Span` — all reused by Tasks 2 and 4.

- [ ] **Step 1: Write the failing test**

Create `crates/paigasus-helikon-core/tests/swarm.rs`:

```rust
//! SwarmAgent integration tests (SMA-333).

#[path = "common/mod.rs"]
mod common;

use paigasus_helikon_core::AgentError;

#[test]
fn max_handoffs_error_displays_limit() {
    let err = AgentError::MaxHandoffsExceeded { limit: 3 };
    assert_eq!(err.to_string(), "max handoffs (3) exceeded");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-core --test swarm`
Expected: COMPILE FAIL — `no variant named MaxHandoffsExceeded`.

- [ ] **Step 3: Implement**

In `agent.rs`, inside `pub enum AgentError`, after the `MaxAgentDepthExceeded` variant, add:

```rust
    /// A [`crate::SwarmAgent`] exceeded its configured handoff budget
    /// before any member produced a final output.
    #[error("max handoffs ({limit}) exceeded")]
    MaxHandoffsExceeded {
        /// The configured budget that was exceeded.
        limit: u32,
    },
```

In `workflow.rs`, change `fn assistant_text(`, `fn max_depth(`, and `fn workflow_run_span(` to `pub(crate) fn …` (three one-word edits; no body changes).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-core --test swarm` → PASS. Then `cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings` — expect a `dead_code`-style warning is NOT emitted (the `pub(crate)` fns are still used by workflow.rs itself; if clippy flags unused `pub(crate)`, that resolves in Tasks 2/4 — suppress nothing, just proceed if the only failure is `unused` on the two helpers, and fold Step 5's commit into Task 2's first commit instead).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core
git commit -m "feat(core): SMA-333 add MaxHandoffsExceeded error and share workflow helpers"
```

### Task 2: `SwarmAgent<Ctx>`

**Files:**
- Create: `crates/paigasus-helikon-core/src/swarm.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs` (module + re-exports)
- Test: `crates/paigasus-helikon-core/tests/swarm.rs`

**Interfaces:**
- Consumes: `Agent<Ctx>`, `LlmAgent<Ctx, M, T>` (pub field `handoffs: Vec<Handoff<Ctx>>`), `Handoff::shared`, `AgentEvent::{RunStarted, HandoffItem, RunCompleted, RunFailed, AgentUpdated}`, `ctx.subagent_child()/failure_handle()/hooks()/tracer()/run_config()/agent_depth()`, `workflow_run_span`, `max_depth`, `AgentError::{MaxAgentDepthExceeded, MaxHandoffsExceeded}`.
- Produces: `pub struct SwarmAgent<Ctx>`, `SwarmAgent::builder() -> SwarmAgentBuilder<Ctx>`, builder methods `.name(impl Into<String>)`, `.description(impl Into<String>)`, `.member(LlmAgent<Ctx, M, T>)`, `.entry(impl Into<String>)`, `.max_handoffs(u32)`, `.build() -> Result<SwarmAgent<Ctx>, SwarmBuildError>`; `pub enum SwarmBuildError { Empty, MissingName, DuplicateMember(String), UnknownEntry(String) }`; `impl Agent<Ctx> for SwarmAgent<Ctx>`.

- [ ] **Step 1: Write failing builder-validation tests** (append to `tests/swarm.rs`)

```rust
use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, LlmAgent, ModelEvent, RunContext, RunConfig,
    RunResultStreaming, SwarmAgent, SwarmBuildError, FinishReason,
};
use futures_util::StreamExt as _;

fn member(name: &str, scripts: Vec<Vec<ModelEvent>>) -> LlmAgent<(), common::MockModel> {
    LlmAgent::builder::<()>()
        .name(name)
        .description(format!("swarm member {name}"))
        .shared_model(common::MockModel::with_scripts(scripts))
        .instructions("test")
        .build()
}

fn text_final(text: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta { text: text.to_owned() },
        ModelEvent::Finish { reason: FinishReason::Stop },
    ]
}

/// A script turn that calls the transfer tool for `target` (slugged).
fn transfer_turn(target_slug: &str) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCallDelta {
            call_id: "call-1".to_owned(),
            name: Some(format!("transfer_to_{target_slug}")),
            args_delta: "{}".to_owned(),
        },
        ModelEvent::Finish { reason: FinishReason::ToolCalls },
    ]
}

#[test]
fn swarm_build_rejects_empty() {
    let err = SwarmAgent::<()>::builder().name("s").build().unwrap_err();
    assert!(matches!(err, SwarmBuildError::Empty));
}

#[test]
fn swarm_build_rejects_duplicate_member() {
    let err = SwarmAgent::builder()
        .name("s")
        .member(member("a", vec![]))
        .member(member("a", vec![]))
        .build()
        .unwrap_err();
    assert!(matches!(err, SwarmBuildError::DuplicateMember(n) if n == "a"));
}

#[test]
fn swarm_build_rejects_unknown_entry() {
    let err = SwarmAgent::builder()
        .name("s")
        .member(member("a", vec![]))
        .entry("nope")
        .build()
        .unwrap_err();
    assert!(matches!(err, SwarmBuildError::UnknownEntry(n) if n == "nope"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p paigasus-helikon-core --test swarm` → COMPILE FAIL (`SwarmAgent` unresolved).

- [ ] **Step 3: Implement `swarm.rs`**

Create `crates/paigasus-helikon-core/src/swarm.rs` (complete file; every pub item documented):

```rust
//! `SwarmAgent` — a pool of `LlmAgent`s with full-mesh handoff tools
//! auto-injected; the first member to produce a final output instead of
//! handing off wins (SMA-333, ADR-11).

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use tracing::Instrument as _;

use crate::workflow::{max_depth, workflow_run_span};
use crate::{
    Agent, AgentError, AgentEvent, AgentInput, Handoff, LlmAgent, RunContext,
};

/// Errors from [`SwarmAgentBuilder::build`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SwarmBuildError {
    /// The swarm has no members.
    #[error("swarm has no members")]
    Empty,
    /// `.name(…)` was never called.
    #[error("swarm has no name")]
    MissingName,
    /// Two members share a name (handoff tool names would collide).
    #[error("duplicate swarm member name: {0}")]
    DuplicateMember(String),
    /// `.entry(…)` names an unknown member.
    #[error("unknown swarm entry member: {0}")]
    UnknownEntry(String),
}

/// Adapter standing in for a member inside sibling handoffs. Holds a
/// weak reference so member↔member wiring cannot form strong `Arc`
/// cycles; the swarm (and each returned run stream) hold the strong ones.
struct MemberSlot<Ctx> {
    name: String,
    description: String,
    target: OnceLock<Weak<dyn Agent<Ctx>>>,
}

#[async_trait]
impl<Ctx> Agent<Ctx> for MemberSlot<Ctx>
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
        let target = self
            .target
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| {
                AgentError::Other(anyhow::anyhow!(
                    "swarm member '{}' is no longer alive",
                    self.name
                ))
            })?;
        // Forward ctx unchanged: the handoff machinery already derived
        // the child context, so the slot adds no depth level.
        target.run(ctx, input).await
    }
}

type MemberInjector<Ctx> =
    Box<dyn FnOnce(Vec<Handoff<Ctx>>) -> Arc<dyn Agent<Ctx>> + Send>;

/// Builder for [`SwarmAgent`]. Members are added pre-wired; `build()`
/// injects the full-mesh handoffs.
pub struct SwarmAgentBuilder<Ctx> {
    name: Option<String>,
    description: String,
    members: Vec<(String, String, MemberInjector<Ctx>)>,
    entry: Option<String>,
    max_handoffs: Option<u32>,
}

impl<Ctx> SwarmAgentBuilder<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Set the swarm's agent name (required).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the swarm's description.
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = d.into();
        self
    }

    /// Add a member. Only `LlmAgent`s can be members — they are the only
    /// agents that can call the injected `transfer_to_<member>` tools.
    /// Pre-existing handoffs on the member are preserved (appended to).
    pub fn member<M, T>(mut self, agent: LlmAgent<Ctx, M, T>) -> Self
    where
        LlmAgent<Ctx, M, T>: Agent<Ctx> + 'static,
        M: Send + Sync + 'static,
        T: Send + Sync + 'static,
    {
        let name = agent.name.clone();
        let description = agent.description.clone();
        self.members.push((
            name,
            description,
            Box::new(move |handoffs| {
                let mut agent = agent;
                agent.handoffs.extend(handoffs);
                Arc::new(agent) as Arc<dyn Agent<Ctx>>
            }),
        ));
        self
    }

    /// Choose the member that receives the initial input. Defaults to
    /// the first member added.
    pub fn entry(mut self, name: impl Into<String>) -> Self {
        self.entry = Some(name.into());
        self
    }

    /// Bound the number of handoffs before the swarm fails with
    /// `AgentError::MaxHandoffsExceeded`. Unset: only
    /// `RunConfig::max_agent_depth` bounds the chain.
    pub fn max_handoffs(mut self, limit: u32) -> Self {
        self.max_handoffs = Some(limit);
        self
    }

    /// Validate and wire the swarm.
    pub fn build(self) -> Result<SwarmAgent<Ctx>, SwarmBuildError> {
        let name = self.name.ok_or(SwarmBuildError::MissingName)?;
        if self.members.is_empty() {
            return Err(SwarmBuildError::Empty);
        }
        let mut seen = std::collections::HashSet::new();
        for (member_name, _, _) in &self.members {
            if !seen.insert(member_name.clone()) {
                return Err(SwarmBuildError::DuplicateMember(member_name.clone()));
            }
        }
        let entry_idx = match &self.entry {
            None => 0,
            Some(e) => self
                .members
                .iter()
                .position(|(n, _, _)| n == e)
                .ok_or_else(|| SwarmBuildError::UnknownEntry(e.clone()))?,
        };

        // 1. One weak slot per member (name/description copied now).
        let slots: Vec<Arc<MemberSlot<Ctx>>> = self
            .members
            .iter()
            .map(|(n, d, _)| {
                Arc::new(MemberSlot {
                    name: n.clone(),
                    description: d.clone(),
                    target: OnceLock::new(),
                })
            })
            .collect();

        // 2. Wire each member with handoffs to every OTHER member's slot.
        let mut members: Vec<Arc<dyn Agent<Ctx>>> = Vec::with_capacity(self.members.len());
        for (i, (_, _, injector)) in self.members.into_iter().enumerate() {
            let handoffs: Vec<Handoff<Ctx>> = slots
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, slot)| Handoff::shared(Arc::clone(slot) as Arc<dyn Agent<Ctx>>))
                .collect();
            members.push(injector(handoffs));
        }

        // 3. Point each slot at its finished member (weak).
        for (slot, member) in slots.iter().zip(&members) {
            let _ = slot.target.set(Arc::downgrade(member));
        }

        Ok(SwarmAgent {
            name,
            description: self.description,
            members,
            entry_idx,
            max_handoffs: self.max_handoffs,
        })
    }
}

/// A pool of `LlmAgent`s with auto-injected full-mesh handoff tools.
/// Execution is a sequential handoff chain; the swarm ends when the
/// active member produces a final output instead of handing off.
pub struct SwarmAgent<Ctx> {
    name: String,
    description: String,
    members: Vec<Arc<dyn Agent<Ctx>>>,
    entry_idx: usize,
    max_handoffs: Option<u32>,
}

impl<Ctx> SwarmAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Start building a swarm.
    pub fn builder() -> SwarmAgentBuilder<Ctx> {
        SwarmAgentBuilder {
            name: None,
            description: String::new(),
            members: Vec::new(),
            entry: None,
            max_handoffs: None,
        }
    }
}

#[async_trait]
impl<Ctx> Agent<Ctx> for SwarmAgent<Ctx>
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
        // Strong ownership moves into the stream: a caller may drop the
        // SwarmAgent before draining (`'static` stream contract).
        let members = self.members.clone();
        let entry = Arc::clone(&self.members[self.entry_idx]);
        let max_handoffs = self.max_handoffs;

        let stream = async_stream::stream! {
            let _members_alive = members;
            let parent_failure = ctx.failure_handle();
            let span = workflow_run_span(&name, ctx.tracer());
            yield AgentEvent::RunStarted { agent: name.clone() };

            let max = max_depth(ctx.run_config());
            if ctx.agent_depth() + 1 > max {
                let err = AgentError::MaxAgentDepthExceeded {
                    depth: ctx.agent_depth() + 1,
                    max,
                };
                let msg = err.to_string();
                parent_failure.set(err);
                span.record("otel.status_code", "ERROR");
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            let child = ctx.subagent_child();
            let child_failure = child.failure_handle();
            yield AgentEvent::AgentUpdated { agent: entry.name().to_owned() };

            let mut sub = match entry.run(child, input).instrument(span.clone()).await {
                Ok(s) => s,
                Err(e) => {
                    let msg = e.to_string();
                    parent_failure.set(e);
                    span.record("otel.status_code", "ERROR");
                    yield AgentEvent::RunFailed { error: msg };
                    return;
                }
            };

            let mut hops: u32 = 0;
            let mut failed = false;
            while let Some(ev) = sub.next().instrument(span.clone()).await {
                match ev {
                    AgentEvent::RunStarted { .. } => {}
                    AgentEvent::HandoffItem { from, to } => {
                        hops += 1;
                        if let Some(limit) = max_handoffs {
                            if hops > limit {
                                // The budget-busting handoff is not forwarded.
                                drop(sub);
                                let err = AgentError::MaxHandoffsExceeded { limit };
                                let msg = err.to_string();
                                parent_failure.set(err);
                                span.record("otel.status_code", "ERROR");
                                yield AgentEvent::RunFailed { error: msg };
                                return;
                            }
                        }
                        yield AgentEvent::HandoffItem { from, to };
                    }
                    AgentEvent::RunCompleted { usage } => {
                        span.record("gen_ai.usage.input_tokens", usage.input_tokens as i64);
                        span.record("gen_ai.usage.output_tokens", usage.output_tokens as i64);
                        yield AgentEvent::RunCompleted { usage };
                    }
                    AgentEvent::RunFailed { error } => {
                        failed = true;
                        span.record("otel.status_code", "ERROR");
                        yield AgentEvent::RunFailed { error };
                    }
                    other => yield other,
                }
            }

            if failed {
                if let Some(e) = child_failure.take() {
                    parent_failure.set(e);
                }
            }
            for hook in ctx.hooks().iter() {
                let _ = hook
                    .on_event(&ctx, &crate::HookEvent::OnSubagentStop {
                        agent: entry.name().to_owned(),
                    })
                    .await;
            }
        };

        Ok(Box::pin(stream))
    }
}
```

In `lib.rs`, next to the workflow re-exports, add module + re-export with doc comments:

```rust
mod swarm;
pub use swarm::{SwarmAgent, SwarmAgentBuilder, SwarmBuildError};
```

(match the file's existing `mod`/`pub use` style; each `pub use` line inherits item docs — no extra doc needed on re-exports in core's lib.rs if the existing style omits them; follow whatever `workflow` does.)

If `HookEvent::OnSubagentStop` or `hooks().iter()` differ in spelling, copy the exact call shape from `SequentialAgent::run` in `workflow.rs:280-286` — the swarm's post-drain hook block must be identical to Sequential's.

- [ ] **Step 4: Run builder tests** → `cargo test -p paigasus-helikon-core --test swarm` → the three build tests PASS.

- [ ] **Step 5: Write failing runtime tests** (append to `tests/swarm.rs`)

```rust
#[tokio::test]
async fn swarm_converges_on_winner() {
    // triage hands off to budgeting; budgeting answers.
    let triage = member("triage", vec![transfer_turn("budgeting")]);
    let budgeting = member("budgeting", vec![text_final("Cut subscriptions by $40.")]);
    let investing = member("investing", vec![]);

    let swarm = SwarmAgent::builder()
        .name("support_swarm")
        .description("finance pool")
        .member(triage)
        .member(budgeting)
        .member(investing)
        .entry("triage")
        .max_handoffs(4)
        .build()
        .unwrap();

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let stream = swarm.run(ctx, AgentInput::from_user_text("help me budget")).await.unwrap();
    let events: Vec<AgentEvent> = stream.collect().await;

    assert!(matches!(&events[0], AgentEvent::RunStarted { agent } if agent == "support_swarm"));
    assert!(events.iter().any(|e| matches!(e, AgentEvent::HandoffItem { from, to } if from == "triage" && to == "budgeting")));
    // exactly one RunStarted (the swarm's own; children swallowed)
    assert_eq!(events.iter().filter(|e| matches!(e, AgentEvent::RunStarted { .. })).count(), 1);

    // Re-run through collect() to check final output attribution.
    let triage = member("triage", vec![transfer_turn("budgeting")]);
    let budgeting = member("budgeting", vec![text_final("Cut subscriptions by $40.")]);
    let swarm = SwarmAgent::builder()
        .name("support_swarm")
        .member(triage)
        .member(budgeting)
        .build()
        .unwrap();
    let ctx: RunContext<()> = RunContext::ephemeral(());
    let stream = swarm.run(ctx, AgentInput::from_user_text("help me budget")).await.unwrap();
    let result = RunResultStreaming::new(stream).collect().await.unwrap();
    assert_eq!(result.final_output, "Cut subscriptions by $40.");
}

#[tokio::test]
async fn swarm_ping_pong_hits_max_handoffs() {
    // a and b transfer to each other forever (each gets plenty of scripts).
    let a = member("a", vec![transfer_turn("b"); 8]);
    let b = member("b", vec![transfer_turn("a"); 8]);
    let swarm = SwarmAgent::builder()
        .name("pingpong")
        .member(a)
        .member(b)
        .max_handoffs(3)
        .build()
        .unwrap();

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let stream = swarm.run(ctx, AgentInput::from_user_text("go")).await.unwrap();
    let err = RunResultStreaming::new(stream).collect().await.unwrap_err();
    assert!(err.to_string().contains("max handoffs (3) exceeded"), "got: {err}");
}

#[tokio::test]
async fn swarm_ping_pong_without_budget_hits_depth_bound() {
    let a = member("a", vec![transfer_turn("b"); 12]);
    let b = member("b", vec![transfer_turn("a"); 12]);
    let swarm = SwarmAgent::builder().name("pingpong").member(a).member(b).build().unwrap();

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let stream = swarm.run(ctx, AgentInput::from_user_text("go")).await.unwrap();
    let err = RunResultStreaming::new(stream).collect().await.unwrap_err();
    assert!(err.to_string().contains("nesting depth"), "got: {err}");
}

#[tokio::test]
async fn swarm_stream_survives_dropping_the_swarm() {
    let triage = member("triage", vec![transfer_turn("budgeting")]);
    let budgeting = member("budgeting", vec![text_final("done")]);
    let swarm = SwarmAgent::builder().name("s").member(triage).member(budgeting).build().unwrap();

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let stream = swarm.run(ctx, AgentInput::from_user_text("x")).await.unwrap();
    drop(swarm); // stream must own the members
    let result = RunResultStreaming::new(stream).collect().await.unwrap();
    assert_eq!(result.final_output, "done");
}
```

Note: `RunResultStreaming::collect()` returns `Err(RunError::Other(msg))` when no typed failure slot is attached; the swarm's `parent_failure` is the *context's* slot, which `RunResultStreaming::new` doesn't know — so the error string assertions (not variant matches) are the correct checks here.

- [ ] **Step 6: Run** → failures on unimplemented behavior; fix `swarm.rs` until all PASS: `cargo test -p paigasus-helikon-core --test swarm`.

- [ ] **Step 7: Full-crate check + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
cargo test -p paigasus-helikon-core
git add crates/paigasus-helikon-core
git commit -m "feat(core): SMA-333 add SwarmAgent with full-mesh handoffs and hop budget"
```

### Task 3: `GraphAgent` builder + validation

**Files:**
- Create: `crates/paigasus-helikon-core/src/graph.rs` (builder half)
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Test: `crates/paigasus-helikon-core/tests/graph.rs`

**Interfaces:**
- Produces: `pub struct GraphAgent<Ctx>`, `GraphAgent::builder() -> GraphAgentBuilder<Ctx>` with `.name()`, `.description()`, `.node(impl Into<String>, impl Agent<Ctx> + 'static)`, `.shared_node(impl Into<String>, Arc<dyn Agent<Ctx>>)`, `.edge(from, to)`, `.build() -> Result<GraphAgent<Ctx>, GraphBuildError>`; `pub enum GraphBuildError { Empty, MissingName, DuplicateNode(String), UnknownNode(String), Cycle(Vec<String>) }`. Internal (used by Task 4): `GraphAgent { name, description, nodes: Vec<(String, Arc<dyn Agent<Ctx>>)>, preds: Vec<Vec<usize>>, succs: Vec<Vec<usize>> }`.

- [ ] **Step 1: Failing tests** — create `crates/paigasus-helikon-core/tests/graph.rs`:

```rust
//! GraphAgent integration tests (SMA-333).

#[path = "common/mod.rs"]
mod common;

use paigasus_helikon_core::{GraphAgent, GraphBuildError, LlmAgent, ModelEvent, FinishReason};

fn node_agent(name: &str, reply: &str) -> LlmAgent<(), common::MockModel> {
    LlmAgent::builder::<()>()
        .name(name)
        .description(format!("node {name}"))
        .shared_model(common::MockModel::with_scripts(vec![vec![
            ModelEvent::TokenDelta { text: reply.to_owned() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ]]))
        .instructions("test")
        .build()
}

#[test]
fn graph_build_rejects_cycle() {
    let err = GraphAgent::builder()
        .name("g")
        .node("a", node_agent("a", "x"))
        .node("b", node_agent("b", "x"))
        .edge("a", "b")
        .edge("b", "a")
        .build()
        .unwrap_err();
    assert!(matches!(err, GraphBuildError::Cycle(nodes) if nodes.contains(&"a".to_owned())));
}

#[test]
fn graph_build_rejects_unknown_edge_endpoint() {
    let err = GraphAgent::builder()
        .name("g")
        .node("a", node_agent("a", "x"))
        .edge("a", "ghost")
        .build()
        .unwrap_err();
    assert!(matches!(err, GraphBuildError::UnknownNode(n) if n == "ghost"));
}

#[test]
fn graph_build_rejects_duplicate_node_and_empty() {
    let err = GraphAgent::builder()
        .name("g")
        .node("a", node_agent("a", "x"))
        .node("a", node_agent("a", "x"))
        .build()
        .unwrap_err();
    assert!(matches!(err, GraphBuildError::DuplicateNode(n) if n == "a"));
    let err = GraphAgent::<()>::builder().name("g").build().unwrap_err();
    assert!(matches!(err, GraphBuildError::Empty));
}
```

- [ ] **Step 2: Run** → COMPILE FAIL. `cargo test -p paigasus-helikon-core --test graph`

- [ ] **Step 3: Implement builder half of `graph.rs`**

```rust
//! `GraphAgent` — a declared DAG of agents; node execution gated by
//! dependencies (SMA-333, ADR-11).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use tracing::Instrument as _;

use crate::workflow::{assistant_text, max_depth, workflow_run_span};
use crate::{
    Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
};

/// Errors from [`GraphAgentBuilder::build`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphBuildError {
    /// The graph has no nodes.
    #[error("graph has no nodes")]
    Empty,
    /// `.name(…)` was never called.
    #[error("graph has no name")]
    MissingName,
    /// Two nodes share a name.
    #[error("duplicate graph node name: {0}")]
    DuplicateNode(String),
    /// An edge references a node that doesn't exist.
    #[error("unknown graph node in edge: {0}")]
    UnknownNode(String),
    /// The declared edges contain a cycle (node names listed).
    #[error("graph contains a cycle among nodes: {0:?}")]
    Cycle(Vec<String>),
}

/// Builder for [`GraphAgent`].
pub struct GraphAgentBuilder<Ctx> {
    name: Option<String>,
    description: String,
    nodes: Vec<(String, Arc<dyn Agent<Ctx>>)>,
    edges: Vec<(String, String)>,
}

impl<Ctx> GraphAgentBuilder<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Set the graph's agent name (required).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
    /// Set the graph's description.
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = d.into();
        self
    }
    /// Add a node.
    pub fn node(mut self, name: impl Into<String>, agent: impl Agent<Ctx> + 'static) -> Self {
        self.nodes.push((name.into(), Arc::new(agent)));
        self
    }
    /// Add a node from a shared agent.
    pub fn shared_node(mut self, name: impl Into<String>, agent: Arc<dyn Agent<Ctx>>) -> Self {
        self.nodes.push((name.into(), agent));
        self
    }
    /// Declare a dependency edge `from → to` (`to` runs after `from`).
    pub fn edge(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.edges.push((from.into(), to.into()));
        self
    }

    /// Validate (duplicates, unknown endpoints, cycles) and build.
    pub fn build(self) -> Result<GraphAgent<Ctx>, GraphBuildError> {
        let name = self.name.ok_or(GraphBuildError::MissingName)?;
        if self.nodes.is_empty() {
            return Err(GraphBuildError::Empty);
        }
        let mut index: HashMap<String, usize> = HashMap::new();
        for (i, (n, _)) in self.nodes.iter().enumerate() {
            if index.insert(n.clone(), i).is_some() {
                return Err(GraphBuildError::DuplicateNode(n.clone()));
            }
        }
        let n = self.nodes.len();
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut seen_edges = HashSet::new();
        for (from, to) in &self.edges {
            let f = *index.get(from).ok_or_else(|| GraphBuildError::UnknownNode(from.clone()))?;
            let t = *index.get(to).ok_or_else(|| GraphBuildError::UnknownNode(to.clone()))?;
            if seen_edges.insert((f, t)) {
                preds[t].push(f);
                succs[f].push(t);
            }
        }
        // Kahn's algorithm: leftover nodes are on cycles.
        let mut indegree: Vec<usize> = preds.iter().map(Vec::len).collect();
        let mut queue: VecDeque<usize> =
            (0..n).filter(|&i| indegree[i] == 0).collect();
        let mut visited = 0usize;
        while let Some(i) = queue.pop_front() {
            visited += 1;
            for &j in &succs[i] {
                indegree[j] -= 1;
                if indegree[j] == 0 {
                    queue.push_back(j);
                }
            }
        }
        if visited != n {
            let mut cyclic: Vec<String> = indegree
                .iter()
                .enumerate()
                .filter(|(_, d)| **d > 0)
                .map(|(i, _)| self.nodes[i].0.clone())
                .collect();
            cyclic.sort();
            return Err(GraphBuildError::Cycle(cyclic));
        }
        Ok(GraphAgent {
            name,
            description: self.description,
            nodes: self.nodes,
            preds,
            succs,
        })
    }
}

/// A declared DAG of agents with dependency-gated execution.
pub struct GraphAgent<Ctx> {
    name: String,
    description: String,
    nodes: Vec<(String, Arc<dyn Agent<Ctx>>)>,
    preds: Vec<Vec<usize>>,
    succs: Vec<Vec<usize>>,
}

impl<Ctx> GraphAgent<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Start building a graph.
    pub fn builder() -> GraphAgentBuilder<Ctx> {
        GraphAgentBuilder {
            name: None,
            description: String::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }
}
```

`lib.rs`: `mod graph;` + `pub use graph::{GraphAgent, GraphAgentBuilder, GraphBuildError};`. Add a temporary `impl Agent` stub ONLY if lib fails to compile without it — otherwise leave `run` to Task 4 (the struct alone compiles; there is no `impl Agent` yet and the tests above don't need one).

- [ ] **Step 4: Run** → PASS: `cargo test -p paigasus-helikon-core --test graph`

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && git add crates/paigasus-helikon-core
git commit -m "feat(core): SMA-333 add GraphAgent builder with cycle detection"
```

### Task 4: `GraphAgent::run` — wavefront scheduler

**Files:**
- Modify: `crates/paigasus-helikon-core/src/graph.rs`
- Test: `crates/paigasus-helikon-core/tests/graph.rs`

**Interfaces:**
- Consumes: Task 3's `GraphAgent` fields; `ctx.state().set(key, value)`; `assistant_text`; `Item::UserMessage`/`Item::AssistantMessage`/`ContentPart::Text`; `futures_util::stream::SelectAll`.
- Produces: `impl Agent<Ctx> for GraphAgent<Ctx>`. Behavior contract (evals/CLI never see this — core-only): node outputs land in `ctx.state()` under the node name; final synthesized `MessageOutput` = single sink's text verbatim, or deterministic JSON `{sink: text}` for >1 sink; failed node → transitive descendants skipped and named in the aggregate `RunFailed`.

- [ ] **Step 1: Failing tests** (append to `tests/graph.rs`):

```rust
use std::sync::Arc;
use futures_util::StreamExt as _;
use paigasus_helikon_core::{Agent, AgentEvent, AgentInput, RunContext, RunResultStreaming};

fn failing_agent(name: &str) -> LlmAgent<(), common::MockModel> {
    // MockModel with zero scripts → invoke errors → run fails.
    LlmAgent::builder::<()>()
        .name(name)
        .description("fails")
        .shared_model(common::MockModel::with_scripts(vec![]))
        .instructions("test")
        .build()
}

#[tokio::test]
async fn graph_diamond_runs_in_dependency_order() {
    // a → b, a → c, b → d, c → d ; d is the single sink.
    let graph = GraphAgent::builder()
        .name("diamond")
        .node("a", node_agent("a", "A-out"))
        .node("b", node_agent("b", "B-out"))
        .node("c", node_agent("c", "C-out"))
        .node("d", node_agent("d", "D-final"))
        .edge("a", "b").edge("a", "c").edge("b", "d").edge("c", "d")
        .build()
        .unwrap();

    let ctx: RunContext<()> = RunContext::ephemeral(());
    let state = ctx.state().clone();
    let stream = graph.run(ctx, AgentInput::from_user_text("go")).await.unwrap();
    let result = RunResultStreaming::new(stream).collect().await.unwrap();

    assert_eq!(result.final_output, "D-final"); // single sink: verbatim
    assert_eq!(state.get("a"), Some(serde_json::json!("A-out")));
    assert_eq!(state.get("d"), Some(serde_json::json!("D-final")));
    // d must start only after both b and c completed: check event order —
    // the AgentUpdated{d} index is after both b/c RunCompleted-adjacent
    // MessageOutputs. Simplest robust check: collect AgentUpdated order.
    let ctx: RunContext<()> = RunContext::ephemeral(());
    let graph2 = GraphAgent::builder()
        .name("chain")
        .node("first", node_agent("first", "1"))
        .node("second", node_agent("second", "2"))
        .edge("first", "second")
        .build().unwrap();
    let events: Vec<AgentEvent> = graph2.run(ctx, AgentInput::from_user_text("go")).await.unwrap().collect().await;
    let order: Vec<String> = events.iter().filter_map(|e| match e {
        AgentEvent::AgentUpdated { agent } => Some(agent.clone()), _ => None
    }).collect();
    assert_eq!(order, vec!["first".to_owned(), "second".to_owned()]);
}

#[tokio::test]
async fn graph_multi_sink_merges_deterministically() {
    // a → b, a → c ; sinks b and c.
    let graph = GraphAgent::builder()
        .name("fanout")
        .node("a", node_agent("a", "A"))
        .node("b", node_agent("b", "B"))
        .node("c", node_agent("c", "C"))
        .edge("a", "b").edge("a", "c")
        .build().unwrap();
    let ctx: RunContext<()> = RunContext::ephemeral(());
    let result = RunResultStreaming::new(
        graph.run(ctx, AgentInput::from_user_text("go")).await.unwrap()
    ).collect().await.unwrap();
    assert_eq!(result.final_output, r#"{"b":"B","c":"C"}"#);
}

#[tokio::test]
async fn graph_failure_skips_descendants_but_completes_independent_branch() {
    // bad → child ; solo is independent.
    let graph = GraphAgent::builder()
        .name("partial")
        .node("bad", failing_agent("bad"))
        .node("child", node_agent("child", "never"))
        .node("solo", node_agent("solo", "solo-out"))
        .edge("bad", "child")
        .build().unwrap();
    let ctx: RunContext<()> = RunContext::ephemeral(());
    let state = ctx.state().clone();
    let err = RunResultStreaming::new(
        graph.run(ctx, AgentInput::from_user_text("go")).await.unwrap()
    ).collect().await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("bad"), "failed node named: {msg}");
    assert!(msg.contains("child"), "skipped node named: {msg}");
    assert_eq!(state.get("solo"), Some(serde_json::json!("solo-out"))); // independent branch ran
    assert_eq!(state.get("child"), None); // descendant skipped
}
```

- [ ] **Step 2: Run** → COMPILE FAIL (`graph.run` unresolved / no `impl Agent`).

- [ ] **Step 3: Implement `run`** (append to `graph.rs`):

```rust
#[async_trait]
impl<Ctx> Agent<Ctx> for GraphAgent<Ctx>
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
        let nodes = self.nodes.clone();
        let preds = self.preds.clone();
        let succs = self.succs.clone();

        let stream = async_stream::stream! {
            let parent_failure = ctx.failure_handle();
            let span = workflow_run_span(&name, ctx.tracer());
            yield AgentEvent::RunStarted { agent: name.clone() };

            let max = max_depth(ctx.run_config());
            if ctx.agent_depth() + 1 > max {
                let err = AgentError::MaxAgentDepthExceeded { depth: ctx.agent_depth() + 1, max };
                let msg = err.to_string();
                parent_failure.set(err);
                span.record("otel.status_code", "ERROR");
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            let n = nodes.len();
            let mut indegree: Vec<usize> = preds.iter().map(Vec::len).collect();
            let mut finals: Vec<Option<String>> = vec![None; n];
            let mut skipped = vec![false; n];
            let mut failed_nodes: Vec<usize> = Vec::new();
            let mut failures: Vec<Option<crate::FailureSlot>> = vec![None; n];
            let mut total = TokenUsage::default();
            let mut running = futures_util::stream::SelectAll::new();
            let mut ready: VecDeque<usize> =
                (0..n).filter(|&i| indegree[i] == 0).collect();

            loop {
                // Launch everything currently ready (single start site).
                while let Some(i) = ready.pop_front() {
                    let child = ctx.subagent_child();
                    failures[i] = Some(child.failure_handle());
                    yield AgentEvent::AgentUpdated { agent: nodes[i].1.name().to_owned() };

                    // Node input = original input + predecessor outputs as
                    // labeled context messages (declared-edge order).
                    let mut messages = input.messages.clone();
                    for &p in &preds[i] {
                        if let Some(text) = &finals[p] {
                            messages.push(Item::UserMessage {
                                content: vec![ContentPart::Text {
                                    text: format!("[{} output]\n{}", nodes[p].0, text),
                                }],
                            });
                        }
                    }
                    let node_input = AgentInput { messages };

                    match nodes[i].1.run(child, node_input).instrument(span.clone()).await {
                        Ok(s) => running.push(Box::pin(s.map(move |ev| (i, ev)))
                            as BoxStream<'static, (usize, AgentEvent)>),
                        Err(e) => {
                            failed_nodes.push(i);
                            parent_failure.set(e);
                            mark_skipped(i, &succs, &mut skipped);
                        }
                    }
                }

                let Some((i, ev)) = running.next().instrument(span.clone()).await else {
                    break;
                };
                match ev {
                    AgentEvent::RunStarted { .. } => {}
                    AgentEvent::MessageOutput { item } => {
                        if let Some(t) = assistant_text(&item) {
                            finals[i] = Some(t);
                        }
                        yield AgentEvent::MessageOutput { item };
                    }
                    AgentEvent::RunCompleted { usage } => {
                        total.add(usage);
                        let node_name = nodes[i].0.clone();
                        ctx.state().set(node_name, finals[i].clone().unwrap_or_default());
                        for hook in ctx.hooks().iter() {
                            let _ = hook
                                .on_event(&ctx, &crate::HookEvent::OnSubagentStop {
                                    agent: nodes[i].1.name().to_owned(),
                                })
                                .await;
                        }
                        for &j in &succs[i] {
                            indegree[j] -= 1;
                            if indegree[j] == 0 && !skipped[j] {
                                ready.push_back(j);
                            }
                        }
                    }
                    AgentEvent::RunFailed { .. } => {
                        failed_nodes.push(i);
                        for hook in ctx.hooks().iter() {
                            let _ = hook
                                .on_event(&ctx, &crate::HookEvent::OnSubagentStop {
                                    agent: nodes[i].1.name().to_owned(),
                                })
                                .await;
                        }
                        mark_skipped(i, &succs, &mut skipped);
                    }
                    other => yield other,
                }
            }

            if !failed_nodes.is_empty() {
                let first_err = failed_nodes
                    .iter()
                    .find_map(|&i| failures[i].as_ref().and_then(crate::FailureSlot::take))
                    .unwrap_or_else(|| AgentError::Other(anyhow::anyhow!("a graph node failed")));
                let mut failed_names: Vec<&str> =
                    failed_nodes.iter().map(|&i| nodes[i].0.as_str()).collect();
                failed_names.sort_unstable();
                let mut skipped_names: Vec<&str> = skipped
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| **s)
                    .map(|(i, _)| nodes[i].0.as_str())
                    .collect();
                skipped_names.sort_unstable();
                let msg = format!(
                    "graph node(s) {failed_names:?} failed ({first_err}); skipped downstream: {skipped_names:?}"
                );
                parent_failure.set(first_err);
                span.record("otel.status_code", "ERROR");
                yield AgentEvent::RunFailed { error: msg };
                return;
            }

            // Deterministic synthesized final message from the sinks.
            let mut sink_outputs: BTreeMap<String, String> = BTreeMap::new();
            for i in 0..n {
                if succs[i].is_empty() {
                    sink_outputs.insert(nodes[i].0.clone(), finals[i].clone().unwrap_or_default());
                }
            }
            let final_text = if sink_outputs.len() == 1 {
                sink_outputs.into_values().next().unwrap_or_default()
            } else {
                serde_json::to_string(&sink_outputs).unwrap_or_else(|_| "{}".to_owned())
            };
            yield AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text { text: final_text }],
                    agent: Some(name.clone()),
                },
            };
            span.record("gen_ai.usage.input_tokens", total.input_tokens as i64);
            span.record("gen_ai.usage.output_tokens", total.output_tokens as i64);
            yield AgentEvent::RunCompleted { usage: total };
        };

        Ok(Box::pin(stream))
    }
}

/// Mark all transitive descendants of `i` as skipped.
fn mark_skipped(i: usize, succs: &[Vec<usize>], skipped: &mut [bool]) {
    let mut stack = vec![i];
    while let Some(k) = stack.pop() {
        for &j in &succs[k] {
            if !skipped[j] {
                skipped[j] = true;
                stack.push(j);
            }
        }
    }
}
```

Note: `FailureSlot::take(&self)` takes `&self` — the `find_map(|&i| failures[i].as_ref().and_then(crate::FailureSlot::take))` call may need the closure form `.and_then(|f| f.take())`. `AgentInput { messages }` literal construction is fine in-crate despite `#[non_exhaustive]`.

- [ ] **Step 4: Run until green**: `cargo test -p paigasus-helikon-core --test graph`

- [ ] **Step 5: Full core gate + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
cargo test -p paigasus-helikon-core
git add crates/paigasus-helikon-core
git commit -m "feat(core): SMA-333 add GraphAgent wavefront scheduler with skip propagation"
```

---

## Task Group B — `paigasus-helikon-evals`

### Task 5: evals scaffolding + `EvalDataset` (JSONL)

**Files:**
- Modify: root `Cargo.toml` (`[workspace.dependencies]`: add `jsonschema`; evals pin stays `0.0.0` until Task 19)
- Modify: `crates/paigasus-helikon-evals/Cargo.toml` (deps + features; version stays `0.0.0` + `publish = false` until Task 19)
- Create: `crates/paigasus-helikon-evals/src/lib.rs` (rewrite), `src/error.rs`, `src/dataset.rs`
- Test: `crates/paigasus-helikon-evals/tests/dataset.rs`

**Interfaces:**
- Produces: `EvalError` (thiserror, `#[non_exhaustive]`, variants `Io(#[from] std::io::Error)`, `Parse { line: usize, source: serde_json::Error }`, `InvalidSchema(String)`, `MissingCtxFactory`, `MissingAgent`, `MissingDataset`, `Run(String)`, `Other(#[from] anyhow::Error)`); `EvalCase { id: String, input: String, expected: Option<serde_json::Value>, expected_tools: Option<Vec<String>>, metadata: serde_json::Map<String, serde_json::Value> }` (serde `Deserialize`+`Serialize`, `id` defaults to `case-<line#>` when absent); `EvalDataset { name: String, cases: Vec<EvalCase> }` with `from_jsonl_path(&Path)`, `from_jsonl_str(name, &str)`.

- [ ] **Step 1: Wire deps.** Root `Cargo.toml` `[workspace.dependencies]` — add (alphabetical position; resolve the current latest compatible with MSRV 1.94 by checking `cargo info jsonschema` — pin the major that supports draft 2020-12 without pulling reqwest):

```toml
jsonschema            = { version = "0.33", default-features = false }
```

(If `0.33` is superseded, use the newest; `default-features = false` drops the HTTP resolver and its reqwest tree — verify `validator_for` still compiles; if the crate requires a feature for draft 2020-12 add exactly that feature.)

`crates/paigasus-helikon-evals/Cargo.toml` — replace the whole file:

```toml
[package]
name        = "paigasus-helikon-evals"
description = "Evaluation harness for the Paigasus Helikon AI SDK."
version                = "0.0.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true
publish                = false

[features]
default       = []
trace-sqlite  = ["dep:sqlx"]
trace-parquet = ["dep:arrow", "dep:parquet"]

[dependencies]
paigasus-helikon-core          = { workspace = true }
paigasus-helikon-runtime-tokio = { workspace = true }
anyhow      = { workspace = true }
async-trait = { workspace = true }
futures-util = { workspace = true }
jiff        = { workspace = true }
jsonschema  = { workspace = true }
serde       = { workspace = true }
serde_json  = { workspace = true }
thiserror   = { workspace = true }
tokio       = { workspace = true }
uuid        = { workspace = true }
sqlx        = { workspace = true, optional = true }
arrow       = { workspace = true, optional = true }
parquet     = { workspace = true, optional = true }

[dev-dependencies]
tempfile = { workspace = true }

[lints]
workspace = true
```

`arrow`/`parquet` workspace entries land in Task 10 (the optional deps here are inert until then — if `cargo metadata` complains about missing workspace entries, add the two lines from Task 10 Step 1 now instead).

`src/lib.rs` (rewrite; module list grows in later tasks — add all `mod`s now with stub files or add incrementally; incremental is fine):

```rust
//! Evaluation harness for Paigasus Helikon agents: datasets, evaluators,
//! deterministic replay via [`MockModel`], and trace recording.
//!
//! The core loop: load an [`EvalDataset`], point an [`EvalRun`] at an
//! agent, attach [`Evaluator`]s, and collect an [`EvalReport`] of
//! trajectory and final-response scores.

mod dataset;
mod error;

pub use dataset::{EvalCase, EvalDataset};
pub use error::EvalError;
```

`src/error.rs`:

```rust
//! Error types for the evals crate.

/// Errors produced by dataset loading, evaluation, and eval runs.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EvalError {
    /// Reading a dataset or script file failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A JSONL line failed to parse.
    #[error("parse error on line {line}: {source}")]
    Parse {
        /// 1-based line number in the JSONL file.
        line: usize,
        /// The underlying serde error.
        source: serde_json::Error,
    },
    /// A JSON Schema failed to compile.
    #[error("invalid json schema: {0}")]
    InvalidSchema(String),
    /// `EvalRun` was started without a context factory.
    #[error("EvalRun requires a ctx_factory (or default_ctx)")]
    MissingCtxFactory,
    /// `EvalRun` was started without an agent or agent factory.
    #[error("EvalRun requires an agent or agent_factory")]
    MissingAgent,
    /// `EvalRun` was started without a dataset.
    #[error("EvalRun requires a dataset")]
    MissingDataset,
    /// An agent run failed during evaluation.
    #[error("agent run failed: {0}")]
    Run(String),
    /// Any other error.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

`src/dataset.rs`:

```rust
//! JSONL eval datasets.

use std::path::Path;

use crate::EvalError;

/// One evaluation case: an input plus optional expectations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalCase {
    /// Case identifier (defaults to `case-<line#>` when absent in JSONL).
    #[serde(default)]
    pub id: String,
    /// The user-turn input text.
    pub input: String,
    /// Expected final output (string, or JSON for structural comparison).
    #[serde(default)]
    pub expected: Option<serde_json::Value>,
    /// Expected tool-call names, in order.
    #[serde(default)]
    pub expected_tools: Option<Vec<String>>,
    /// Free-form per-case metadata.
    #[serde(default)]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

/// A named collection of [`EvalCase`]s.
#[derive(Debug, Clone)]
pub struct EvalDataset {
    /// Dataset name (defaults to the file stem).
    pub name: String,
    /// The cases, in file order.
    pub cases: Vec<EvalCase>,
}

impl EvalDataset {
    /// Load a dataset from a JSONL file (one `EvalCase` per line; blank
    /// lines skipped).
    pub fn from_jsonl_path(path: &Path) -> Result<Self, EvalError> {
        let text = std::fs::read_to_string(path)?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "dataset".to_owned());
        Self::from_jsonl_str(&name, &text)
    }

    /// Parse a dataset from JSONL text.
    pub fn from_jsonl_str(name: &str, s: &str) -> Result<Self, EvalError> {
        let mut cases = Vec::new();
        for (idx, line) in s.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let mut case: EvalCase = serde_json::from_str(line)
                .map_err(|source| EvalError::Parse { line: idx + 1, source })?;
            if case.id.is_empty() {
                case.id = format!("case-{}", idx + 1);
            }
            cases.push(case);
        }
        Ok(Self { name: name.to_owned(), cases })
    }
}
```

- [ ] **Step 2: Failing test** — `crates/paigasus-helikon-evals/tests/dataset.rs`:

```rust
//! EvalDataset JSONL parsing tests.

use paigasus_helikon_evals::{EvalDataset, EvalError};

#[test]
fn parses_jsonl_with_defaults() {
    let jsonl = r#"
{"id":"greet","input":"Hi","expected":"Hello"}
{"input":"tools?","expected_tools":["lookup_spending"]}
"#;
    let ds = EvalDataset::from_jsonl_str("triage", jsonl).unwrap();
    assert_eq!(ds.name, "triage");
    assert_eq!(ds.cases.len(), 2);
    assert_eq!(ds.cases[0].id, "greet");
    assert_eq!(ds.cases[1].id, "case-3"); // 1-based line numbering, blank line 1
    assert_eq!(ds.cases[1].expected_tools.as_deref(), Some(&["lookup_spending".to_owned()][..]));
    assert!(ds.cases[1].expected.is_none());
}

#[test]
fn reports_parse_error_line() {
    let err = EvalDataset::from_jsonl_str("x", "{\"input\":\"ok\"}\nnot json").unwrap_err();
    assert!(matches!(err, EvalError::Parse { line: 2, .. }));
}
```

- [ ] **Step 3: Run** `cargo test -p paigasus-helikon-evals` → fix until PASS.
- [ ] **Step 4: Docs gate** `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-evals --no-deps` → clean.
- [ ] **Step 5: Commit** `feat(evals): SMA-333 add crate scaffolding, EvalError, and jsonl datasets`

### Task 6: `MockModel` + `ScriptEvent` mirrors

**Files:**
- Create: `crates/paigasus-helikon-evals/src/mock.rs`, `src/script.rs`
- Modify: `src/lib.rs` (`mod` + `pub use MockModel, ScriptEvent, ScriptFinishReason, ScriptFile`)
- Test: `crates/paigasus-helikon-evals/tests/mock.rs`

**Interfaces:**
- Consumes: core `Model`, `ModelEvent` (5 variants), `FinishReason`, `ModelError`, `ModelRequest`, `ModelCapabilities`, `CancellationToken`.
- Produces: `MockModel::with_script(Vec<ModelEvent>) -> Arc<Self>`, `::with_scripts(Vec<Vec<ModelEvent>>) -> Arc<Self>`, `::from_script_file(&Path) -> Result<Arc<Self>, EvalError>` (uses `ScriptFile.default`); `ScriptEvent`/`ScriptFinishReason` (serde, `From` into core types); `ScriptFile { default: Vec<Vec<ScriptEvent>>, cases: BTreeMap<String, Vec<Vec<ScriptEvent>>> }` with `load(&Path) -> Result<Self, EvalError>` and `scripts_for(&self, case_id: &str) -> Vec<Vec<ModelEvent>>` (case entry, else `default`).

- [ ] **Step 1: Failing test** — `tests/mock.rs`:

```rust
//! MockModel replay + script mirror tests.

use paigasus_helikon_core::{CancellationToken, Model, ModelEvent, ModelRequest};
use paigasus_helikon_evals::{MockModel, ScriptFile};
use futures_util::StreamExt as _;

const SCRIPT_JSON: &str = r#"{
  "default": [[ {"type":"token_delta","text":"hi"}, {"type":"finish","reason":"stop"} ]],
  "cases": {
    "tools": [[
      {"type":"tool_call_delta","call_id":"c1","name":"lookup_spending","args_delta":"{}"},
      {"type":"finish","reason":"tool_calls"}
    ]]
  }
}"#;

#[tokio::test]
async fn replays_script_and_exhausts() {
    let model = MockModel::with_script(vec![
        ModelEvent::TokenDelta { text: "hello".into() },
    ]);
    let mut s = model.invoke(ModelRequest::new(), CancellationToken::new()).await.unwrap();
    let first = s.next().await.unwrap().unwrap();
    assert!(matches!(first, ModelEvent::TokenDelta { text } if text == "hello"));
    // second invoke: exhausted
    assert!(model.invoke(ModelRequest::new(), CancellationToken::new()).await.is_err());
}

#[test]
fn script_file_selects_per_case_with_default_fallback() {
    let f: ScriptFile = serde_json::from_str(SCRIPT_JSON).unwrap();
    let tools = f.scripts_for("tools");
    assert!(matches!(&tools[0][0], ModelEvent::ToolCallDelta { name: Some(n), .. } if n == "lookup_spending"));
    let dflt = f.scripts_for("anything-else");
    assert!(matches!(&dflt[0][0], ModelEvent::TokenDelta { text } if text == "hi"));
}
```

- [ ] **Step 2: Run** → COMPILE FAIL.
- [ ] **Step 3: Implement.** `src/script.rs`:

```rust
//! Serde mirror types for recorded model scripts. Core's `ModelEvent`
//! deliberately has no serde; these mirrors keep the file format local
//! to the evals crate (spec §4.3/§6E).

use std::collections::BTreeMap;
use std::path::Path;

use paigasus_helikon_core::{FinishReason, ModelEvent};

use crate::EvalError;

/// Serde mirror of core's `FinishReason`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptFinishReason {
    /// Natural end of turn.
    Stop,
    /// Token limit reached.
    Length,
    /// The model emitted tool calls.
    ToolCalls,
    /// Provider content filter fired.
    ContentFilter,
    /// Any other provider-specific reason.
    Other(String),
}

impl From<ScriptFinishReason> for FinishReason {
    fn from(r: ScriptFinishReason) -> Self {
        match r {
            ScriptFinishReason::Stop => FinishReason::Stop,
            ScriptFinishReason::Length => FinishReason::Length,
            ScriptFinishReason::ToolCalls => FinishReason::ToolCalls,
            ScriptFinishReason::ContentFilter => FinishReason::ContentFilter,
            ScriptFinishReason::Other(s) => FinishReason::Other(s),
        }
    }
}

/// Serde mirror of core's `ModelEvent` (same five variants).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScriptEvent {
    /// Mirror of `ModelEvent::TokenDelta`.
    TokenDelta {
        /// Text chunk.
        text: String,
    },
    /// Mirror of `ModelEvent::ReasoningDelta`.
    ReasoningDelta {
        /// Reasoning text chunk.
        text: String,
    },
    /// Mirror of `ModelEvent::ToolCallDelta`.
    ToolCallDelta {
        /// Provider call id.
        call_id: String,
        /// Tool name (first delta carries it).
        #[serde(default)]
        name: Option<String>,
        /// JSON-arguments fragment.
        args_delta: String,
    },
    /// Mirror of `ModelEvent::Usage`.
    Usage {
        /// Prompt tokens.
        input_tokens: u32,
        /// Completion tokens.
        output_tokens: u32,
        /// Cached prompt tokens, when reported.
        #[serde(default)]
        cached_input_tokens: Option<u32>,
        /// Reasoning tokens, when reported.
        #[serde(default)]
        reasoning_tokens: Option<u32>,
    },
    /// Mirror of `ModelEvent::Finish`.
    Finish {
        /// Why the turn ended.
        reason: ScriptFinishReason,
    },
}

impl From<ScriptEvent> for ModelEvent {
    fn from(e: ScriptEvent) -> Self {
        match e {
            ScriptEvent::TokenDelta { text } => ModelEvent::TokenDelta { text },
            ScriptEvent::ReasoningDelta { text } => ModelEvent::ReasoningDelta { text },
            ScriptEvent::ToolCallDelta { call_id, name, args_delta } => {
                ModelEvent::ToolCallDelta { call_id, name, args_delta }
            }
            ScriptEvent::Usage { input_tokens, output_tokens, cached_input_tokens, reasoning_tokens } => {
                ModelEvent::Usage { input_tokens, output_tokens, cached_input_tokens, reasoning_tokens }
            }
            ScriptEvent::Finish { reason } => ModelEvent::Finish { reason: reason.into() },
        }
    }
}

/// A recorded script file: per-invoke scripts, optionally keyed by case id.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ScriptFile {
    /// Scripts used when no case-specific entry matches.
    #[serde(default)]
    pub default: Vec<Vec<ScriptEvent>>,
    /// Case-id-keyed script sets (deterministic multi-case eval).
    #[serde(default)]
    pub cases: BTreeMap<String, Vec<Vec<ScriptEvent>>>,
}

impl ScriptFile {
    /// Load a script file from JSON.
    pub fn load(path: &Path) -> Result<Self, EvalError> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text)
            .map_err(|source| EvalError::Parse { line: 0, source })
    }

    /// Scripts for `case_id` (falling back to `default`), converted to
    /// core `ModelEvent`s.
    pub fn scripts_for(&self, case_id: &str) -> Vec<Vec<ModelEvent>> {
        self.cases
            .get(case_id)
            .unwrap_or(&self.default)
            .iter()
            .map(|script| script.iter().cloned().map(ModelEvent::from).collect())
            .collect()
    }
}
```

Note: core's `ModelEvent::Usage`/`ToolCallDelta` are `#[non_exhaustive]` **enum variants of a non_exhaustive enum in a foreign crate — literal construction of variants is allowed for enums unless the *variant* is marked non_exhaustive.** Core marks the enum, not the variants, so `ModelEvent::Usage { … }` constructs fine from evals (the existing tests in runtime-tokio already do this).

`src/mock.rs`:

```rust
//! A scripted `Model` for deterministic replay.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::{EvalError, ScriptFile};

/// A scripted [`Model`] that replays pre-recorded `ModelEvent`s: one
/// script per `invoke` call, in order. Running out of scripts yields a
/// `ModelError` — deterministic by construction.
pub struct MockModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl MockModel {
    /// A mock that answers exactly one `invoke` with `script`.
    pub fn with_script(script: Vec<ModelEvent>) -> Arc<Self> {
        Self::with_scripts(vec![script])
    }

    /// A mock that answers successive `invoke`s with successive scripts.
    pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self { scripts: Mutex::new(VecDeque::from(scripts)) })
    }

    /// Load the `default` scripts from a JSON script file.
    pub fn from_script_file(path: &Path) -> Result<Arc<Self>, EvalError> {
        let file = ScriptFile::load(path)?;
        Ok(Self::with_scripts(file.scripts_for("")))
    }
}

#[async_trait]
impl Model for MockModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let script = self
            .scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| ModelError::Other(anyhow::anyhow!("MockModel: no more scripted responses")))?;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    fn provider(&self) -> &str {
        "mock"
    }
}
```

futures-core must be a dependency: add `futures-core = { workspace = true }` to evals `[dependencies]`.

- [ ] **Step 4: Run until green.** `cargo test -p paigasus-helikon-evals`
- [ ] **Step 5: Commit** `feat(evals): SMA-333 add MockModel and script mirror types`

### Task 7: `Evaluator` trait + `ExactMatch` + `JsonSchemaConformance`

**Files:**
- Create: `src/evaluator.rs`, `src/evaluators/mod.rs`, `src/evaluators/exact_match.rs`, `src/evaluators/json_schema.rs`
- Modify: `src/lib.rs`
- Test: `crates/paigasus-helikon-evals/tests/evaluators.rs`

**Interfaces:**
- Produces: `CaseOutcome { final_output: String, events: Vec<AgentEvent>, usage: TokenUsage }`; `ScoreOutcome { Passed, Failed, Skipped }` (+ `Serialize`); `Score { value: f64, outcome: ScoreOutcome, detail: Option<String> }` with ctors `Score::passed(value)`, `Score::failed(value, detail: impl Into<String>)`, `Score::skipped(reason: impl Into<String>)` (skipped ⇒ value 0.0, detail = reason); `#[async_trait] trait Evaluator: Send + Sync { fn name(&self) -> &str; async fn evaluate(&self, case: &EvalCase, outcome: &CaseOutcome) -> Result<Score, EvalError>; }`; `ExactMatch::new()`, `.case_insensitive()`; `JsonSchemaConformance::new(schema: serde_json::Value) -> Result<Self, EvalError>`.

- [ ] **Step 1: Failing tests** — `tests/evaluators.rs`:

```rust
//! Built-in evaluator tests.

use paigasus_helikon_core::TokenUsage;
use paigasus_helikon_evals::{
    CaseOutcome, EvalCase, Evaluator, ExactMatch, JsonSchemaConformance, ScoreOutcome,
};

fn case(expected: Option<serde_json::Value>) -> EvalCase {
    EvalCase {
        id: "c1".into(),
        input: "q".into(),
        expected,
        expected_tools: None,
        metadata: serde_json::Map::new(),
    }
}

fn outcome(text: &str) -> CaseOutcome {
    CaseOutcome { final_output: text.into(), events: vec![], usage: TokenUsage::default() }
}

#[tokio::test]
async fn exact_match_string_and_json_and_skip() {
    let e = ExactMatch::new();
    let s = e.evaluate(&case(Some("Hello".into())), &outcome("  Hello ")).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
    let s = e.evaluate(&case(Some("Hello".into())), &outcome("nope")).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Failed));
    // JSON expected → structural comparison
    let s = e
        .evaluate(&case(Some(serde_json::json!({"a": 1}))), &outcome("{ \"a\": 1 }"))
        .await
        .unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
    // absent expected → skipped
    let s = e.evaluate(&case(None), &outcome("x")).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Skipped));
    // case-insensitive option
    let s = ExactMatch::new().case_insensitive()
        .evaluate(&case(Some("HELLO".into())), &outcome("hello")).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
}

#[tokio::test]
async fn json_schema_validates() {
    let schema = serde_json::json!({"type":"object","required":["month"],"properties":{"month":{"type":"string"}}});
    let e = JsonSchemaConformance::new(schema).unwrap();
    let s = e.evaluate(&case(None), &outcome(r#"{"month":"June"}"#)).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
    let s = e.evaluate(&case(None), &outcome(r#"{"day": 3}"#)).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Failed));
    assert!(s.detail.unwrap().contains("month"));
    let s = e.evaluate(&case(None), &outcome("not json")).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Failed));
}
```

- [ ] **Step 2: Run** → COMPILE FAIL.
- [ ] **Step 3: Implement.** `src/evaluator.rs`:

```rust
//! The `Evaluator` trait and score types.

use async_trait::async_trait;
use paigasus_helikon_core::{AgentEvent, TokenUsage};

use crate::{EvalCase, EvalError};

/// What one case's agent run produced.
#[derive(Debug, Clone)]
pub struct CaseOutcome {
    /// The run's final output text.
    pub final_output: String,
    /// The full event trajectory.
    pub events: Vec<AgentEvent>,
    /// Run-level token usage.
    pub usage: TokenUsage,
}

/// Pass/fail/skip classification of one score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreOutcome {
    /// The evaluator's criterion held.
    Passed,
    /// The criterion failed.
    Failed,
    /// The evaluator wasn't applicable to this case.
    Skipped,
}

/// One evaluator's verdict on one case.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Score {
    /// Score value in `[0, 1]`.
    pub value: f64,
    /// Pass/fail/skip classification.
    pub outcome: ScoreOutcome,
    /// Human-readable explanation (violations, diffs, skip reason).
    pub detail: Option<String>,
}

impl Score {
    /// A passing score.
    pub fn passed(value: f64) -> Self {
        Self { value, outcome: ScoreOutcome::Passed, detail: None }
    }
    /// A failing score with an explanation.
    pub fn failed(value: f64, detail: impl Into<String>) -> Self {
        Self { value, outcome: ScoreOutcome::Failed, detail: Some(detail.into()) }
    }
    /// A skipped (not-applicable) score.
    pub fn skipped(reason: impl Into<String>) -> Self {
        Self { value: 0.0, outcome: ScoreOutcome::Skipped, detail: Some(reason.into()) }
    }
}

/// Scores one case's outcome. Implementations must be side-effect free.
#[async_trait]
pub trait Evaluator: Send + Sync {
    /// Stable evaluator name (used in reports and trace sinks).
    fn name(&self) -> &str;
    /// Score `outcome` for `case`.
    async fn evaluate(&self, case: &EvalCase, outcome: &CaseOutcome) -> Result<Score, EvalError>;
}
```

`src/evaluators/exact_match.rs` — implement exactly the tested behavior: skip on `expected: None`; `Value::String` → trimmed (optionally lowercased) equality; other JSON → parse output, structural equality (on output parse failure → `Score::failed(0.0, "final output is not valid JSON: <err>")`). `src/evaluators/json_schema.rs` — `jsonschema::validator_for(&schema).map_err(|e| EvalError::InvalidSchema(e.to_string()))?`; evaluate: parse output (`Score::failed` on parse error), `validator.iter_errors(&value)` → collect messages into `detail`, empty ⇒ `Score::passed(1.0)`. `src/evaluators/mod.rs` re-exports; lib.rs adds `mod evaluator; mod evaluators;` + `pub use evaluator::{CaseOutcome, Evaluator, Score, ScoreOutcome}; pub use evaluators::{ExactMatch, JsonSchemaConformance};`.

- [ ] **Step 4: Run until green**, then **Step 5: Commit** `feat(evals): SMA-333 add Evaluator trait with ExactMatch and JsonSchemaConformance`

### Task 8: `LlmJudge` + `ToolUseTrajectory`

**Files:**
- Create: `src/evaluators/llm_judge.rs`, `src/evaluators/trajectory.rs`
- Modify: `src/evaluators/mod.rs`, `src/lib.rs`
- Test: append to `tests/evaluators.rs`

**Interfaces:**
- Produces: `LlmJudge::new(model: Arc<dyn Model>)`, `.rubric(impl Into<String>)`, `.threshold(f64)` (default 0.7), name `"llm_judge"`; `ToolUseTrajectory::exact()`, `::in_order()`, `.include_handoffs()`, name `"tool_trajectory"`.
- Consumes: `Model::invoke` + `ModelRequest::new()` (pub fields), `ModelEvent::TokenDelta` aggregation; `AgentEvent::ToolCallItem { item: Item::ToolCall { name, .. } }`.

- [ ] **Step 1: Failing tests** (append to `tests/evaluators.rs`):

```rust
use std::sync::Arc;
use paigasus_helikon_core::{AgentEvent, FinishReason, Item, ModelEvent};
use paigasus_helikon_evals::{LlmJudge, MockModel, ToolUseTrajectory};

fn tool_call_event(name: &str) -> AgentEvent {
    AgentEvent::ToolCallItem {
        item: Item::ToolCall { call_id: "c".into(), name: name.into(), args: serde_json::json!({}) },
    }
}

#[tokio::test]
async fn llm_judge_parses_score_and_thresholds() {
    let judge_reply = r#"{"score": 0.9, "reasoning": "solid"}"#;
    let model = MockModel::with_script(vec![
        ModelEvent::TokenDelta { text: judge_reply.into() },
        ModelEvent::Finish { reason: FinishReason::Stop },
    ]);
    let judge = LlmJudge::new(model.clone() as Arc<dyn paigasus_helikon_core::Model>)
        .rubric("Is the answer helpful?");
    let s = judge.evaluate(&case(Some("ref".into())), &outcome("answer")).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
    assert!((s.value - 0.9).abs() < 1e-9);

    let model = MockModel::with_script(vec![
        ModelEvent::TokenDelta { text: r#"{"score": 0.2, "reasoning": "weak"}"#.into() },
        ModelEvent::Finish { reason: FinishReason::Stop },
    ]);
    let judge = LlmJudge::new(model as Arc<dyn paigasus_helikon_core::Model>).threshold(0.5);
    let s = judge.evaluate(&case(None), &outcome("answer")).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Failed));
}

#[tokio::test]
async fn trajectory_modes_and_handoff_filter() {
    let mut c = case(None);
    c.expected_tools = Some(vec!["lookup_spending".into(), "send_report".into()]);

    let mut o = outcome("x");
    o.events = vec![
        tool_call_event("transfer_to_budgeting"), // filtered by default
        tool_call_event("lookup_spending"),
        tool_call_event("send_report"),
    ];
    let s = ToolUseTrajectory::exact().evaluate(&c, &o).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));

    // in_order: extra tool between expected ones still passes
    let mut o2 = outcome("x");
    o2.events = vec![
        tool_call_event("lookup_spending"),
        tool_call_event("noise"),
        tool_call_event("send_report"),
    ];
    assert!(matches!(ToolUseTrajectory::exact().evaluate(&c, &o2).await.unwrap().outcome, ScoreOutcome::Failed));
    assert!(matches!(ToolUseTrajectory::in_order().evaluate(&c, &o2).await.unwrap().outcome, ScoreOutcome::Passed));

    // skip without expected_tools
    let s = ToolUseTrajectory::exact().evaluate(&case(None), &o).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Skipped));

    // include_handoffs keeps transfer tools
    let mut c2 = case(None);
    c2.expected_tools = Some(vec!["transfer_to_budgeting".into(), "lookup_spending".into(), "send_report".into()]);
    let s = ToolUseTrajectory::exact().include_handoffs().evaluate(&c2, &o).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));

    // empty expected_tools means "no tools expected"
    let mut c3 = case(None);
    c3.expected_tools = Some(vec![]);
    let s = ToolUseTrajectory::exact().evaluate(&c3, &outcome("x")).await.unwrap();
    assert!(matches!(s.outcome, ScoreOutcome::Passed));
}
```

- [ ] **Step 2: Run** → COMPILE FAIL.
- [ ] **Step 3: Implement.**

`llm_judge.rs` core logic (complete the file with docs):

```rust
/// Judges final responses with a model call against a rubric.
pub struct LlmJudge {
    model: Arc<dyn Model>,
    rubric: String,
    threshold: f64,
}

impl LlmJudge {
    /// Judge with `model`; default rubric asks for general answer quality,
    /// default threshold 0.7.
    pub fn new(model: Arc<dyn Model>) -> Self {
        Self {
            model,
            rubric: "Rate how well the answer addresses the input.".to_owned(),
            threshold: 0.7,
        }
    }
    /// Set the rubric shown to the judge model.
    pub fn rubric(mut self, r: impl Into<String>) -> Self { self.rubric = r.into(); self }
    /// Set the pass threshold (default 0.7).
    pub fn threshold(mut self, t: f64) -> Self { self.threshold = t; self }
}

#[async_trait]
impl Evaluator for LlmJudge {
    fn name(&self) -> &str { "llm_judge" }

    async fn evaluate(&self, case: &EvalCase, outcome: &CaseOutcome) -> Result<Score, EvalError> {
        let mut prompt = format!(
            "You are an impartial evaluation judge.\nRubric: {}\n\nInput:\n{}\n",
            self.rubric, case.input
        );
        if let Some(expected) = &case.expected {
            prompt.push_str(&format!("\nReference answer:\n{expected}\n"));
        }
        prompt.push_str(&format!(
            "\nActual answer:\n{}\n\nReply with ONLY a JSON object: {{\"score\": <0..1>, \"reasoning\": \"...\"}}",
            outcome.final_output
        ));

        let mut request = ModelRequest::new();
        request.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text { text: prompt }],
        }];

        let mut stream = self
            .model
            .invoke(request, CancellationToken::new())
            .await
            .map_err(|e| EvalError::Run(e.to_string()))?;
        let mut text = String::new();
        while let Some(ev) = stream.next().await {
            if let Ok(ModelEvent::TokenDelta { text: t }) = ev {
                text.push_str(&t);
            }
        }
        // Lenient extraction: first '{' … last '}'.
        let json_slice = match (text.find('{'), text.rfind('}')) {
            (Some(a), Some(b)) if b >= a => &text[a..=b],
            _ => return Ok(Score::failed(0.0, format!("judge returned no JSON: {text}"))),
        };
        #[derive(serde::Deserialize)]
        struct Verdict { score: f64, #[serde(default)] reasoning: Option<String> }
        let verdict: Verdict = match serde_json::from_str(json_slice) {
            Ok(v) => v,
            Err(e) => return Ok(Score::failed(0.0, format!("judge JSON parse error: {e}"))),
        };
        let value = verdict.score.clamp(0.0, 1.0);
        if value >= self.threshold {
            Ok(Score { value, outcome: ScoreOutcome::Passed, detail: verdict.reasoning })
        } else {
            Ok(Score { value, outcome: ScoreOutcome::Failed, detail: verdict.reasoning })
        }
    }
}
```

Wait — `Item::UserMessage` is `#[non_exhaustive]` in a foreign crate: **literal construction from evals will not compile.** Check `Item` for constructors (`Item::user(…)`, `From<&str>`) with `grep -n "impl Item" -A 30 crates/paigasus-helikon-core/src/item.rs`. If a ctor like `Item::user_text(...)`/`Item::user(...)` exists, use it. If none exists, build messages via `AgentInput::from_user_text(prompt).messages` (public, returns the right `Vec<Item>` — this IS available in registry core). Use whichever compiles; `AgentInput::from_user_text(prompt).messages` is the guaranteed-portable fallback.

`trajectory.rs`:

```rust
/// Compares the tool-call sequence against `expected_tools`.
pub struct ToolUseTrajectory {
    mode: Mode,
    include_handoffs: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode { Exact, InOrder }

impl ToolUseTrajectory {
    /// The observed sequence must equal `expected_tools` exactly.
    pub fn exact() -> Self { Self { mode: Mode::Exact, include_handoffs: false } }
    /// `expected_tools` must appear as an in-order subsequence.
    pub fn in_order() -> Self { Self { mode: Mode::InOrder, include_handoffs: false } }
    /// Keep `transfer_to_*` handoff tool calls in the observed sequence
    /// (filtered by default).
    pub fn include_handoffs(mut self) -> Self { self.include_handoffs = true; self }
}

#[async_trait]
impl Evaluator for ToolUseTrajectory {
    fn name(&self) -> &str { "tool_trajectory" }

    async fn evaluate(&self, case: &EvalCase, outcome: &CaseOutcome) -> Result<Score, EvalError> {
        let Some(expected) = &case.expected_tools else {
            return Ok(Score::skipped("no `expected_tools` on case"));
        };
        let actual: Vec<String> = outcome
            .events
            .iter()
            .filter_map(|ev| match ev {
                AgentEvent::ToolCallItem { item: Item::ToolCall { name, .. } } => Some(name.clone()),
                _ => None,
            })
            .filter(|n| self.include_handoffs || !n.starts_with("transfer_to_"))
            .collect();

        let (matched, denom) = match self.mode {
            Mode::Exact => {
                let matched = expected.iter().zip(&actual).filter(|(e, a)| e == a).count();
                (matched, expected.len().max(actual.len()))
            }
            Mode::InOrder => {
                let mut it = actual.iter();
                let matched = expected
                    .iter()
                    .filter(|e| it.by_ref().any(|a| &a == e))
                    .count();
                (matched, expected.len())
            }
        };
        let value = if denom == 0 { 1.0 } else { matched as f64 / denom as f64 };
        if (value - 1.0).abs() < f64::EPSILON {
            Ok(Score::passed(1.0))
        } else {
            Ok(Score::failed(value, format!("expected {expected:?}, observed {actual:?}")))
        }
    }
}
```

(`Item::ToolCall` pattern-matching on a `#[non_exhaustive]` foreign enum variant with `..` rest-pattern is allowed.)

- [ ] **Step 4: Run until green**, **Step 5: Commit** `feat(evals): SMA-333 add LlmJudge and ToolUseTrajectory evaluators`

### Task 9: `EvalRun` + `EvalReport`

**Files:**
- Create: `src/run.rs`
- Modify: `src/lib.rs` (`pub use run::{CaseResult, EvalReport, EvalRun, EvalRunBuilder, EvalSummary, EvaluatorScore, EvaluatorSummary, RunMeta};`)
- Test: `crates/paigasus-helikon-evals/tests/eval_run.rs`

**Interfaces:**
- Consumes: `TokioRunner` (from `paigasus_helikon_runtime_tokio`), `Runner`, `RunConfig`, `RunContext::ephemeral`, `AgentInput::from_user_text`, Task 5-8 types.
- Produces:
  - `EvalRun::builder() -> EvalRunBuilder<Ctx>` with `.dataset(EvalDataset)`, `.agent(impl Agent<Ctx> + 'static)`, `.shared_agent(Arc<dyn Agent<Ctx>>)`, `.agent_factory(impl Fn(&EvalCase) -> Arc<dyn Agent<Ctx>> + Send + Sync + 'static)`, `.ctx_factory(impl Fn() -> Ctx + Send + Sync + 'static)`, `.default_ctx()` (where `Ctx: Default`), `.evaluator(impl Evaluator + 'static)`, `.shared_evaluator(Arc<dyn Evaluator>)`, `.concurrency(usize)` (default 1), `.run_config(RunConfig)`, `.runner(Arc<dyn Runner<Ctx>>)` (default `TokioRunner`), `.trace(Arc<dyn TraceSink>)` — `.trace` arrives in Task 10; leave the field + setter in now with the trait declared in Task 10 — instead: declare a minimal `pub trait TraceSink` placeholder NOW in `src/trace.rs` with the exact Task 10 signature so the builder compiles.
  - `run().await -> Result<EvalReport, EvalError>`.
  - `RunMeta { run_id: String, dataset: String, started_ts_nanos: i64 }`; `EvaluatorScore { evaluator: String, score: Score }`; `CaseResult { case_id: String, outcome: Option<CaseOutcome>, error: Option<String>, scores: Vec<EvaluatorScore> }`; `EvaluatorSummary { mean: f64, passed: usize, failed: usize, skipped: usize }`; `EvalSummary { evaluators: BTreeMap<String, EvaluatorSummary>, cases_passed: usize, cases_failed: usize }`; `EvalReport { meta: RunMeta, results: Vec<CaseResult>, summary: EvalSummary }` with `passed() -> bool`, `render_table() -> String`, and `#[derive(serde::Serialize)]` on all report types (`CaseOutcome` events serialize via core's serde derives; add `#[serde(skip)]` on `CaseOutcome.events` if the report JSON gets too noisy — no: keep events IN the serialized report, the CLI `--json` consumer wants them; `CaseOutcome` needs `Serialize` derive added in Task 7 — add `#[derive(serde::Serialize)]` there).

- [ ] **Step 1: Failing test** — `tests/eval_run.rs`:

```rust
//! EvalRun end-to-end tests over MockModel agents.

use std::sync::Arc;

use paigasus_helikon_core::{Agent, FinishReason, LlmAgent, ModelEvent};
use paigasus_helikon_evals::{
    EvalDataset, EvalRun, ExactMatch, MockModel, ScoreOutcome, ToolUseTrajectory,
};

const DATASET: &str = r#"
{"id":"a","input":"question a","expected":"answer a"}
{"id":"b","input":"question b","expected":"answer b"}
{"id":"c","input":"question c","expected":"answer c"}
"#;

fn agent_for(case_id: &str) -> Arc<dyn Agent<()>> {
    let text = format!("answer {case_id}");
    Arc::new(
        LlmAgent::builder::<()>()
            .name("echo")
            .description("echoes per case")
            .shared_model(MockModel::with_script(vec![
                ModelEvent::TokenDelta { text },
                ModelEvent::Finish { reason: FinishReason::Stop },
            ]))
            .instructions("test")
            .build(),
    )
}

#[tokio::test]
async fn eval_run_is_deterministic_under_concurrency() {
    for _ in 0..3 {
        let report = EvalRun::builder()
            .dataset(EvalDataset::from_jsonl_str("t", DATASET).unwrap())
            .agent_factory(|case| agent_for(&case.id))
            .default_ctx()
            .evaluator(ExactMatch::new())
            .evaluator(ToolUseTrajectory::exact())
            .concurrency(4)
            .run()
            .await
            .unwrap();
        assert!(report.passed());
        assert_eq!(report.results.len(), 3);
        // report order matches dataset order regardless of concurrency
        assert_eq!(report.results[0].case_id, "a");
        assert_eq!(report.results[2].case_id, "c");
        // trajectory skipped (no expected_tools), exact_match passed
        let scores = &report.results[0].scores;
        assert!(scores.iter().any(|s| s.evaluator == "exact_match" && matches!(s.score.outcome, ScoreOutcome::Passed)));
        assert!(scores.iter().any(|s| s.evaluator == "tool_trajectory" && matches!(s.score.outcome, ScoreOutcome::Skipped)));
        let summary = &report.summary;
        assert_eq!(summary.evaluators["exact_match"].passed, 3);
        assert_eq!(summary.evaluators["tool_trajectory"].skipped, 3);
        assert_eq!(summary.cases_passed, 3);
    }
}

#[tokio::test]
async fn eval_run_failure_and_agent_error_reported() {
    // agent answers wrong for b; agent for c errors (no scripts).
    let report = EvalRun::builder()
        .dataset(EvalDataset::from_jsonl_str("t", DATASET).unwrap())
        .agent_factory(|case| match case.id.as_str() {
            "c" => Arc::new(
                LlmAgent::builder::<()>()
                    .name("broken").description("no scripts")
                    .shared_model(MockModel::with_scripts(vec![]))
                    .instructions("test").build(),
            ) as Arc<dyn Agent<()>>,
            id => agent_for(if id == "b" { "WRONG" } else { id }),
        })
        .default_ctx()
        .evaluator(ExactMatch::new())
        .run()
        .await
        .unwrap();
    assert!(!report.passed());
    assert!(report.results[1].scores.iter().any(|s| matches!(s.score.outcome, ScoreOutcome::Failed)));
    assert!(report.results[2].error.is_some());
    assert_eq!(report.summary.cases_failed, 2);
    let table = report.render_table();
    assert!(table.contains("exact_match"));
}
```

- [ ] **Step 2: Run** → COMPILE FAIL.
- [ ] **Step 3: Implement `src/run.rs`.** Key logic (write the full file with docs on every pub item):

```rust
enum AgentSource<Ctx> {
    Shared(Arc<dyn Agent<Ctx>>),
    Factory(Box<dyn Fn(&EvalCase) -> Arc<dyn Agent<Ctx>> + Send + Sync>),
}

impl<Ctx> AgentSource<Ctx> {
    fn agent_for(&self, case: &EvalCase) -> Arc<dyn Agent<Ctx>> {
        match self {
            Self::Shared(a) => Arc::clone(a),
            Self::Factory(f) => f(case),
        }
    }
}

// builder holds: dataset, source, ctx_factory: Option<Arc<dyn Fn() -> Ctx + Send + Sync>>,
// evaluators: Vec<Arc<dyn Evaluator>>, concurrency: usize (min 1), run_config: RunConfig,
// runner: Option<Arc<dyn Runner<Ctx>>>, trace: Option<Arc<dyn TraceSink>>.

pub async fn run(self) -> Result<EvalReport, EvalError> {
    let dataset = self.dataset.ok_or(EvalError::MissingDataset)?;
    let source = Arc::new(self.agent.ok_or(EvalError::MissingAgent)?);
    let ctx_factory = self.ctx_factory.ok_or(EvalError::MissingCtxFactory)?;
    let runner: Arc<dyn Runner<Ctx>> =
        self.runner.unwrap_or_else(|| Arc::new(paigasus_helikon_runtime_tokio::TokioRunner));
    let evaluators = Arc::new(self.evaluators);
    let run_config = self.run_config;
    let meta = RunMeta {
        run_id: uuid::Uuid::new_v4().to_string(),
        dataset: dataset.name.clone(),
        started_ts_nanos: jiff::Timestamp::now().as_nanosecond() as i64,
    };

    let mut results: Vec<(usize, CaseResult)> = futures_util::stream::iter(
        dataset.cases.into_iter().enumerate().map(|(idx, case)| {
            let source = Arc::clone(&source);
            let ctx_factory = Arc::clone(&ctx_factory);
            let runner = Arc::clone(&runner);
            let evaluators = Arc::clone(&evaluators);
            let config = run_config.clone();
            async move {
                let agent = source.agent_for(&case);
                let ctx = RunContext::ephemeral((ctx_factory)());
                let input = AgentInput::from_user_text(case.input.clone());
                let mut result = CaseResult {
                    case_id: case.id.clone(),
                    outcome: None,
                    error: None,
                    scores: Vec::new(),
                };
                match runner.run(agent.as_ref(), ctx, input, config).await {
                    Err(e) => result.error = Some(e.to_string()),
                    Ok(run_result) => {
                        let outcome = CaseOutcome {
                            final_output: run_result.final_output,
                            events: run_result.events,
                            usage: run_result.usage,
                        };
                        for ev in evaluators.iter() {
                            match ev.evaluate(&case, &outcome).await {
                                Ok(score) => result.scores.push(EvaluatorScore {
                                    evaluator: ev.name().to_owned(),
                                    score,
                                }),
                                Err(e) => result.scores.push(EvaluatorScore {
                                    evaluator: ev.name().to_owned(),
                                    score: Score::failed(0.0, format!("evaluator error: {e}")),
                                }),
                            }
                        }
                        result.outcome = Some(outcome);
                    }
                }
                (idx, result)
            }
        }),
    )
    .buffer_unordered(self.concurrency.max(1))
    .collect()
    .await;
    results.sort_by_key(|(idx, _)| *idx);
    let results: Vec<CaseResult> = results.into_iter().map(|(_, r)| r).collect();

    if let Some(trace) = &self.trace {
        for case in &results {
            trace.record_case(&meta, case).await.map_err(|e| EvalError::Other(e.into()))?;
        }
        trace.finish().await.map_err(|e| EvalError::Other(e.into()))?;
    }

    let summary = summarize(&results);
    Ok(EvalReport { meta, results, summary })
}
```

`summarize`: per evaluator name accumulate mean over non-skipped values, passed/failed/skipped counts; case passed = `error.is_none()` && no `Failed` score; `EvalReport::passed()` = `summary.cases_failed == 0`. `render_table()`: one line per case (`case_id`, then `evaluator=value(outcome)` pairs), then a summary block per evaluator (`mean=…, passed=…, failed=…, skipped=…`) and a final `cases: N passed, M failed` line — plain `format!`-built text, no table dep.

`src/trace.rs` minimal now (Task 10 fills the sinks):

```rust
//! Trace sinks for offline analysis.

use async_trait::async_trait;

use crate::{CaseResult, RunMeta};

/// Errors from trace sinks.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TraceError {
    /// Backend I/O or storage failure.
    #[error("trace backend error: {0}")]
    Backend(String),
}

/// Receives each case's result during an eval run.
#[async_trait]
pub trait TraceSink: Send + Sync {
    /// Record one case (called once per case, after its evaluators ran).
    async fn record_case(&self, run: &RunMeta, case: &CaseResult) -> Result<(), TraceError>;
    /// Flush and close the sink (called once, after all cases).
    async fn finish(&self) -> Result<(), TraceError>;
}
```

(`EvalError::Other(e.into())` needs `TraceError: Into<anyhow::Error>` — `thiserror` errors satisfy `Into<anyhow::Error>` via `anyhow::Error::new`; write `EvalError::Other(anyhow::Error::new(e))`.)

`uuid` is already in `[workspace.dependencies]` with `v4`.

- [ ] **Step 4: Run until green** (`cargo test -p paigasus-helikon-evals`), **Step 5: Commit** `feat(evals): SMA-333 add EvalRun orchestration and EvalReport`

### Task 10: Trace sinks (SQLite + Parquet)

**Files:**
- Modify: root `Cargo.toml` (`arrow`, `parquet` workspace deps)
- Create: `src/trace/sqlite.rs`, `src/trace/parquet.rs` (convert `src/trace.rs` into `src/trace/mod.rs`), `crates/paigasus-helikon-evals/migrations/0001_eval_traces.sql`
- Test: `crates/paigasus-helikon-evals/tests/trace.rs`

**Interfaces:**
- Produces: `SqliteTraceSink::open(path: &Path) -> Result<Self, TraceError>` (async; connects `sqlite://…?mode=rwc`, runs embedded migration) behind feature `trace-sqlite`; `ParquetTraceSink::new(dir: &Path) -> Result<Self, TraceError>` behind `trace-parquet`, buffering rows and writing `<run_id>-events.parquet` + `<run_id>-scores.parquet` on `finish()`.
- Consumes: `SessionRecorder` (core) to derive `SessionEvent`s from `CaseOutcome.events`.

- [ ] **Step 1: Workspace deps.** Resolve current latest arrow/parquet (`cargo info arrow parquet` or docs.rs) whose MSRV ≤ 1.94, then add to root `[workspace.dependencies]` (majors below are the mid-2026 expectation — bump to whatever is current):

```toml
arrow                 = { version = "56", default-features = false }
parquet               = { version = "56", default-features = false, features = ["arrow", "snap"] }
```

Verify: `cargo +1.94 check -p paigasus-helikon-evals --features trace-parquet` (install the 1.94 toolchain if missing: `rustup toolchain install 1.94`). If the newest major demands > 1.94, step down one major and note it in the commit body.

- [ ] **Step 2: Migration** — `migrations/0001_eval_traces.sql`:

```sql
CREATE TABLE eval_runs (
    run_id           TEXT PRIMARY KEY,
    dataset          TEXT NOT NULL,
    started_ts_nanos INTEGER NOT NULL
);

CREATE TABLE eval_cases (
    run_id       TEXT NOT NULL,
    case_id      TEXT NOT NULL,
    final_output TEXT NOT NULL,
    error        TEXT,
    scores       TEXT NOT NULL, -- JSON: [{evaluator, score:{value, outcome, detail}}]
    PRIMARY KEY (run_id, case_id)
);

CREATE TABLE eval_events (
    run_id   TEXT NOT NULL,
    case_id  TEXT NOT NULL,
    seq      INTEGER NOT NULL,
    kind     TEXT NOT NULL,
    ts_nanos INTEGER NOT NULL,
    payload  TEXT NOT NULL, -- SessionEvent JSON
    PRIMARY KEY (run_id, case_id, seq)
);
```

- [ ] **Step 3: Failing tests** — `tests/trace.rs` (gate the whole file: `#![cfg(feature = "trace-sqlite")]`, with a second `#[cfg(feature = "trace-parquet")]` module inside):

```rust
#![cfg(feature = "trace-sqlite")]
//! Trace sink round-trip tests.

use paigasus_helikon_evals::{CaseResult, RunMeta, SqliteTraceSink, TraceSink};

fn meta() -> RunMeta {
    RunMeta { run_id: "r1".into(), dataset: "d".into(), started_ts_nanos: 42 }
}

fn case_result() -> CaseResult {
    // build via the public structs; a CaseOutcome with one MessageOutput
    // event so eval_events gets at least one row
    todo!("construct with Score::passed etc. — all fields are pub")
}

#[tokio::test]
async fn sqlite_sink_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("trace.db");
    let sink = SqliteTraceSink::open(&db).await.unwrap();
    sink.record_case(&meta(), &case_result()).await.unwrap();
    sink.finish().await.unwrap();

    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", db.display())).await.unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM eval_cases").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM eval_events").fetch_one(&pool).await.unwrap();
    assert!(n >= 1);
}

#[cfg(feature = "trace-parquet")]
mod parquet_sink {
    use super::*;
    use paigasus_helikon_evals::ParquetTraceSink;

    #[tokio::test]
    async fn parquet_sink_writes_readable_files() {
        let dir = tempfile::tempdir().unwrap();
        let sink = ParquetTraceSink::new(dir.path()).unwrap();
        sink.record_case(&meta(), &case_result()).await.unwrap();
        sink.finish().await.unwrap();
        let events = dir.path().join("r1-events.parquet");
        let scores = dir.path().join("r1-scores.parquet");
        assert!(events.exists() && scores.exists());
        // read back with parquet's arrow reader; assert ≥1 row each
        let file = std::fs::File::open(&events).unwrap();
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap().build().unwrap();
        let rows: usize = reader.map(|b| b.unwrap().num_rows()).sum();
        assert!(rows >= 1);
    }
}
```

Replace the `todo!` with a real constructor before running: build `CaseOutcome { final_output: "hi".into(), events: vec![AgentEvent::MessageOutput { item: /* via serde: serde_json::from_value on the AgentEvent JSON shape */ }], usage: TokenUsage::default() }` — simplest portable construction of an `AgentEvent` outside core: `serde_json::from_value::<AgentEvent>(serde_json::json!({"type":"message_output","item":{"type":"assistant_message","content":[{"type":"text","text":"hi"}],"agent":"a"}})).unwrap()` (AgentEvent + Item derive serde in core; check `Item`'s serde tag with one glance at item.rs and adjust the JSON to match — `#[serde(tag="type", rename_all="snake_case")]` per core convention).

Add `sqlx`/`parquet`/`arrow` + `tempfile` to evals `[dev-dependencies]`? No — dev-deps: `tempfile` already there; sqlx/parquet come via the features under test. Run with: `cargo test -p paigasus-helikon-evals --features trace-sqlite,trace-parquet`.

- [ ] **Step 4: Implement.** `src/trace/sqlite.rs` — `pub struct SqliteTraceSink { pool: sqlx::SqlitePool }`; `open`: `SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))` then `sqlx::migrate!("./migrations").run(&pool)`, mapping errors to `TraceError::Backend(e.to_string())`. `record_case`: `INSERT OR IGNORE INTO eval_runs`, `INSERT INTO eval_cases` (scores as `serde_json::to_string(&case.scores)`), then derive events: `let mut rec = SessionRecorder::new("eval"); if let Some(outcome) = &case.outcome { for ev in &outcome.events { rec.observe(ev); } }` and insert each drained `SessionEvent` with `seq = i`, `kind = ev.kind()`, `ts_nanos = ev.ts_nanos_saturating()`, `payload = serde_json::to_string(ev)` (mirror `sessions-sqlite`'s `event_metadata` approach — check its helper at `crates/paigasus-helikon-sessions-sqlite/src/lib.rs` and copy the kind/ts extraction shape). `finish`: no-op `Ok(())` (pool drops).
`src/trace/parquet.rs` — buffer `Mutex<Vec<EventRow>>`/`Mutex<Vec<ScoreRow>>`; `finish()` builds two `arrow::record_batch::RecordBatch`es from `StringArray`/`Int64Array`/`Float64Array` columns (events: run_id, case_id, seq, kind, ts_nanos, payload; scores: run_id, case_id, evaluator, value, outcome, detail) and writes each with `parquet::arrow::ArrowWriter::try_new(File::create(path)?, schema, None)` + `.write(&batch)` + `.close()`. Feature-gate both modules in `trace/mod.rs`:

```rust
#[cfg(feature = "trace-sqlite")]
mod sqlite;
#[cfg(feature = "trace-sqlite")]
pub use sqlite::SqliteTraceSink;
#[cfg(feature = "trace-parquet")]
mod parquet;
#[cfg(feature = "trace-parquet")]
pub use parquet::ParquetTraceSink;
```

- [ ] **Step 5: Run until green**: `cargo test -p paigasus-helikon-evals --features trace-sqlite,trace-parquet`, plus feature matrix sanity: `cargo check -p paigasus-helikon-evals` (no features) and `cargo check -p paigasus-helikon-evals --all-features`.
- [ ] **Step 6: Commit** `feat(evals): SMA-333 add sqlite and parquet trace sinks`

---

## Task Group C — `paigasus-helikon-cli`

### Task 11: CLI scaffolding (clap tree + thin bins)

**Files:**
- Modify: root `Cargo.toml` (add `clap`, `toml`, `rhai`, `notify`, `notify-debouncer-mini` to `[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon-cli/Cargo.toml` (deps; keep `version 0.0.0`/`publish=false` until Task 19)
- Create: `crates/paigasus-helikon-cli/src/lib.rs`, `src/cli.rs`
- Modify: `src/bin/helikon.rs`, `src/bin/paigasus_helikon.rs`
- Test: `crates/paigasus-helikon-cli/tests/cli_smoke.rs`

**Interfaces:**
- Produces: `paigasus_helikon_cli::main() -> std::process::ExitCode` (both bins call it); clap tree: `helikon repl [--agents <path>] [--agent <name>]`, `helikon eval run <dataset> --agent <name> [--agents <path>] [--json] [--fail-under <f64>] [--trace <spec>]`, `helikon mcp serve --agent <name> [--agents <path>] [--http <addr>]`; `--agents` defaults to `./agents.toml`.

- [ ] **Step 1: Workspace deps** (root `Cargo.toml`, resolve latest stable majors, MSRV ≤ 1.94):

```toml
clap                  = { version = "4", features = ["derive"] }
toml                  = "0.9"
rhai                  = { version = "1", features = ["sync", "serde"] }
notify                = "8"
notify-debouncer-mini = "0.7"
```

(Check each with `cargo info <name>`; use the newest major. `notify`/`notify-debouncer-mini` are CC0-1.0 — deny.toml gets the allowlist entry in Task 19; `cargo deny` may go red locally until then, that's expected.)

- [ ] **Step 2: CLI Cargo.toml** — replace `[package]`-adjacent sections, keeping `autobins = false`, both `[[bin]]`s, `version = "0.0.0"`, `publish = false`, and the `[lints.rust] missing_docs = "allow"` block; add:

```toml
[dependencies]
paigasus-helikon-core                = { workspace = true }
paigasus-helikon-evals               = { workspace = true, features = ["trace-sqlite"] }
paigasus-helikon-runtime-tokio       = { workspace = true }
paigasus-helikon-providers-openai    = { workspace = true }
paigasus-helikon-providers-anthropic = { workspace = true }
paigasus-helikon-mcp                 = { workspace = true }
anyhow      = { workspace = true }
async-trait = { workspace = true }
clap        = { workspace = true }
futures-util = { workspace = true }
futures-core = { workspace = true }
notify      = { workspace = true }
notify-debouncer-mini = { workspace = true }
rhai        = { workspace = true }
serde       = { workspace = true }
serde_json  = { workspace = true }
tokio       = { workspace = true }
toml        = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 3: lib + bins.** `src/cli.rs`:

```rust
//! Clap command tree for the `helikon` binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Paigasus Helikon agent CLI.
#[derive(Debug, Parser)]
#[command(name = "helikon", version, about = "Paigasus Helikon agent CLI")]
pub struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Interactive REPL with hot-reloading agent definitions.
    Repl(ReplArgs),
    /// Evaluation commands.
    Eval {
        /// Eval subcommand.
        #[command(subcommand)]
        command: EvalCommand,
    },
    /// MCP server commands.
    Mcp {
        /// MCP subcommand.
        #[command(subcommand)]
        command: McpCommand,
    },
}

/// Arguments for `helikon repl`.
#[derive(Debug, clap::Args)]
pub struct ReplArgs {
    /// Path to the agents sidecar file.
    #[arg(long, default_value = "agents.toml")]
    pub agents: PathBuf,
    /// Agent to talk to first (default: first agent in the file).
    #[arg(long)]
    pub agent: Option<String>,
}

/// `helikon eval …` subcommands.
#[derive(Debug, Subcommand)]
pub enum EvalCommand {
    /// Run a JSONL dataset against an agent and print scores.
    Run(EvalRunArgs),
}

/// Arguments for `helikon eval run`.
#[derive(Debug, clap::Args)]
pub struct EvalRunArgs {
    /// Path to the JSONL dataset.
    pub dataset: PathBuf,
    /// Agent name from the sidecar file.
    #[arg(long)]
    pub agent: String,
    /// Path to the agents sidecar file.
    #[arg(long, default_value = "agents.toml")]
    pub agents: PathBuf,
    /// Emit the full report as JSON instead of a table.
    #[arg(long)]
    pub json: bool,
    /// Fail (exit 1) if the mean non-skipped score is below this value.
    #[arg(long)]
    pub fail_under: Option<f64>,
    /// Trace sink, e.g. `sqlite:traces.db`.
    #[arg(long)]
    pub trace: Option<String>,
}

/// `helikon mcp …` subcommands.
#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// Serve a sidecar agent as an MCP server (stdio by default).
    Serve(McpServeArgs),
}

/// Arguments for `helikon mcp serve`.
#[derive(Debug, clap::Args)]
pub struct McpServeArgs {
    /// Agent name from the sidecar file.
    #[arg(long)]
    pub agent: String,
    /// Path to the agents sidecar file.
    #[arg(long, default_value = "agents.toml")]
    pub agents: PathBuf,
    /// Serve over streamable HTTP on this address instead of stdio.
    #[arg(long)]
    pub http: Option<String>,
}
```

`src/lib.rs`:

```rust
//! Internal implementation of the `helikon` / `paigasus-helikon` CLI.
//!
//! **Internal — no stability guarantees.** This library target exists so
//! the two binaries can share code; its API may change in any release.

pub mod cli;

use clap::Parser as _;
use std::process::ExitCode;

/// Entry point shared by both binaries.
pub fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("failed to start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: cli::Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        cli::Command::Repl(_args) => anyhow::bail!("repl: implemented in a later task"),
        cli::Command::Eval { .. } => anyhow::bail!("eval: implemented in a later task"),
        cli::Command::Mcp { .. } => anyhow::bail!("mcp: implemented in a later task"),
    }
}
```

Both bins become:

```rust
fn main() -> std::process::ExitCode {
    paigasus_helikon_cli::main()
}
```

- [ ] **Step 4: Smoke test** — `tests/cli_smoke.rs`:

```rust
//! Binary smoke tests.

#[test]
fn help_lists_subcommands() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_helikon"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for cmd in ["repl", "eval", "mcp"] {
        assert!(stdout.contains(cmd), "missing {cmd} in help");
    }
}

#[test]
fn shim_binary_works_too() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_paigasus-helikon"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
}
```

Run: `cargo test -p paigasus-helikon-cli` → PASS.

- [ ] **Step 5: Commit** `feat(cli): SMA-333 add clap scaffolding and shared binary entry point`

### Task 12: Sidecar TOML parsing + validation

**Files:**
- Create: `src/sidecar.rs`
- Modify: `src/lib.rs` (`pub mod sidecar;`)
- Test: `crates/paigasus-helikon-cli/tests/sidecar.rs`

**Interfaces:**
- Produces:

```rust
pub struct Sidecar { pub agents: BTreeMap<String, AgentDef>, pub tools: BTreeMap<String, ToolDefToml>, pub eval: Option<EvalSection>, pub base_dir: PathBuf }
impl Sidecar {
    pub fn load(path: &Path) -> anyhow::Result<Self>;      // parse + validate; base_dir = path.parent()
    pub fn parse(text: &str, base_dir: &Path) -> anyhow::Result<Self>;
    pub fn first_agent(&self) -> Option<&str>;
}
pub struct AgentDef { pub description: Option<String>, pub instructions: InstructionsDef, pub model: ModelDef, pub max_turns: Option<u32>, pub tools: Vec<String>, pub handoffs: Vec<String> }
pub enum InstructionsDef { Inline(String), File { file: PathBuf } }        // #[serde(untagged)]
pub enum ModelDef { Openai { id: String }, Anthropic { id: String }, Mock { script: PathBuf } }  // #[serde(tag = "provider", rename_all = "lowercase")]
pub struct ToolDefToml { pub description: String, pub params: serde_json::Value, pub script: Option<PathBuf>, pub inline: Option<String> }
pub struct EvalSection { pub evaluators: Vec<String>, pub json_schema: Option<JsonSchemaCfg>, pub llm_judge: Option<LlmJudgeCfg> }
pub struct JsonSchemaCfg { pub schema: PathBuf }
pub struct LlmJudgeCfg { pub model: ModelDef, pub rubric: Option<String>, pub threshold: Option<f64> }
```

Validation rules (all produce `anyhow::bail!` with the offending name): every `agents.*.tools` entry exists in `[tools]`; every `agents.*.handoffs` entry exists in `[agents]` and is not the agent itself; **handoff declarations must be acyclic** (DFS over the handoff references; on a cycle, error `handoff cycle detected involving '<name>' — declare one-way handoff chains`); every tool has exactly one of `script`/`inline`; `params` must be a JSON object (`toml::Value` → `serde_json::Value` conversion via `serde_json::to_value(&toml_value)`); evaluator names ∈ {`exact_match`, `json_schema`, `llm_judge`, `tool_trajectory`} and their config sections present when named (`json_schema` needs `[eval.json_schema]`, `llm_judge` needs `[eval.llm_judge]`).

Note (spec deviation, agreed at plan time): the spec's §5.2 parenthetical tolerated cyclic handoffs as acceptable `Arc` leaks; building them with `Handoff::to` value semantics would actually require slot machinery the CLI can't reuse (registry-verify rule forbids the CLI touching new core API), so cycles are **rejected at validation** with a clear error instead — strictly safer. The docs task records this.

- [ ] **Step 1: Failing tests** — `tests/sidecar.rs` with a known-good fixture string (an `[agents.triage]` with mock model + one inline Rhai tool + `[eval]`) asserting parsed fields; plus one test per validation rule (unknown tool ref, unknown handoff ref, self-handoff, handoff cycle `a→b→a`, tool with both `script` and `inline`, unknown evaluator name). Full known-good fixture:

```toml
[agents.triage]
description  = "Routes personal-finance questions"
instructions = "Route the question."
model        = { provider = "mock", script = "triage_script.json" }
max_turns    = 8
tools        = ["lookup_spending"]
handoffs     = ["budgeting"]

[agents.budgeting]
instructions = "Answer budget questions."
model        = { provider = "openai", id = "gpt-5-mini" }

[tools.lookup_spending]
description = "Look up spending for a month"
params      = { type = "object", properties = { month = { type = "string" } }, required = ["month"] }
inline      = "fn run(args) { #{ month: args.month, total: 1250 } }"

[eval]
evaluators = ["exact_match", "tool_trajectory"]
```

- [ ] **Step 2: Run** → COMPILE FAIL. **Step 3: Implement** `sidecar.rs` (serde structs mirroring the Interfaces block; `toml::from_str::<RawSidecar>` then a `validate()` pass; `params` arrives as `toml::Value` in a raw struct and converts with `serde_json::to_value`). **Step 4: green.** **Step 5: Commit** `feat(cli): SMA-333 add agents.toml sidecar parser and validation`

### Task 13: `CliModel` + `RhaiTool`

**Files:**
- Create: `src/model.rs`, `src/rhai_tool.rs`
- Modify: `src/lib.rs`
- Test: `crates/paigasus-helikon-cli/tests/rhai_tool.rs`

**Interfaces:**
- Produces:
  - `pub enum CliModel { OpenAi(OpenAiModel), Anthropic(AnthropicModel), Mock(Arc<MockModel>) }` + `impl Model for CliModel` delegating `invoke`/`capabilities`/`provider`/`model` by match; `pub fn build_model(def: &ModelDef, base_dir: &Path) -> anyhow::Result<CliModel>` — `Openai → OpenAiModel::chat(id).build()?`, `Anthropic → AnthropicModel::messages(id).build()?`, `Mock → MockModel::from_script_file(&base_dir.join(script))?`; and `pub fn build_model_for_case(def, base_dir, case_id) -> anyhow::Result<CliModel>` — mock variant loads `ScriptFile` and uses `scripts_for(case_id)` via `MockModel::with_scripts`.
  - `RhaiTool::new(name, description, params_schema: serde_json::Value, source: &str) -> anyhow::Result<Self>`; `impl Tool<()> for RhaiTool` — compile-once `Arc<Engine>`+`Arc<AST>` (`engine.set_max_operations(1_000_000)`), invoke via `tokio::task::spawn_blocking`, JSON↔`Dynamic` via `rhai::serde::{to_dynamic, from_dynamic}`, calling `fn run(args)`; script errors → `ToolError::Other`, never panics.

- [ ] **Step 1: Failing tests** — `tests/rhai_tool.rs`:

```rust
//! RhaiTool execution tests.

use paigasus_helikon_cli::rhai_tool::RhaiTool;
use paigasus_helikon_core::{RunContext, Tool};

fn tool(source: &str) -> RhaiTool {
    RhaiTool::new(
        "t", "test tool",
        serde_json::json!({"type":"object"}),
        source,
    ).unwrap()
}

#[tokio::test]
async fn runs_script_and_maps_json() {
    let t = tool("fn run(args) { #{ doubled: args.n * 2 } }");
    let ctx: RunContext<()> = RunContext::ephemeral(());
    let out = t.invoke(&ctx.to_tool_context(), serde_json::json!({"n": 21})).await.unwrap();
    assert_eq!(out.content, serde_json::json!({"doubled": 42}));
}

#[tokio::test]
async fn script_error_is_tool_error_not_panic() {
    let t = tool("fn run(args) { missing_fn() }");
    let ctx: RunContext<()> = RunContext::ephemeral(());
    assert!(t.invoke(&ctx.to_tool_context(), serde_json::json!({})).await.is_err());
}

#[tokio::test]
async fn operation_limit_stops_runaway_scripts() {
    let t = tool("fn run(args) { let x = 0; loop { x += 1; } }");
    let ctx: RunContext<()> = RunContext::ephemeral(());
    assert!(t.invoke(&ctx.to_tool_context(), serde_json::json!({})).await.is_err());
}

#[test]
fn compile_error_surfaces_at_construction() {
    assert!(RhaiTool::new("t", "d", serde_json::json!({}), "fn run( {").is_err());
}
```

(`ctx.to_tool_context()` is the public projection — exists on `RunContext`.)

- [ ] **Step 2: Run** → COMPILE FAIL. **Step 3: Implement** both files per the Interfaces block; `RhaiTool::invoke` body:

```rust
async fn invoke(&self, _ctx: &ToolContext<()>, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
    let engine = Arc::clone(&self.engine);
    let ast = Arc::clone(&self.ast);
    let result = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        let dyn_args = rhai::serde::to_dynamic(&args).map_err(|e| e.to_string())?;
        let mut scope = rhai::Scope::new();
        let out: rhai::Dynamic = engine
            .call_fn(&mut scope, &ast, "run", (dyn_args,))
            .map_err(|e| e.to_string())?;
        rhai::serde::from_dynamic(&out).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| ToolError::Other(anyhow::anyhow!("rhai task join error: {e}")))?
    .map_err(|e| ToolError::Other(anyhow::anyhow!("rhai tool '{}' failed: {e}", self.name)))?;
    Ok(ToolOutput::new(result))
}
```

`Tool` impl also returns `fn schema(&self) -> &serde_json::Value { &self.schema }`. Make `pub mod rhai_tool; pub mod model;` in lib.rs.

- [ ] **Step 4: green** (`cargo test -p paigasus-helikon-cli`). **Step 5: Commit** `feat(cli): SMA-333 add CliModel provider enum and sandboxed RhaiTool`

### Task 14: `AgentRegistry` + hot reload

**Files:**
- Create: `src/registry.rs`
- Modify: `src/lib.rs`
- Test: `crates/paigasus-helikon-cli/tests/registry.rs`

**Interfaces:**
- Produces:

```rust
pub struct AgentRegistry { /* path: PathBuf, inner: RwLock<Sidecar> */ }
impl AgentRegistry {
    pub fn load(path: &Path) -> anyhow::Result<Self>;
    pub fn reload(&self) -> anyhow::Result<()>;          // re-parse; on error KEEP old defs and return Err
    pub fn agent_names(&self) -> Vec<String>;
    pub fn has_agent(&self, name: &str) -> bool;
    pub fn build_agent(&self, name: &str) -> anyhow::Result<LlmAgent<(), CliModel>>;
    pub fn build_agent_for_case(&self, name: &str, case_id: &str) -> anyhow::Result<LlmAgent<(), CliModel>>;
    pub fn eval_section(&self) -> Option<EvalSection>;   // cloned
    pub fn watch(self: &Arc<Self>, on_reload: impl Fn(anyhow::Result<()>) + Send + 'static) -> anyhow::Result<Debouncer<RecommendedWatcher>>;  // notify-debouncer-mini, 300ms, watches base_dir recursively; caller keeps the returned debouncer alive
}
```

`build_agent` walks handoffs recursively (guaranteed acyclic by Task 12 validation): builds handoff target agents first (each with their own tools/model), wraps with `Handoff::to(target)`, then builds the named agent with `.tools(...)` (each TOML tool → `Arc::new(RhaiTool::new(…)?) as Arc<dyn Tool<()>>`), `.handoffs([...])`, `.max_turns(n)` when set, instructions from inline string or `std::fs::read_to_string(base_dir.join(file))?`.

- [ ] **Step 1: Failing tests** — `tests/registry.rs`: (a) `load` + `build_agent("triage")` from a tempdir sidecar (mock model + inline tool + one handoff) — asserts `agent.name == "triage"` (LlmAgent has pub `name` field); (b) **the hot-reload AC test**: load, assert instructions text via `agent.instructions.render(&RunContext::ephemeral(()))` contains "Route", rewrite the file with new instructions "Escalate", call `registry.reload().unwrap()`, rebuild, assert render contains "Escalate"; (c) reload with broken TOML → `Err` AND old defs still build; (d) watcher smoke test: `Arc<AgentRegistry>::watch` with an `mpsc` channel in the callback, rewrite the file, `recv_timeout(Duration::from_secs(10))` gets a reload notification (`#[test]`, std threads — no tokio needed for the watcher itself; mark it `#[cfg_attr(windows, ignore = "FS watcher latency is flaky on CI")]` only if it proves flaky locally — try unconditional first).

- [ ] **Step 2: Run** → COMPILE FAIL. **Step 3: Implement.** Reload keeps old state on error:

```rust
pub fn reload(&self) -> anyhow::Result<()> {
    let fresh = Sidecar::load(&self.path)?;   // error → early return, lock untouched
    *self.inner.write().unwrap_or_else(|e| e.into_inner()) = fresh;
    Ok(())
}
```

`watch`: `notify_debouncer_mini::new_debouncer(Duration::from_millis(300), move |res| { if res.is_ok() { let outcome = registry.reload(); on_reload(outcome); } })`, then `debouncer.watcher().watch(&base_dir, RecursiveMode::Recursive)`. (Clone an `Arc<Self>` into the closure — that's why `watch` takes `self: &Arc<Self>`.)

- [ ] **Step 4: green.** **Step 5: Commit** `feat(cli): SMA-333 add AgentRegistry with hot reload`

### Task 15: `eval run` command + AC1 fixtures

**Files:**
- Create: `src/eval_cmd.rs`, `tests/fixtures/agents.toml`, `tests/fixtures/triage_script.json`, `tests/fixtures/triage.jsonl`, `tests/fixtures/tools/lookup_spending.rhai`
- Modify: `src/lib.rs` (route `Command::Eval` to `eval_cmd::run`)
- Test: `crates/paigasus-helikon-cli/tests/eval_cli.rs`

**Interfaces:**
- Consumes: everything from Tasks 5-14.
- Produces: `pub async fn eval_cmd::run(args: EvalRunArgs) -> anyhow::Result<ExitCode>`. Behavior: registry load → dataset load → evaluators from `[eval]` (exact_match → `ExactMatch::new()`; tool_trajectory → `ToolUseTrajectory::exact()`; json_schema → `JsonSchemaConformance::new(read schema file)`; llm_judge → `LlmJudge::new(Arc::new(build_model(cfg.model)?) as Arc<dyn Model>)` + rubric/threshold) → `EvalRun::builder().dataset(…).agent_factory(|case| Arc::new(registry.build_agent_for_case(&name, &case.id).expect("validated sidecar")) as Arc<dyn Agent<()>>).default_ctx().evaluators…` — mock providers get per-case scripts through `build_agent_for_case`; non-mock defs return the same agent each call (factory is still fine). `--trace sqlite:<path>` → `SqliteTraceSink::open`. Print `render_table()` or `serde_json::to_string_pretty(&report)`. Exit code: `ExitCode::SUCCESS` iff `report.passed()` and (when `--fail-under` set) mean non-skipped score ≥ threshold.

- [ ] **Step 1: Fixtures.** `tests/fixtures/agents.toml`:

```toml
[agents.triage]
description  = "Routes personal-finance questions"
instructions = "Answer using the spending lookup tool when asked about spending."
model        = { provider = "mock", script = "triage_script.json" }
tools        = ["lookup_spending"]

[tools.lookup_spending]
description = "Look up spending for a month"
params      = { type = "object", properties = { month = { type = "string" } }, required = ["month"] }
script      = "tools/lookup_spending.rhai"

[eval]
evaluators = ["exact_match", "tool_trajectory"]
```

`tests/fixtures/tools/lookup_spending.rhai`:

```rhai
fn run(args) {
    #{ month: args.month, total: 1250 }
}
```

`tests/fixtures/triage_script.json` (two cases; the tool-using case takes two invokes — tool call turn, then final answer):

```json
{
  "default": [],
  "cases": {
    "spending-question": [
      [
        {"type":"tool_call_delta","call_id":"call-1","name":"lookup_spending","args_delta":"{\"month\":\"June\"}"},
        {"type":"finish","reason":"tool_calls"}
      ],
      [
        {"type":"token_delta","text":"You spent $1,250 in June."},
        {"type":"usage","input_tokens":10,"output_tokens":8},
        {"type":"finish","reason":"stop"}
      ]
    ],
    "greeting": [
      [
        {"type":"token_delta","text":"Hello! Ask me about your spending."},
        {"type":"finish","reason":"stop"}
      ]
    ]
  }
}
```

`tests/fixtures/triage.jsonl`:

```jsonl
{"id":"spending-question","input":"How much did I spend in June?","expected":"You spent $1,250 in June.","expected_tools":["lookup_spending"]}
{"id":"greeting","input":"Hi!","expected":"Hello! Ask me about your spending.","expected_tools":[]}
```

- [ ] **Step 2: Failing integration test** — `tests/eval_cli.rs`:

```rust
//! AC1: `helikon eval run triage.jsonl --agent triage` produces
//! trajectory + final-response scores in CI (mock provider, cwd at the
//! fixture dir so the `./agents.toml` default engages).

use std::path::Path;
use std::process::Command;

fn fixtures() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"))
}

#[test]
fn eval_run_scores_pass_and_exit_zero() {
    let out = Command::new(env!("CARGO_BIN_EXE_helikon"))
        .current_dir(fixtures())
        .args(["eval", "run", "triage.jsonl", "--agent", "triage"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("exact_match"), "final-response scores present:\n{stdout}");
    assert!(stdout.contains("tool_trajectory"), "trajectory scores present:\n{stdout}");
    assert!(stdout.contains("2 passed"), "summary present:\n{stdout}");
}

#[test]
fn eval_run_wrong_expectation_exits_nonzero() {
    // same fixtures, but a dataset expecting the wrong answer
    let dir = tempfile::tempdir().unwrap();
    let dataset = dir.path().join("bad.jsonl");
    std::fs::write(&dataset, r#"{"id":"greeting","input":"Hi!","expected":"WRONG"}"#).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_helikon"))
        .current_dir(fixtures())
        .args(["eval", "run"])
        .arg(&dataset)
        .args(["--agent", "triage"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn eval_run_json_output_parses() {
    let out = Command::new(env!("CARGO_BIN_EXE_helikon"))
        .current_dir(fixtures())
        .args(["eval", "run", "triage.jsonl", "--agent", "triage", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["summary"]["cases_passed"], 2);
}
```

- [ ] **Step 3: Run** → fails (eval bails). **Step 4: Implement `eval_cmd.rs`** per Interfaces; wire in `lib.rs::run`. Mean-score helper for `--fail-under`: mean of all non-skipped score values across the report. **Step 5: green.** **Step 6: Commit** `feat(cli): SMA-333 add eval run command with CI fixtures`

### Task 16: `repl` + `mcp serve`

**Files:**
- Create: `src/repl.rs`, `src/mcp_cmd.rs`
- Modify: `src/lib.rs` (route both)
- Test: `crates/paigasus-helikon-cli/tests/repl_commands.rs` (command parsing only — the loop itself is I/O glue)

**Interfaces:**
- Produces:
  - `repl::run(args: ReplArgs) -> anyhow::Result<ExitCode>` — loads registry, starts watcher (prints `reloaded agents.toml` / `reload failed: <err>` on events via an mpsc drained in the select loop), REPL loop over `tokio::io::BufReader::new(tokio::io::stdin()).lines()` with `tokio::select!` between lines and reload notifications; slash commands parsed by `pub fn parse_repl_command(line: &str) -> ReplCommand` where `ReplCommand { Agents, Switch(String), Reload, Quit, Say(String) }`; a turn = `registry.build_agent(current)?` → `TokioRunner.run_streamed(&agent, RunContext::ephemeral(()).with_session(session.clone()), AgentInput::from_user_text(line), RunConfig::default())` → print `TokenDelta` text as it streams, newline on `MessageOutput`, `error: <e>` on `RunFailed`. Session: `Arc::new(MemorySession::new())` created once (verify the ctor name in core `session.rs:263` — use `MemorySession::default()` if `new` doesn't exist).
  - `mcp_cmd::serve(args: McpServeArgs) -> anyhow::Result<ExitCode>` — `let agent = registry.build_agent(&args.agent)?;` then `McpAgentServer::with_default_ctx(agent).name(format!("helikon-{}", args.agent)).serve_stdio().await` or `.serve_streamable_http(&addr).await`.

- [ ] **Step 1: Failing test** — `tests/repl_commands.rs` covering `parse_repl_command`: `"/agents"` → `Agents`, `"/switch budgeting"` → `Switch("budgeting")`, `"/reload"` → `Reload`, `"/quit"` → `Quit`, `"how much?"` → `Say(...)`, `"/unknown"` → `Say("/unknown")` (unknown slash falls through as text? No — better: `Unknown(String)` variant printed as `unknown command`; assert that).
- [ ] **Step 2-4: Implement, run until green.** `cargo test -p paigasus-helikon-cli` and `cargo build -p paigasus-helikon-cli` must both pass; manually sanity-check `target/debug/helikon repl --agents crates/paigasus-helikon-cli/tests/fixtures/agents.toml` starts and `/quit` exits (mock agent needs no keys).
- [ ] **Step 5: Commit** `feat(cli): SMA-333 add repl with hot reload and mcp serve`

---

## Task Group D — Examples, docs, release

### Task 17: Facade examples (AC3) + core swarm example test

**Files:**
- Create: `crates/paigasus-helikon/examples/swarm_finance.rs`, `crates/paigasus-helikon/examples/graph_report.rs`
- Modify: `crates/paigasus-helikon/Cargo.toml` (two `[[example]]` blocks)

**Interfaces:** consumes `paigasus_helikon::core::{SwarmAgent, GraphAgent, …}` (facade re-exports core wholesale) + `paigasus_helikon::openai::OpenAiModel`.

- [ ] **Step 1:** `swarm_finance.rs` — 3 members (triage/budgeting/investing), mirroring `multi_agent_triage.rs`'s style and doc-comment header (`OPENAI_API_KEY=… cargo run -p paigasus-helikon --features openai --example swarm_finance`), built with `SwarmAgent::builder().name("support_swarm").member(triage).member(budgeting).member(investing).entry("triage").max_handoffs(6).build()?`, run via `RunResultStreaming::new(swarm.run(ctx, input).await?).collect().await?`, printing `final_output`. Triage instructions: "Route to the right specialist via transfer; answer yourself only for trivial questions." `graph_report.rs` — diamond: `spending`/`income` fan-out → `summary` sink, `.edge("spending","summary").edge("income","summary")`, printing the sink output.
- [ ] **Step 2:** Register both:

```toml
[[example]]
name              = "swarm_finance"
required-features = ["openai"]

[[example]]
name              = "graph_report"
required-features = ["openai"]
```

- [ ] **Step 3: Verify compile:** `cargo build -p paigasus-helikon --features openai --examples`
- [ ] **Step 4: Commit** `feat(facade): SMA-333 add swarm and graph examples`

### Task 18: Documentation (mdBook + READMEs)

**Files:**
- Modify: `docs/book/src/concepts/multi-agent-patterns.md` (Swarm/Graph sections + two rows in the "Choosing a pattern" table), `docs/book/src/concepts/observability-evaluation.md` (real evals-crate section: dataset/evaluators/MockModel/trace sinks + `helikon eval run`), `docs/book/src/reference/crates.md` (roster: evals + cli now published), `docs/book/src/SUMMARY.md` (+ new page), root `README.md`, `crates/paigasus-helikon/README.md`
- Create: `docs/book/src/reference/cli.md`, real `crates/paigasus-helikon-evals/README.md`, real `crates/paigasus-helikon-cli/README.md`

**Steps:**

- [ ] **Step 1: mdBook.** Add to `SUMMARY.md` under Reference: `- [CLI](./reference/cli.md)`. `cli.md` covers: install (`cargo install paigasus-helikon-cli`), the two binary names, all three subcommands with the exact flag grammar from Task 11, a complete `agents.toml` example (copy Task 15's fixture, openai variant for the model line), the hot-reload behavior (in-flight turn unaffected; parse errors keep old defs), and the handoff-cycle rejection note (spec deviation from §5.2 recorded here). `multi-agent-patterns.md`: extend the intro list and table with `SwarmAgent` (pool + auto-injected handoffs; ends on first final output; `max_handoffs` budget; underlying `max_agent_depth` bound) and `GraphAgent` (declared DAG; dependency-gated; state keys per node; deterministic sink merge; failure skips descendants), each with a short builder snippet from Task 2/3 tests. `observability-evaluation.md`: replace the "v0.3 / Stage 3" future-tense section with present-tense crate docs + a compact `EvalRun` snippet (from Task 9's test, trimmed) + the `trace-sqlite`/`trace-parquet` feature note.
- [ ] **Step 2: READMEs.** `crates/paigasus-helikon-evals/README.md`: title, one-paragraph pitch, `cargo add paigasus-helikon-evals` (plus `--features trace-sqlite,trace-parquet` note), the `EvalRun` snippet in a ```` ```rust,ignore ```` fence (README not include_str!'d, but `ignore` keeps it copy-paste-honest), evaluator table from spec §4.1, MockModel/ScriptFile paragraph. `crates/paigasus-helikon-cli/README.md`: install via `cargo install paigasus-helikon-cli`, binaries `helikon` + `paigasus-helikon`, subcommand summary, minimal `agents.toml`, pointer to the book's CLI page. Facade `README.md`: flip the `evals` row in the feature→module map from stub to real (match existing row phrasing). Root `README.md`: update the crate roster table rows for `-evals` (published, evaluation harness) and `-cli` (published binary).
- [ ] **Step 3: Verify:** `mdbook build docs/book` → clean (linkcheck is error-level). 
- [ ] **Step 4: Commit** `docs(docs): SMA-333 document evals crate, cli, and swarm/graph patterns` (split into `docs(readme)` + `docs(docs)` commits if cleaner; both scopes are allowed).

### Task 19: Release engineering (ascend evals + cli) + full gates

**Files:**
- Modify: `crates/paigasus-helikon-evals/Cargo.toml`, `crates/paigasus-helikon-cli/Cargo.toml`, root `Cargo.toml`, `release-plz.toml`, `deny.toml`

**Steps:**

- [ ] **Step 1: Ascend evals.** `crates/paigasus-helikon-evals/Cargo.toml`: `version = "0.0.0"` → `"0.1.0"`, delete the `publish = false` line. Root `Cargo.toml`: `paigasus-helikon-evals = { path = …, version = "0.0.0" }` → `version = "0.1.0"`. `release-plz.toml`: delete the whole `[[package]] name = "paigasus-helikon-evals" publish = false release = false` block.
- [ ] **Step 2: Ascend cli.** `crates/paigasus-helikon-cli/Cargo.toml`: `version = "0.0.0"` → `"0.1.0"`, delete `publish = false`. `release-plz.toml`: delete the `[[package]] name = "paigasus-helikon-cli" publish = false` block (and its "Binary-only" comment). Note the cli README + lib banner already state "internal, no stability guarantees" (Task 18/11).
- [ ] **Step 3: deny.toml.** Add to `[licenses].allow` (keep the existing comment style):

```toml
  "CC0-1.0",                        # notify + notify-debouncer-mini (CLI file watching); public-domain dedication — accepted at SMA-333 GATE 1 (patent non-grant noted)
```

- [ ] **Step 4: Full local CI parity run** (fix anything red before committing):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
mdbook build docs/book
cargo deny check
cargo build -p paigasus-helikon-runtime-axum --no-default-features
```

Also verify the publish story dry-run: `cargo publish -p paigasus-helikon-evals --dry-run` (must build against registry core — this is the registry-verify rule's proof) and `cargo package -p paigasus-helikon-cli --list` (sanity: both bins + lib included). `cargo publish -p paigasus-helikon-cli --dry-run` will fail with "evals 0.1.0 not found on registry" — that is EXPECTED (release-plz publishes evals first on merge); do not "fix" it.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-evals/Cargo.toml crates/paigasus-helikon-cli/Cargo.toml Cargo.toml Cargo.lock release-plz.toml deny.toml
git commit -m "chore(release): SMA-333 lift stage-1 gates for paigasus-helikon-evals and paigasus-helikon-cli"
```

---

## Plan self-review notes (already applied)

- Spec coverage: §3 → Tasks 1-4; §4 → Tasks 5-10; §5 → Tasks 11-16; §7 tests distributed into each task; §8 deps → Tasks 5/10/11/19; §9 → Tasks 17-19. The spec's §5.2 "cyclic handoffs acceptable" is refined to validation-time rejection (Task 12, documented in Task 18) — safer and honest.
- Type consistency: `Score{value, outcome, detail}` + `ScoreOutcome` used in Tasks 7-10/15; `CliModel` in 13-16; `RunMeta`/`CaseResult` in 9/10/15; `MaxHandoffsExceeded{limit}` in 1/2/17.
- Known verify-at-implementation points (flagged inline): `Item` user-message constructor for evals (`AgentInput::from_user_text` fallback given), `MemorySession::new` vs `default`, exact latest versions/MSRV for `jsonschema`/`arrow`/`parquet`/`notify`/`toml`/`clap`, `hooks().iter()`/`HookEvent::OnSubagentStop` exact shape (copy from workflow.rs), `FailureSlot::take` closure form.
