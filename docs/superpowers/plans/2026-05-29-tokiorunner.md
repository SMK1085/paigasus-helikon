# TokioRunner (SMA-321) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `TokioRunner`, the default ephemeral `Runner<Ctx>`, adding run-level cancellation, timeout, and bounded tool-call concurrency on top of the existing `LlmAgent` loop.

**Architecture:** `TokioRunner` is thin — it consumes `agent.run()`'s `AgentEvent` stream and wraps it with tokio control (`biased` `select!` on cancel-token / deadline; drop-based cancellation). The loop driver stays in `core::agent`; bounded concurrency and per-invocation config are made *in place* there and threaded via `RunContext`. The pure `transition` fn remains the durability seam (ADR-13). Session persistence is deferred to a no-op `finalize()` seam that runs on every exit.

**Tech Stack:** Rust, `tokio` (runtime-tokio only — core stays tokio-runtime-free), `futures-util`, `async-stream`, `async-trait`.

**Spec:** `docs/superpowers/specs/2026-05-29-tokiorunner-design.md`.

**Conventions:** Branch `feature/sma-321-tokiorunner-cancellation-timeouts-parallel-tool-calls` (already created). Commit prefix `<type>(<scope>): SMA-### <lowercase subject>`. Before every commit run `cargo fmt --all` and the relevant `cargo clippy … -- -D warnings` (pre-push hook enforces these; failing to run them locally is the #1 CI surprise).

---

## File map

| File | Change | Responsibility |
|------|--------|----------------|
| `crates/paigasus-helikon-core/src/runner.rs` | modify | `RunConfig` fields + builders; `RunError::Timeout` |
| `crates/paigasus-helikon-core/src/context.rs` | modify | `RunContext::{with_run_config,run_config}` (runner-injection channel) |
| `crates/paigasus-helikon-core/src/agent.rs` | modify | resolve effective config; bounded `run_tools_concurrent` |
| `crates/paigasus-helikon-core/tests/common/mod.rs` | modify | add `ConcurrencyProbe` tool |
| `crates/paigasus-helikon-core/tests/loop_parallel_limit.rs` | create | bounded-concurrency test |
| `crates/paigasus-helikon-runtime-tokio/Cargo.toml` | modify | deps + (Task 8) ascend |
| `crates/paigasus-helikon-runtime-tokio/src/lib.rs` | modify | `TokioRunner`, `controlled()`, `finalize()` |
| `crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs` | create | mocks (`MockModel`, `PendingModel`, `MockToolBarrier`, `CountingSession`, helpers) |
| `crates/paigasus-helikon-runtime-tokio/tests/run_smoke.rs` | create | happy-path smoke (Task 5) |
| `crates/paigasus-helikon-runtime-tokio/tests/run_control.rs` | create | cancel / timeout / same-poll / finalize-on-`run` (Task 6) |
| `crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs` | create | ordering / concurrency / terminal / finalize-on-`run_streamed` (Task 7) |
| `release-plz.toml` | modify | remove the runtime-tokio `release = false` block (Task 8) |
| root `Cargo.toml` | modify | bump `runtime-tokio` workspace dep to `0.1.0` (Task 8) |

---

## Task 1: Core — `RunConfig` gains `timeout` + `parallel_tool_call_limit`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs`

- [ ] **Step 1: Write the failing test**

In `crates/paigasus-helikon-core/src/runner.rs`, the file ends with `#[cfg(test)] mod tests { use super::*; … }`? It does **not** today — add this test module at the end of the file:

```rust
#[cfg(test)]
mod runconfig_tests {
    use super::*;

    #[test]
    fn run_config_defaults_and_builders() {
        let c = RunConfig::default();
        assert_eq!(c.max_turns, 16);
        assert!(c.timeout.is_none());
        assert!(c.parallel_tool_call_limit.is_none());

        let c = RunConfig::new()
            .with_timeout(std::time::Duration::from_secs(5))
            .with_parallel_tool_call_limit(std::num::NonZeroUsize::new(3).unwrap());
        assert_eq!(c.timeout, Some(std::time::Duration::from_secs(5)));
        assert_eq!(c.parallel_tool_call_limit, std::num::NonZeroUsize::new(3));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-core run_config_defaults_and_builders`
Expected: FAIL to compile — `no field timeout`, `no method with_timeout`.

- [ ] **Step 3: Add the fields + builders**

In `runner.rs`, add near the top (after the existing `use` lines):

```rust
use std::num::NonZeroUsize;
use std::time::Duration;
```

Replace the `RunConfig` struct (currently only `max_turns`) with:

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunConfig {
    /// `[driver-scoped]` Maximum number of model turns before the loop fails
    /// with [`crate::AgentError::MaxTurnsExceeded`]. Honored by the core loop
    /// driver, including on a bare `agent.run()` with no runner. Default `16`.
    pub max_turns: u32,
    /// `[runner-scoped]` Wall-clock deadline for the whole run. Honored ONLY by
    /// a runtime backend (e.g. `TokioRunner`); a bare `agent.run()` cannot time
    /// out (core has no timer). `None` = no deadline.
    pub timeout: Option<Duration>,
    /// `[driver-scoped]` Cap on concurrently-executing tool calls. Honored by
    /// the core loop driver. `None` = unbounded (today's behavior).
    pub parallel_tool_call_limit: Option<NonZeroUsize>,
}
```

Replace the `impl Default for RunConfig`:

```rust
impl Default for RunConfig {
    fn default() -> Self {
        Self {
            max_turns: 16,
            timeout: None,
            parallel_tool_call_limit: None,
        }
    }
}
```

Add the builders inside `impl RunConfig` (which currently holds only `new`):

```rust
    /// Set the wall-clock run deadline. Honored by a runtime backend (e.g. `TokioRunner`).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Cap the number of tool calls executed concurrently. Honored by the core loop driver.
    pub fn with_parallel_tool_call_limit(mut self, limit: NonZeroUsize) -> Self {
        self.parallel_tool_call_limit = Some(limit);
        self
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-core run_config_defaults_and_builders`
Expected: PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/runner.rs
git commit -m "feat(core): SMA-321 add timeout + parallel_tool_call_limit to RunConfig"
```

---

## Task 2: Core — `RunError::Timeout`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs`

- [ ] **Step 1: Write the failing test**

Add to the `runconfig_tests` module created in Task 1:

```rust
    #[test]
    fn run_error_timeout_displays() {
        assert_eq!(RunError::Timeout.to_string(), "run timed out");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-core run_error_timeout_displays`
Expected: FAIL to compile — `no variant Timeout`.

- [ ] **Step 3: Add the variant**

In `runner.rs`, in `pub enum RunError`, add after the `Cancelled` variant:

```rust
    /// The run exceeded its configured [`RunConfig::timeout`].
    #[error("run timed out")]
    Timeout,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-core run_error_timeout_displays`
Expected: PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/runner.rs
git commit -m "feat(core): SMA-321 add RunError::Timeout variant"
```

---

## Task 3: Core — `RunContext` carries per-invocation `RunConfig`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/paigasus-helikon-core/src/context.rs`:

```rust
#[cfg(test)]
mod runcontext_tests {
    use super::*;
    use crate::{MemorySession, RunConfig};
    use std::sync::Arc;

    #[test]
    fn with_run_config_round_trips_and_defaults_none() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        );
        assert!(ctx.run_config().is_none());

        let ctx = ctx.with_run_config(
            RunConfig::new().with_timeout(std::time::Duration::from_secs(1)),
        );
        assert_eq!(
            ctx.run_config().unwrap().timeout,
            Some(std::time::Duration::from_secs(1))
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-core with_run_config_round_trips_and_defaults_none`
Expected: FAIL to compile — `no method run_config` / `with_run_config`.

- [ ] **Step 3: Add the field + methods**

In `context.rs`, add `RunConfig` to the crate import:

```rust
use crate::{Hook, RunConfig, Session, ToolContext};
```

Add a field to `struct RunContext` (after `cancel: CancellationToken,`):

```rust
    /// Per-invocation execution policy, injected by a `Runner` (e.g.
    /// `TokioRunner`). This is the runner-injection channel, **not** general
    /// context state: it is deliberately NOT surfaced into [`ToolContext`] by
    /// [`RunContext::to_tool_context`]. `None` when an agent is run directly
    /// without a runner.
    run_config: Option<RunConfig>,
```

In `RunContext::new`, add `run_config: None,` to the struct initializer (leave the five parameters unchanged).

Add two methods inside `impl RunContext` (e.g. after the `cancel` accessor):

```rust
    /// Borrow the per-invocation [`RunConfig`], if a runner installed one.
    pub fn run_config(&self) -> Option<&RunConfig> {
        self.run_config.as_ref()
    }

    /// Install the per-invocation [`RunConfig`] (consuming builder). A
    /// [`crate::Runner`] calls this before [`crate::Agent::run`].
    pub fn with_run_config(mut self, config: RunConfig) -> Self {
        self.run_config = Some(config);
        self
    }
```

> Do **not** touch `to_tool_context` — it must keep passing only `user_ctx`, `tracer`, and the child cancel token, so `run_config` never leaks into `ToolContext`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-core with_run_config_round_trips_and_defaults_none`
Expected: PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/context.rs
git commit -m "feat(core): SMA-321 thread per-invocation RunConfig through RunContext"
```

---

## Task 4: Core — effective-config resolution + bounded tool concurrency

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs`
- Modify: `crates/paigasus-helikon-core/tests/common/mod.rs`
- Create: `crates/paigasus-helikon-core/tests/loop_parallel_limit.rs`

- [ ] **Step 1: Add the `ConcurrencyProbe` test tool**

Append to `crates/paigasus-helikon-core/tests/common/mod.rs` (the file already has `#![allow(dead_code)]` and imports `Tool`, `ToolContext`, `ToolError`, `ToolOutput`, `async_trait`, `Arc`):

```rust
use std::sync::atomic::{AtomicUsize, Ordering};

/// A [`Tool`] that tracks how many instances run concurrently. Each
/// invocation bumps `current`, records the running peak into `max`, yields
/// several times (so the scheduler can interleave peers), then decrements.
pub struct ConcurrencyProbe {
    name: String,
    description: String,
    schema: serde_json::Value,
    current: Arc<AtomicUsize>,
    max: Arc<AtomicUsize>,
}

impl ConcurrencyProbe {
    pub fn new(name: &str, current: Arc<AtomicUsize>, max: Arc<AtomicUsize>) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            description: format!("concurrency probe {name}"),
            schema: serde_json::json!({"type": "object"}),
            current,
            max,
        })
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for ConcurrencyProbe
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }

    async fn invoke(
        &self,
        _ctx: &ToolContext<Ctx>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(now, Ordering::SeqCst);
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        self.current.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::new(serde_json::json!({"ok": true})))
    }
}
```

- [ ] **Step 2: Write the failing test**

Create `crates/paigasus-helikon-core/tests/loop_parallel_limit.rs`:

```rust
//! `parallel_tool_call_limit` bounds concurrent tool execution; `None` runs
//! all tool calls at once. Verified with a peak-concurrency probe.

#[path = "common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentInput, FinishReason, Instructions, LlmAgent, ModelEvent, ModelSettings, RunConfig,
    RunResultStreaming, Tool,
};

use common::{noop_run_context, ConcurrencyProbe, MockModel};

fn four_call_model() -> Arc<MockModel> {
    let mut calls = Vec::new();
    for i in 1..=4 {
        calls.push(ModelEvent::ToolCallDelta {
            call_id: i.to_string(),
            name: Some(format!("p{i}")),
            args_delta: "{}".into(),
        });
    }
    calls.push(ModelEvent::Finish {
        reason: FinishReason::ToolCalls,
    });
    MockModel::with_scripts(vec![
        calls,
        vec![
            ModelEvent::TokenDelta {
                text: "done".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ])
}

fn probe_agent(current: Arc<AtomicUsize>, max: Arc<AtomicUsize>) -> LlmAgent<(), MockModel> {
    let tools: Vec<Arc<dyn Tool<()>>> = (1..=4)
        .map(|i| ConcurrencyProbe::new(&format!("p{i}"), current.clone(), max.clone()) as Arc<dyn Tool<()>>)
        .collect();
    LlmAgent::<(), _> {
        name: "test".into(),
        description: "probe".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model: four_call_model(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limit_two_caps_concurrency() {
    let current = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let agent = probe_agent(current.clone(), max.clone());

    let ctx = noop_run_context::<()>().with_run_config(
        RunConfig::new().with_parallel_tool_call_limit(std::num::NonZeroUsize::new(2).unwrap()),
    );
    let stream = agent
        .run(ctx, AgentInput::from_user_text("go"))
        .await
        .expect("run starts");
    RunResultStreaming::new(stream).collect().await.expect("ok");

    assert_eq!(max.load(Ordering::SeqCst), 2, "peak concurrency must be capped at 2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unbounded_runs_all_four() {
    let current = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let agent = probe_agent(current.clone(), max.clone());

    // No run_config => falls back to agent.config (limit None => unbounded).
    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .expect("run starts");
    RunResultStreaming::new(stream).collect().await.expect("ok");

    assert_eq!(max.load(Ordering::SeqCst), 4, "unbounded must run all four at once");
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-core --test loop_parallel_limit`
Expected: FAIL — `limit_two_caps_concurrency` sees peak 4 (limit not yet honored).

- [ ] **Step 4: Implement bounded concurrency + config resolution in `agent.rs`**

In `crates/paigasus-helikon-core/src/agent.rs`, in `LlmAgent::run`, replace this snapshot line:

```rust
        let max_turns = self.config.max_turns;
```

with:

```rust
        let effective_config = ctx
            .run_config()
            .cloned()
            .unwrap_or_else(|| self.config.clone());
        let max_turns = effective_config.max_turns;
        let parallel_tool_call_limit = effective_config.parallel_tool_call_limit;
```

In the same function, in the `crate::NextAction::ExecuteTools { calls }` arm, replace:

```rust
                        let outcomes =
                            run_tools_concurrent(&tools, &calls, &tool_ctx).await;
```

with:

```rust
                        let outcomes = run_tools_concurrent(
                            &tools,
                            &calls,
                            &tool_ctx,
                            parallel_tool_call_limit,
                        )
                        .await;
```

Change the `run_tools_concurrent` signature to take the limit (add the 4th param):

```rust
async fn run_tools_concurrent<Ctx>(
    tools: &[std::sync::Arc<dyn crate::Tool<Ctx>>],
    calls: &[crate::ToolCallRequest],
    tool_ctx: &crate::ToolContext<Ctx>,
    limit: Option<std::num::NonZeroUsize>,
) -> Vec<crate::ToolCallOutcome>
where
    Ctx: Send + Sync + 'static,
{
```

And replace its final expression:

```rust
    futures_util::future::join_all(futures).await
```

with:

```rust
    match limit {
        None => futures_util::future::join_all(futures).await,
        Some(n) => {
            use futures_util::stream::StreamExt as _;
            futures_util::stream::iter(futures)
                .buffered(n.get())
                .collect()
                .await
        }
    }
```

(`buffered`, not `buffer_unordered`, so outcome order matches call order.)

- [ ] **Step 5: Run the new test + full core regression**

Run: `cargo test -p paigasus-helikon-core --test loop_parallel_limit`
Expected: PASS (both functions).

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS — all existing loop/session/structured tests stay green.

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/tests/common/mod.rs crates/paigasus-helikon-core/tests/loop_parallel_limit.rs
git commit -m "feat(core): SMA-321 honor parallel_tool_call_limit and ctx run config"
```

---

## Task 5: runtime-tokio — dependencies, scaffold, mocks, smoke test

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/Cargo.toml`
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs`
- Create: `crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs`
- Create: `crates/paigasus-helikon-runtime-tokio/tests/run_smoke.rs`

- [ ] **Step 1: Add dependencies**

Replace the body of `crates/paigasus-helikon-runtime-tokio/Cargo.toml` below the `[package]` block (keep the existing `[package]` and `[lints]` blocks; do **not** bump the version or remove `publish = false` yet — that is Task 8) so it reads:

```toml
[dependencies]
paigasus-helikon-core = { workspace = true }
async-trait  = { workspace = true }
futures-core = { workspace = true }
futures-util = { workspace = true }
async-stream = { workspace = true }
tokio        = { workspace = true }
tokio-util   = { workspace = true }

[dev-dependencies]
tokio      = { workspace = true }
anyhow     = { workspace = true }
serde_json = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 2: Write the skeleton `TokioRunner`**

Replace `crates/paigasus-helikon-runtime-tokio/src/lib.rs` entirely:

```rust
//! `paigasus-helikon-runtime-tokio` — the default ephemeral Tokio runner.
//!
//! [`TokioRunner`] implements [`paigasus_helikon_core::Runner`] by consuming
//! the agent's [`paigasus_helikon_core::AgentEvent`] stream and adding
//! run-level control (cancellation, timeout, aggregation) at the boundary. It
//! does not own the loop driver — the agent does (see ADR-13).

use async_trait::async_trait;
use paigasus_helikon_core::{
    Agent, AgentInput, RunConfig, RunContext, RunError, RunResult, RunResultStreaming, Runner,
};

/// The default ephemeral execution backend. Stateless.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioRunner;

#[async_trait]
impl<Ctx> Runner<Ctx> for TokioRunner
where
    Ctx: Send + Sync + 'static,
{
    async fn run(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResult, RunError> {
        let ctx = ctx.with_run_config(config);
        let stream = agent.run(ctx, input).await?;
        RunResultStreaming::new(stream).collect().await
    }

    async fn run_streamed(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        let ctx = ctx.with_run_config(config);
        let stream = agent.run(ctx, input).await?;
        Ok(RunResultStreaming::new(stream))
    }
}
```

- [ ] **Step 3: Write the shared test mocks**

Create `crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs`:

```rust
//! Shared mocks for TokioRunner integration tests.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;

use paigasus_helikon_core::{
    CancellationToken, ConversationSnapshot, HookRegistry, Instructions, LlmAgent, Model,
    ModelCapabilities, ModelError, ModelEvent, ModelRequest, ModelSettings, RunConfig, RunContext,
    SequenceId, Session, SessionError, SessionEvent, Tool, ToolContext, ToolError, ToolOutput,
    TracerHandle,
};

/// Scripted model: one `Vec<ModelEvent>` per `invoke`; empty queue => error.
pub struct MockModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl MockModel {
    pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(VecDeque::from(scripts)),
        })
    }

    /// One quick assistant turn: "hi" then stop.
    pub fn quick_hi() -> Arc<Self> {
        Self::with_scripts(vec![vec![
            ModelEvent::TokenDelta { text: "hi".into() },
            ModelEvent::Finish {
                reason: paigasus_helikon_core::FinishReason::Stop,
            },
        ]])
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
            .unwrap()
            .pop_front()
            .ok_or_else(|| ModelError::Other(anyhow::anyhow!("no more scripted responses")))?;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// A model whose response stream never completes — for cancellation/timeout.
pub struct PendingModel;

#[async_trait]
impl Model for PendingModel {
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        Ok(Box::pin(stream::pending::<Result<ModelEvent, ModelError>>()))
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// Barrier-synced tool: N instances on a `Barrier::new(N)` deadlock unless
/// they run concurrently.
pub struct MockToolBarrier {
    name: String,
    description: String,
    schema: serde_json::Value,
    barrier: Arc<tokio::sync::Barrier>,
}

impl MockToolBarrier {
    pub fn new(name: &str, barrier: Arc<tokio::sync::Barrier>) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            description: format!("barrier tool {name}"),
            schema: serde_json::json!({"type": "object"}),
            barrier,
        })
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for MockToolBarrier
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext<Ctx>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.barrier.wait().await;
        Ok(ToolOutput::new(serde_json::json!({"ok": true})))
    }
}

/// Session that counts `append` calls — lets tests assert `finalize` ran.
#[derive(Default)]
pub struct CountingSession {
    appends: AtomicUsize,
}

impl CountingSession {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn append_count(&self) -> usize {
        self.appends.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Session for CountingSession {
    async fn append(&self, _events: &[SessionEvent]) -> Result<(), SessionError> {
        self.appends.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn events(&self, _since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        Ok(Vec::new())
    }
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(ConversationSnapshot::default())
    }
}

/// No-op session.
pub struct NoopSession;

#[async_trait]
impl Session for NoopSession {
    async fn append(&self, _events: &[SessionEvent]) -> Result<(), SessionError> {
        Ok(())
    }
    async fn events(&self, _since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        Ok(Vec::new())
    }
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(ConversationSnapshot::default())
    }
}

pub fn noop_run_context() -> RunContext<()> {
    RunContext::new(
        Arc::new(()),
        Arc::new(NoopSession) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
}

pub fn run_context_with_cancel(cancel: CancellationToken) -> RunContext<()> {
    RunContext::new(
        Arc::new(()),
        Arc::new(NoopSession) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        cancel,
    )
}

pub fn run_context_with_session(session: Arc<dyn Session>) -> RunContext<()> {
    RunContext::new(
        Arc::new(()),
        session,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
}

pub fn run_context_with_session_and_cancel(
    session: Arc<dyn Session>,
    cancel: CancellationToken,
) -> RunContext<()> {
    RunContext::new(
        Arc::new(()),
        session,
        HookRegistry::new(),
        TracerHandle::default(),
        cancel,
    )
}

/// Build an `LlmAgent<(), M>` with the given model and tools.
pub fn text_agent<M: Model + 'static>(
    model: Arc<M>,
    tools: Vec<Arc<dyn Tool<()>>>,
) -> LlmAgent<(), M> {
    LlmAgent::<(), _> {
        name: "test".into(),
        description: "test agent".into(),
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
```

- [ ] **Step 4: Write the smoke test**

Create `crates/paigasus-helikon-runtime-tokio/tests/run_smoke.rs`:

```rust
#[path = "common/mod.rs"]
mod common;

use paigasus_helikon_core::{AgentInput, RunConfig, Runner};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{noop_run_context, text_agent, MockModel};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_returns_final_output() {
    let agent = text_agent(MockModel::quick_hi(), Vec::new());
    let result = TokioRunner
        .run(
            &agent,
            noop_run_context(),
            AgentInput::from_user_text("yo"),
            RunConfig::default(),
        )
        .await
        .expect("run ok");
    assert_eq!(result.final_output, "hi");
}
```

- [ ] **Step 5: Build + run**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_smoke`
Expected: PASS.

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-tokio --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-tokio/Cargo.toml crates/paigasus-helikon-runtime-tokio/src/lib.rs crates/paigasus-helikon-runtime-tokio/tests/common/mod.rs crates/paigasus-helikon-runtime-tokio/tests/run_smoke.rs
git commit -m "feat(runtime-tokio): SMA-321 scaffold TokioRunner with passthrough run"
```

---

## Task 6: runtime-tokio — `controlled()` + full `run` (cancel, timeout, finalize)

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs`
- Create: `crates/paigasus-helikon-runtime-tokio/tests/run_control.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/paigasus-helikon-runtime-tokio/tests/run_control.rs`:

```rust
//! run-level control: cancellation, timeout, biased completion, finalize.

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use paigasus_helikon_core::{
    AgentInput, CancellationToken, RunConfig, RunError, Runner, Session,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{
    run_context_with_cancel, run_context_with_session, run_context_with_session_and_cancel,
    text_agent, CountingSession, MockModel, PendingModel,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_aborts_in_flight_run() {
    let cancel = CancellationToken::new();
    let ctx = run_context_with_cancel(cancel.clone());
    let agent = text_agent(std::sync::Arc::new(PendingModel), Vec::new());

    let res = tokio::time::timeout(Duration::from_secs(5), async {
        let run_fut = TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        );
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (r, _) = tokio::join!(run_fut, canceller);
        r
    })
    .await
    .expect("run must abort within 5s of cancel");

    assert!(matches!(res, Err(RunError::Cancelled)), "got {res:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_returns_timeout() {
    let agent = text_agent(std::sync::Arc::new(PendingModel), Vec::new());
    let res = tokio::time::timeout(Duration::from_secs(5), async {
        TokioRunner
            .run(
                &agent,
                common::run_context_with_cancel(CancellationToken::new()),
                AgentInput::from_user_text("go"),
                RunConfig::new().with_timeout(Duration::from_millis(50)),
            )
            .await
    })
    .await
    .expect("run must self-timeout within 5s");

    assert!(matches!(res, Err(RunError::Timeout)), "got {res:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prefired_cancel_still_completes_ready_run() {
    // Token already cancelled, but every event is immediately ready:
    // biased stream-first must drain to completion, not report Cancelled.
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = run_context_with_cancel(cancel);
    let agent = text_agent(MockModel::quick_hi(), Vec::new());

    let res = TokioRunner
        .run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(res.is_ok(), "ready run must complete despite a fired token: {res:?}");
    assert_eq!(res.unwrap().final_output, "hi");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_runs_on_every_run_exit() {
    // 1. normal
    let session = CountingSession::new();
    let agent = text_agent(MockModel::quick_hi(), Vec::new());
    let _ = TokioRunner
        .run(
            &agent,
            run_context_with_session(session.clone() as std::sync::Arc<dyn Session>),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert_eq!(session.append_count(), 1, "finalize on normal exit");

    // 2. agent failure (empty scripts => model invoke errors => RunFailed)
    let session = CountingSession::new();
    let agent = text_agent(MockModel::with_scripts(vec![]), Vec::new());
    let res = TokioRunner
        .run(
            &agent,
            run_context_with_session(session.clone() as std::sync::Arc<dyn Session>),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await;
    assert!(res.is_err(), "agent failure must be Err");
    assert_eq!(session.append_count(), 1, "finalize on failure exit");

    // 3. cancel
    let session = CountingSession::new();
    let cancel = CancellationToken::new();
    let ctx = run_context_with_session_and_cancel(
        session.clone() as std::sync::Arc<dyn Session>,
        cancel.clone(),
    );
    let agent = text_agent(std::sync::Arc::new(PendingModel), Vec::new());
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        let run_fut = TokioRunner.run(&agent, ctx, AgentInput::from_user_text("go"), RunConfig::default());
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (r, _) = tokio::join!(run_fut, canceller);
        r
    })
    .await
    .expect("cancel within 5s");
    assert_eq!(session.append_count(), 1, "finalize on cancel exit");

    // 4. timeout
    let session = CountingSession::new();
    let agent = text_agent(std::sync::Arc::new(PendingModel), Vec::new());
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        TokioRunner
            .run(
                &agent,
                run_context_with_session(session.clone() as std::sync::Arc<dyn Session>),
                AgentInput::from_user_text("go"),
                RunConfig::new().with_timeout(Duration::from_millis(50)),
            )
            .await
    })
    .await
    .expect("timeout within 5s");
    assert_eq!(session.append_count(), 1, "finalize on timeout exit");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_control`
Expected: FAIL — `cancel_aborts_in_flight_run` / `timeout_returns_timeout` exceed 5s (skeleton ignores cancel/timeout); `finalize_runs_on_every_run_exit` sees `append_count == 0`.

- [ ] **Step 3: Implement `controlled()`, `finalize()`, and the full `run`**

In `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, expand the imports:

```rust
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream::StreamExt as _;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, CancellationToken, RunConfig, RunContext, RunError, RunResult,
    RunResultStreaming, Runner, Session,
};
```

Add the outcome types + helpers (above the `impl Runner`):

```rust
/// How a controlled run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Completed,
    Cancelled,
    TimedOut,
}

/// Read handle for the outcome committed by [`controlled`].
struct OutcomeHandle(Arc<Mutex<Outcome>>);

impl OutcomeHandle {
    fn get(&self) -> Outcome {
        *self.0.lock().unwrap()
    }
}

/// Wrap an agent event stream with cancel/deadline control.
///
/// Passes agent events through. On cancellation or deadline it commits the
/// reason into the returned handle and ends the stream (dropping the inner
/// stream cancels nested in-flight awaits within one poll). The outcome is
/// committed *before* the terminating `None`, so a caller reading the handle
/// after draining never sees a stale value.
fn controlled(
    mut stream: BoxStream<'static, AgentEvent>,
    cancel: CancellationToken,
    timeout: Option<Duration>,
) -> (BoxStream<'static, AgentEvent>, OutcomeHandle) {
    let cell = Arc::new(Mutex::new(Outcome::Completed));
    let handle = OutcomeHandle(Arc::clone(&cell));
    let out = async_stream::stream! {
        let sleep = async move {
            match timeout {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                biased;
                maybe_ev = stream.next() => {
                    match maybe_ev {
                        Some(ev) => yield ev,
                        None => break, // inner stream done => Completed (default)
                    }
                }
                () = cancel.cancelled() => {
                    *cell.lock().unwrap() = Outcome::Cancelled;
                    break;
                }
                () = &mut sleep => {
                    *cell.lock().unwrap() = Outcome::TimedOut;
                    break;
                }
            }
        }
    };
    (Box::pin(out), handle)
}

/// Post-run finalization seam. **SMA-321: placeholder** — flushes zero events
/// so the session handle is wired end-to-end and the "finalize runs on every
/// exit" guarantee is testable now. Session persistence + compaction land in a
/// follow-up, which replaces the empty append with real event writing.
async fn finalize(session: &Arc<dyn Session>) {
    let _ = session.append(&[]).await;
}
```

Replace the skeleton `run` method body with:

```rust
    async fn run(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResult, RunError> {
        let timeout = config.timeout;
        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();

        let stream = agent.run(ctx, input).await?;
        let (controlled_stream, outcome) = controlled(stream, cancel, timeout);
        // Do NOT `?`-short-circuit before finalize: agent failures surface as
        // collect()=Err, and finalize must still run.
        let collected = RunResultStreaming::new(controlled_stream).collect().await;
        finalize(&session).await;

        match outcome.get() {
            Outcome::Cancelled => Err(RunError::Cancelled),
            Outcome::TimedOut => Err(RunError::Timeout),
            Outcome::Completed => collected,
        }
    }
```

(Leave `run_streamed` as the Task-5 skeleton for now — Task 7 rewrites it.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_control`
Expected: PASS (all four).

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_smoke`
Expected: PASS (regression).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-tokio --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-tokio/src/lib.rs crates/paigasus-helikon-runtime-tokio/tests/run_control.rs
git commit -m "feat(runtime-tokio): SMA-321 add cancellation, timeout, and finalize to run"
```

---

## Task 7: runtime-tokio — full `run_streamed` (ordering, concurrency, terminal, finalize)

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs`
- Create: `crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs`:

```rust
//! run_streamed: event ordering, concurrency, terminal-on-cancel, finalize.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::StreamExt as _;
use paigasus_helikon_core::{
    AgentEvent, AgentInput, CancellationToken, FinishReason, ModelEvent, RunConfig, Runner,
    Session, Tool,
};
use paigasus_helikon_runtime_tokio::TokioRunner;

use common::{
    noop_run_context, run_context_with_cancel, run_context_with_session, text_agent,
    CountingSession, MockModel, MockToolBarrier, PendingModel,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_event_order() {
    let agent = text_agent(MockModel::quick_hi(), Vec::new());
    let rs = TokioRunner
        .run_streamed(
            &agent,
            noop_run_context(),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("stream starts");

    let events: Vec<AgentEvent> = rs.events.collect().await;

    assert!(
        matches!(events.first(), Some(AgentEvent::RunStarted { .. })),
        "first must be RunStarted: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::RunCompleted { .. })),
        "last must be RunCompleted: {events:?}"
    );
    let msg = events
        .iter()
        .position(|e| matches!(e, AgentEvent::MessageOutput { .. }))
        .expect("a MessageOutput");
    let done = events
        .iter()
        .position(|e| matches!(e, AgentEvent::RunCompleted { .. }))
        .unwrap();
    assert!(msg < done, "semantic item must precede terminal");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_tools_run_concurrently() {
    let barrier = Arc::new(tokio::sync::Barrier::new(5));
    let tools: Vec<Arc<dyn Tool<()>>> = (1..=5)
        .map(|i| MockToolBarrier::new(&format!("t{i}"), barrier.clone()) as Arc<dyn Tool<()>>)
        .collect();

    let mut first = Vec::new();
    for i in 1..=5 {
        first.push(ModelEvent::ToolCallDelta {
            call_id: i.to_string(),
            name: Some(format!("t{i}")),
            args_delta: "{}".into(),
        });
    }
    first.push(ModelEvent::Finish {
        reason: FinishReason::ToolCalls,
    });
    let model = MockModel::with_scripts(vec![
        first,
        vec![
            ModelEvent::TokenDelta { text: "ok".into() },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);
    let agent = text_agent(model, tools);

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        TokioRunner.run(
            &agent,
            noop_run_context(),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        ),
    )
    .await
    .expect("tools ran serially (barrier deadlock)")
    .expect("run ok");

    assert!(matches!(
        result.events.last(),
        Some(AgentEvent::RunCompleted { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamed_cancel_emits_terminal_runfailed() {
    let cancel = CancellationToken::new();
    let ctx = run_context_with_cancel(cancel.clone());
    let agent = text_agent(Arc::new(PendingModel), Vec::new());

    let rs = TokioRunner
        .run_streamed(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("stream starts");

    let events = tokio::time::timeout(Duration::from_secs(5), async {
        let mut s = rs.events;
        let drain = async {
            let mut evs = Vec::new();
            while let Some(ev) = s.next().await {
                evs.push(ev);
            }
            evs
        };
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (evs, _) = tokio::join!(drain, canceller);
        evs
    })
    .await
    .expect("stream must end within 5s of cancel");

    assert!(
        matches!(events.last(), Some(AgentEvent::RunFailed { error }) if error == "run cancelled"),
        "last event must be RunFailed(run cancelled): {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finalize_runs_on_streamed_exits() {
    // normal
    let session = CountingSession::new();
    let agent = text_agent(MockModel::quick_hi(), Vec::new());
    let rs = TokioRunner
        .run_streamed(
            &agent,
            run_context_with_session(session.clone() as Arc<dyn Session>),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("starts");
    let _drained: Vec<AgentEvent> = rs.events.collect().await;
    assert_eq!(session.append_count(), 1, "finalize on normal streamed exit");

    // agent failure
    let session = CountingSession::new();
    let agent = text_agent(MockModel::with_scripts(vec![]), Vec::new());
    let rs = TokioRunner
        .run_streamed(
            &agent,
            run_context_with_session(session.clone() as Arc<dyn Session>),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect("starts");
    let _drained: Vec<AgentEvent> = rs.events.collect().await;
    assert_eq!(session.append_count(), 1, "finalize on failed streamed exit");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_streamed`
Expected: FAIL — `streamed_cancel_emits_terminal_runfailed` hangs/wrong-last-event (skeleton has no control / no terminal); `finalize_runs_on_streamed_exits` sees `append_count == 0`. (`streamed_event_order` and `five_tools_run_concurrently` already pass on the skeleton — that's fine.)

- [ ] **Step 3: Implement the full `run_streamed`**

In `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, replace the skeleton `run_streamed` body with:

```rust
    async fn run_streamed(
        &self,
        agent: &(dyn Agent<Ctx> + '_),
        ctx: RunContext<Ctx>,
        input: AgentInput,
        config: RunConfig,
    ) -> Result<RunResultStreaming, RunError> {
        let timeout = config.timeout;
        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();

        let stream = agent.run(ctx, input).await?;
        let (mut controlled_stream, outcome) = controlled(stream, cancel, timeout);

        let out = async_stream::stream! {
            while let Some(ev) = controlled_stream.next().await {
                yield ev;
            }
            match outcome.get() {
                Outcome::Cancelled => yield AgentEvent::RunFailed { error: "run cancelled".to_owned() },
                Outcome::TimedOut => yield AgentEvent::RunFailed { error: "run timed out".to_owned() },
                Outcome::Completed => {}
            }
            finalize(&session).await;
        };
        Ok(RunResultStreaming::new(Box::pin(out)))
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_streamed`
Expected: PASS (all four).

Run: `cargo test -p paigasus-helikon-runtime-tokio`
Expected: PASS (run_smoke + run_control + run_streamed).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-tokio --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-tokio/src/lib.rs crates/paigasus-helikon-runtime-tokio/tests/run_streamed.rs
git commit -m "feat(runtime-tokio): SMA-321 control run_streamed with terminal event and finalize"
```

---

## Task 8: Crate ascend `0.0.0 → 0.1.0` + full CI gate

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/Cargo.toml`
- Modify: `release-plz.toml`
- Modify: root `Cargo.toml`

- [ ] **Step 1: Bump the crate version + drop `publish = false`**

In `crates/paigasus-helikon-runtime-tokio/Cargo.toml`, change:

```toml
version                = "0.0.0"
```
to:
```toml
version                = "0.1.0"
```

and delete the line:

```toml
publish                = false
```

- [ ] **Step 2: Remove the release-plz stub block**

In `release-plz.toml`, delete this block entirely:

```toml
[[package]]
name = "paigasus-helikon-runtime-tokio"
publish = false
release = false
```

- [ ] **Step 3: Bump the workspace dependency pin**

In the root `Cargo.toml`, under `[workspace.dependencies]`, change the runtime-tokio line:

```toml
paigasus-helikon-runtime-tokio       = { path = "crates/paigasus-helikon-runtime-tokio",       version = "0.0.0" }
```
to:
```toml
paigasus-helikon-runtime-tokio       = { path = "crates/paigasus-helikon-runtime-tokio",       version = "0.1.0" }
```

- [ ] **Step 4: Verify the facade resolves with the runtime enabled**

Run: `cargo build -p paigasus-helikon --features runtime-tokio`
Expected: builds clean (the facade resolves `runtime-tokio` at `0.1.0`).

- [ ] **Step 5: Run the full local CI gate**

Run, expecting all green:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

If `cargo doc` warns about an intra-doc link to a removed `RunConfig::retry_policy`, note: the existing references in `model.rs` and the provider `error.rs` files are plain backtick code spans (not `[…]` links) and do **not** fail `-D warnings`. No change needed. If any become a hard error, change the phrase to backtick-only text.

- [ ] **Step 6: Commit the ascend**

```bash
git add crates/paigasus-helikon-runtime-tokio/Cargo.toml release-plz.toml Cargo.toml Cargo.lock
git commit -m "chore(release): SMA-321 lift stage-1 gates for paigasus-helikon-runtime-tokio"
```

(`Cargo.lock` is committed in this workspace; include it if `cargo build` updated it.)

---

## Task 9: Open the PR

- [ ] **Step 1: Push the branch**

```bash
git push -u origin feature/sma-321-tokiorunner-cancellation-timeouts-parallel-tool-calls
```

- [ ] **Step 2: Open the PR with a compliant title**

The PR title becomes the squashed `main` commit and is gated by `pr-title.yml`: it needs a full Conventional Commits `type(scope):` prefix AND a lowercase subject after the `SMA-321` token. Use:

```bash
gh pr create \
  --title "feat(runtime-tokio): SMA-321 add TokioRunner with cancellation, timeout, and bounded tool calls" \
  --body "Implements SMA-321. Adds the ephemeral TokioRunner (cancellation, timeout, bounded parallel tool calls), threads per-invocation RunConfig via RunContext, and ascends paigasus-helikon-runtime-tokio to 0.1.0. Design: docs/superpowers/specs/2026-05-29-tokiorunner-design.md. Records ADR-13.

🤖 Generated with [Claude Code](https://claude.com/claude-code)"
```

- [ ] **Step 3: Confirm CI is green**

Watch the required checks (`fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`). Cross-reference each required context actually reported (a dropped `pull_request_target` leaves a blank-but-blocking status).

---

## Self-review notes (author)

- **Spec coverage:** AC#1 cancellation → `cancel_aborts_in_flight_run`; timeout → `timeout_returns_timeout`; AC#2 concurrency → `five_tools_run_concurrently` + `limit_two_caps_concurrency`; AC#3 ordering → `streamed_event_order`; finalize-all-paths (H2/#1) → `finalize_runs_on_every_run_exit` + `finalize_runs_on_streamed_exits`; biased completion (M1) → `prefired_cancel_still_completes_ready_run`; OutcomeHandle ordering (#2) → committed-before-`None` in `controlled`; bounded concurrency (§4.3) → Task 4; config threading (D2) → Tasks 3–4; `RunError::Timeout` → Task 2; ascend (§7) → Task 8.
- **Deferred, intentionally (out of scope):** `retry_policy`; real session persistence/compaction (finalize is a placeholder empty append); structured `AgentError` at the boundary (SMA-346); driver extraction.
- **Type consistency:** `RunConfig.parallel_tool_call_limit: Option<NonZeroUsize>`, `RunConfig.timeout: Option<Duration>`, `RunContext::{with_run_config, run_config}`, `controlled(stream, cancel, timeout) -> (BoxStream, OutcomeHandle)`, `Outcome::{Completed, Cancelled, TimedOut}`, `finalize(&Arc<dyn Session>)` — used identically across Tasks 3–7.
