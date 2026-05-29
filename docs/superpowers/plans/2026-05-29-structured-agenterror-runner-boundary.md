# Structured `AgentError` at the Runner Boundary — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Carry the structured `AgentError` from where a run fails out to the Runner boundary so callers get `RunError::Agent(AgentError::…)` instead of an opaque string — without changing the `Clone`, string-based `AgentEvent::RunFailed { error: String }`.

**Architecture:** A `FailureSlot` (`Arc<Mutex<Option<AgentError>>>`) lives on `RunContext` (mirrors `TokioRunner`'s existing `Arc<Mutex<Outcome>>`). `LlmAgent::run` records the already-existing structured error into the slot at each failure site. `RunResultStreaming::collect`/`collect_typed` **drain the stream fully, then read the slot** — this ordering is load-bearing because three of the six failure sites record *after* the `RunFailed` event is yielded. `TokioRunner` clones the slot handle before moving `ctx` and hands it to the streaming wrapper.

**Tech Stack:** Rust, `async_trait`, `async_stream`, `futures`, `tokio`, `thiserror`, `anyhow`. Workspace crates `paigasus-helikon-core` and `paigasus-helikon-runtime-tokio`.

**Spec:** `docs/superpowers/specs/2026-05-29-structured-agenterror-runner-boundary-design.md`

---

## File Structure

| File | Change |
|------|--------|
| `crates/paigasus-helikon-core/src/agent.rs` | Add `FailureSlot` type (+ `Send+Sync` assertion); record into the slot at 3 direct failure sites + the loop driver's `Terminate` arm. |
| `crates/paigasus-helikon-core/src/context.rs` | Add `failure: FailureSlot` field + `failure_handle()` accessor on `RunContext`. |
| `crates/paigasus-helikon-core/src/runner.rs` | Add `RunResultStreaming::failure` + `with_failure()`; switch `collect`/`collect_typed` to drain-then-read with slot preference. |
| `crates/paigasus-helikon-runtime-tokio/src/lib.rs` | Clone the slot handle before moving `ctx`; build the streaming wrapper via `with_failure` in `run()` and `run_streamed()`. |
| `crates/paigasus-helikon-core/tests/failure_slot.rs` | **New.** Boundary/integration tests (slot preference, drain-then-read regression, end-to-end via `LlmAgent`). |
| `crates/paigasus-helikon-runtime-tokio/tests/run_error.rs` | **New.** `TokioRunner.run` failing model → `RunError::Agent(AgentError::Model(..))`. |
| `crates/paigasus-helikon-core/Cargo.toml`, root `Cargo.toml`, `crates/paigasus-helikon-core/CHANGELOG.md` | Release prep: core `0.2.1 → 0.2.2`, workspace pin, changelog. |

**Notes that prevent rework:**
- `lib.rs` re-exports each module with `pub use agent::*;` — a `pub` `FailureSlot` in `agent.rs` is auto-exported. **No `lib.rs` edit needed.**
- `AgentError` is defined in `agent.rs`, so `FailureSlot` referencing it needs no import; but `agent.rs` does **not** currently import `Mutex` — add `use std::sync::{Arc, Mutex};`.
- `RunConfig` is `#[non_exhaustive]`, so external crates cannot struct-literal it (no `with_max_turns` builder exists). The `MaxTurnsExceeded` end-to-end test therefore lives in **core** (set `max_turns` via the agent builder), not in `runtime-tokio`.

---

## Task 1: Add the `FailureSlot` type (core)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (imports near line 7; new type appended after the `AgentError` enum, which ends at line 780)

- [ ] **Step 1: Write the failing test**

Append this module to the end of `crates/paigasus-helikon-core/src/agent.rs`:

```rust
#[cfg(test)]
mod failure_slot_tests {
    use super::{AgentError, FailureSlot};

    #[test]
    fn set_then_take_returns_the_error() {
        let slot = FailureSlot::new();
        assert!(slot.take().is_none(), "empty slot yields None");
        slot.set(AgentError::MaxTurnsExceeded(3));
        match slot.take() {
            Some(AgentError::MaxTurnsExceeded(n)) => assert_eq!(n, 3),
            other => panic!("expected MaxTurnsExceeded(3), got {other:?}"),
        }
        assert!(slot.take().is_none(), "take() drains the slot");
    }

    #[test]
    fn clone_shares_the_same_slot() {
        let a = FailureSlot::new();
        let b = a.clone();
        b.set(AgentError::NotImplemented { feature: "handoff" });
        assert!(
            matches!(a.take(), Some(AgentError::NotImplemented { feature: "handoff" })),
            "a clone observes a write through the original handle"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-core failure_slot_tests`
Expected: FAIL — `cannot find type FailureSlot in this scope` (compile error).

- [ ] **Step 3: Add the import**

In `crates/paigasus-helikon-core/src/agent.rs`, change line 7-10 from:

```rust
use async_trait::async_trait;
use futures_core::stream::BoxStream;

use crate::{GuardrailKind, Item, ModelError, RunContext, SessionError, TokenUsage, ToolError};
```

to:

```rust
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;

use crate::{GuardrailKind, Item, ModelError, RunContext, SessionError, TokenUsage, ToolError};
```

- [ ] **Step 4: Add the `FailureSlot` type**

Immediately after the `AgentError` enum (after its closing `}` at line 780), add:

```rust
/// Out-of-band carrier for a run's terminal structured [`AgentError`].
///
/// The [`crate::AgentEvent`] stream stays string-based
/// ([`crate::AgentEvent::RunFailed`]` { error: String }`) so it remains `Clone`
/// and snapshot-stable; the structured error rides this side-channel instead.
/// One slot lives on each [`RunContext`]; the agent records into it at the
/// moment of failure and a [`crate::Runner`] (or
/// [`crate::RunResultStreaming::collect`]) reads it **after the event stream is
/// fully drained** — see [`crate::RunResultStreaming::collect`] for why the
/// read must come after draining.
#[derive(Clone, Default)]
pub struct FailureSlot(Arc<Mutex<Option<AgentError>>>);

impl FailureSlot {
    /// Create an empty slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the terminal structured error. Called once per run, at any point
    /// before the stream terminates; last write wins.
    pub fn set(&self, err: AgentError) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(err);
    }

    /// Take the recorded error, if any. Read once at the boundary, after the
    /// event stream has been fully drained.
    pub fn take(&self) -> Option<AgentError> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

// A non-`Send`/`Sync` payload added to `AgentError` would silently break the
// agent's `BoxStream<'static, AgentEvent>: Send` bound far downstream. Fail here
// instead, with a clear pointer to the cause.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FailureSlot>();
};
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-core failure_slot_tests`
Expected: PASS (2 tests).

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/agent.rs
git commit -m "feat(core): SMA-346 add FailureSlot side-channel type"
```

---

## Task 2: Add the slot to `RunContext` (core)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs` (imports line 8; struct line 46-61; constructor line 67-83; new accessor)

- [ ] **Step 1: Write the failing test**

In `crates/paigasus-helikon-core/src/context.rs`, find the existing test module (`#[cfg(test)] mod runcontext_tests` at line 137) and add this test inside it (after the existing `use` lines at line 140):

```rust
    #[test]
    fn failure_handle_shares_the_context_slot() {
        use crate::AgentError;
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            crate::HookRegistry::new(),
            crate::TracerHandle::default(),
            crate::CancellationToken::new(),
        );
        let handle = ctx.failure_handle();
        handle.set(AgentError::MaxTurnsExceeded(2));
        // A second handle from the same ctx observes the write.
        assert!(matches!(
            ctx.failure_handle().take(),
            Some(AgentError::MaxTurnsExceeded(2))
        ));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-core failure_handle_shares_the_context_slot`
Expected: FAIL — `no method named failure_handle found for struct RunContext`.

- [ ] **Step 3: Import `FailureSlot`**

Change line 8 of `crates/paigasus-helikon-core/src/context.rs` from:

```rust
use crate::{Hook, RunConfig, Session, ToolContext};
```

to:

```rust
use crate::{FailureSlot, Hook, RunConfig, Session, ToolContext};
```

- [ ] **Step 4: Add the field**

In the `RunContext` struct (lines 46-61), add a field after `run_config` (line 60). The struct's closing brace is line 61; insert before it:

```rust
    /// Out-of-band carrier for the run's terminal structured [`crate::AgentError`].
    /// Written by [`crate::Agent::run`] at the moment of failure; read at the
    /// boundary by a [`crate::Runner`] / [`crate::RunResultStreaming`]. Like
    /// `run_config`, it is **not** projected into [`ToolContext`].
    failure: FailureSlot,
```

- [ ] **Step 5: Initialize it in the constructor**

In `RunContext::new` (lines 75-82), change the struct literal from:

```rust
        Self {
            user_ctx,
            session,
            hooks,
            tracer,
            cancel,
            run_config: None,
        }
```

to:

```rust
        Self {
            user_ctx,
            session,
            hooks,
            tracer,
            cancel,
            run_config: None,
            failure: FailureSlot::new(),
        }
```

- [ ] **Step 6: Add the accessor**

Immediately after the `run_config` accessor (line 109, the closing `}` of `run_config()`), add:

```rust
    /// Clone the handle to this run's [`FailureSlot`].
    ///
    /// A [`crate::Runner`] clones this **before** moving the context into
    /// [`crate::Agent::run`] (the same way it clones `cancel` / `session`), then
    /// reads the structured error after the run's event stream drains.
    pub fn failure_handle(&self) -> FailureSlot {
        self.failure.clone()
    }
```

- [ ] **Step 7: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-core failure_handle_shares_the_context_slot`
Expected: PASS.

- [ ] **Step 8: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/context.rs
git commit -m "feat(core): SMA-346 add failure slot + failure_handle to RunContext"
```

---

## Task 3: Record structured errors in `LlmAgent::run` (core)

This task records into the slot at all four failure pathways. It is verifiable now because Task 2 lets a test read the slot directly via `ctx.failure_handle()` after draining the raw stream — no `collect` changes yet.

**Files:**
- Create: `crates/paigasus-helikon-core/tests/failure_slot.rs`
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (stream body line 582+; sites at lines 614-620, 679-682, 686-692; `Terminate` arm at line 720)

- [ ] **Step 1: Write the failing tests**

Create `crates/paigasus-helikon-core/tests/failure_slot.rs`:

```rust
//! SMA-346: LlmAgent::run records the structured AgentError into the
//! RunContext failure slot at every terminal-failure pathway.

#[path = "common/mod.rs"]
mod common;

use common::{MockModel, MockTool};
use futures_util::stream::StreamExt as _;
use paigasus_helikon_core::{
    AgentError, AgentInput, CancellationToken, HookRegistry, LlmAgent, RunContext, Session,
    TracerHandle,
};
use std::sync::Arc;

// A no-op session local to this test (common::NoopSession is also available).
use common::NoopSession;

fn ctx() -> RunContext<()> {
    RunContext::new(
        Arc::new(()),
        Arc::new(NoopSession) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
}

/// Drain a raw agent event stream to exhaustion (discarding events).
async fn drain(mut stream: futures_core::stream::BoxStream<'static, paigasus_helikon_core::AgentEvent>) {
    while stream.next().await.is_some() {}
}

#[tokio::test]
async fn model_invoke_error_is_recorded_as_agenterror_model() {
    // Empty scripts => MockModel::invoke returns Err(ModelError::Other(..)).
    let model = MockModel::with_scripts(vec![]);
    let agent = LlmAgent::builder::<()>()
        .name("t")
        .shared_model(model)
        .instructions("go")
        .build();

    let ctx = ctx();
    let failure = ctx.failure_handle();
    let stream = agent
        .run(ctx, AgentInput::from_user_text("hi"))
        .await
        .expect("run starts");
    drain(stream).await;

    assert!(
        matches!(failure.take(), Some(AgentError::Model(_))),
        "model.invoke failure should land in the slot as AgentError::Model"
    );
}

#[tokio::test]
async fn max_turns_exceeded_is_recorded_as_structured_error() {
    use paigasus_helikon_core::{FinishReason, ModelEvent};
    // Turn 0 emits a tool call; after the tool runs, next_turn (1) >= max_turns
    // (1) => the state machine fails with MaxTurnsExceeded and Terminates.
    let model = MockModel::with_scripts(vec![vec![
        ModelEvent::ToolCallDelta {
            call_id: "c1".into(),
            name: Some("noop".into()),
            args_delta: "{}".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]]);
    let tool = MockTool::new("noop", serde_json::json!({"ok": true}));
    let agent = LlmAgent::builder::<()>()
        .name("t")
        .shared_model(model)
        .instructions("go")
        .shared_tool(tool)
        .max_turns(1)
        .build();

    let ctx = ctx();
    let failure = ctx.failure_handle();
    let stream = agent
        .run(ctx, AgentInput::from_user_text("hi"))
        .await
        .expect("run starts");
    drain(stream).await;

    match failure.take() {
        Some(AgentError::MaxTurnsExceeded(n)) => assert_eq!(n, 1),
        other => panic!("expected MaxTurnsExceeded(1) in slot, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --test failure_slot`
Expected: FAIL — both tests find `None` in the slot (`assert!`/`panic!`), because recording is not wired yet. (The file must compile; if it does not, fix imports before proceeding.)

- [ ] **Step 3: Capture the failure handle at the top of the stream body**

In `crates/paigasus-helikon-core/src/agent.rs`, the stream begins at line 582 with `let stream = async_stream::stream! {`. Insert the handle capture as the first statement inside the block. Change line 582-584 from:

```rust
        let stream = async_stream::stream! {
            // Seed conversation: optional system message + user input.
            let mut conversation: Vec<crate::Item> = Vec::new();
```

to:

```rust
        let stream = async_stream::stream! {
            // SMA-346: structured failures are recorded here and read by the
            // boundary after the stream drains (see RunResultStreaming::collect).
            let failure = ctx.failure_handle();

            // Seed conversation: optional system message + user input.
            let mut conversation: Vec<crate::Item> = Vec::new();
```

- [ ] **Step 4: Record at the `model.invoke` site**

Change the `match model.invoke(...)` arm (lines 614-620) from:

```rust
                        let mut model_stream = match model.invoke(request, cancel).await {
                            Ok(s) => s,
                            Err(e) => {
                                yield crate::AgentEvent::RunFailed { error: e.to_string() };
                                return;
                            }
                        };
```

to:

```rust
                        let mut model_stream = match model.invoke(request, cancel).await {
                            Ok(s) => s,
                            Err(e) => {
                                let msg = e.to_string();
                                failure.set(crate::AgentError::Model(e));
                                yield crate::AgentEvent::RunFailed { error: msg };
                                return;
                            }
                        };
```

- [ ] **Step 5: Record at the model-stream error site**

Change the `Err(e)` arm inside the `while let Some(evt) = model_stream.next().await` loop (lines 679-682) from:

```rust
                                Err(e) => {
                                    yield crate::AgentEvent::RunFailed { error: e.to_string() };
                                    return;
                                }
```

to:

```rust
                                Err(e) => {
                                    let msg = e.to_string();
                                    failure.set(crate::AgentError::Model(e));
                                    yield crate::AgentEvent::RunFailed { error: msg };
                                    return;
                                }
```

- [ ] **Step 6: Record at the `build_items` site**

Change the `build_items` error arm (lines 686-692) from:

```rust
                        let items = match build_items(&agent_name, text, reasoning, tool_accum) {
                            Ok(items) => items,
                            Err(e) => {
                                yield crate::AgentEvent::RunFailed { error: e };
                                return;
                            }
                        };
```

to:

```rust
                        let items = match build_items(&agent_name, text, reasoning, tool_accum) {
                            Ok(items) => items,
                            Err(e) => {
                                failure.set(crate::AgentError::Other(anyhow::anyhow!(e.clone())));
                                yield crate::AgentEvent::RunFailed { error: e };
                                return;
                            }
                        };
```

- [ ] **Step 7: Record state-machine failures at the `Terminate` arm**

Change the `Terminate` arm (line 720) from:

```rust
                    crate::NextAction::Terminate => return,
```

to:

```rust
                    crate::NextAction::Terminate => {
                        // On a terminal failure the driver left the structured
                        // error in loop_state; hand it to the slot. (Every
                        // LoopState::Failed branch in loop_state.rs Terminates,
                        // so this is the single capture point for all of them.)
                        // This runs AFTER the RunFailed event was yielded, which
                        // is why the boundary must drain-then-read.
                        if let crate::LoopState::Failed(err) = loop_state {
                            failure.set(err);
                        }
                        return;
                    }
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core --test failure_slot`
Expected: PASS (2 tests).

- [ ] **Step 9: Run the full core test suite (no regressions)**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS — existing loop/streaming/snapshot tests unaffected (events are unchanged).

- [ ] **Step 10: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/tests/failure_slot.rs
git commit -m "feat(core): SMA-346 record structured AgentError into the failure slot"
```

---

## Task 4: Surface the structured error from `RunResultStreaming` (core)

Adds the `failure` field + `with_failure()` and switches `collect`/`collect_typed` to **drain-then-read**. This is the load-bearing read-ordering fix.

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs` (import line 13; struct lines 178-181; `new` lines 185-187; `collect` lines 197-231; `collect_typed` lines 245-300)
- Modify: `crates/paigasus-helikon-core/tests/failure_slot.rs` (add boundary tests)

- [ ] **Step 1: Write the failing tests**

Append to `crates/paigasus-helikon-core/tests/failure_slot.rs`:

```rust
// ── Boundary: RunResultStreaming surfaces the structured error ──────────────

use futures_util::stream;
// AgentError is already imported at the top of this file.
use paigasus_helikon_core::{AgentEvent, FailureSlot, RunError, RunResultStreaming};

fn run_failed_stream() -> futures_core::stream::BoxStream<'static, AgentEvent> {
    Box::pin(stream::iter(vec![AgentEvent::RunFailed {
        error: "max turns (1) exceeded".into(),
    }]))
}

#[tokio::test]
async fn collect_prefers_slot_over_string() {
    let slot = FailureSlot::new();
    slot.set(AgentError::MaxTurnsExceeded(1));
    let err = RunResultStreaming::with_failure(run_failed_stream(), slot)
        .collect()
        .await
        .expect_err("run failed");
    assert!(
        matches!(err, RunError::Agent(AgentError::MaxTurnsExceeded(1))),
        "expected RunError::Agent(MaxTurnsExceeded(1)), got {err:?}"
    );
}

#[tokio::test]
async fn collect_without_slot_falls_back_to_string() {
    let err = RunResultStreaming::new(run_failed_stream())
        .collect()
        .await
        .expect_err("run failed");
    assert!(
        matches!(err, RunError::Other(_)),
        "no slot => opaque string error, got {err:?}"
    );
}

#[tokio::test]
async fn collect_typed_prefers_slot() {
    #[derive(serde::Deserialize)]
    struct Answer {
        #[allow(dead_code)]
        value: u32,
    }
    let slot = FailureSlot::new();
    slot.set(AgentError::NotImplemented { feature: "handoff" });
    let err = RunResultStreaming::with_failure(run_failed_stream(), slot)
        .collect_typed::<Answer>()
        .await
        .expect_err("run failed");
    assert!(
        matches!(err, AgentError::NotImplemented { feature: "handoff" }),
        "expected NotImplemented, got {err:?}"
    );
}

/// Cross-carrier invariant: for InvalidStructuredOutput, the slot (primary) and
/// the StructuredOutputFailed-event fallback yield the same error at the boundary.
#[tokio::test]
async fn collect_typed_slot_matches_event_fallback() {
    #[derive(serde::Deserialize)]
    struct Answer {
        #[allow(dead_code)]
        value: u32,
    }
    let errs = vec!["missing field `value`".to_string()];
    let text = "{}".to_string();

    let events = || {
        Box::pin(stream::iter(vec![
            AgentEvent::StructuredOutputFailed {
                schema_errors: errs.clone(),
                final_text: text.clone(),
            },
            AgentEvent::RunFailed {
                error: "invalid structured output".into(),
            },
        ]))
    };

    // Primary: slot carries the structured error.
    let slot = FailureSlot::new();
    slot.set(AgentError::InvalidStructuredOutput {
        schema_errors: errs.clone(),
        final_text: text.clone(),
    });
    let from_slot = RunResultStreaming::with_failure(events(), slot)
        .collect_typed::<Answer>()
        .await
        .expect_err("err");

    // Fallback: no slot => reconstruct from the StructuredOutputFailed event.
    let from_event = RunResultStreaming::new(events())
        .collect_typed::<Answer>()
        .await
        .expect_err("err");

    for err in [from_slot, from_event] {
        match err {
            AgentError::InvalidStructuredOutput {
                schema_errors,
                final_text,
            } => {
                assert_eq!(schema_errors, errs);
                assert_eq!(final_text, text);
            }
            other => panic!("expected InvalidStructuredOutput, got {other:?}"),
        }
    }
}

/// End-to-end drain-then-read: a real max-turns run, collected via with_failure,
/// surfaces the structured error. A naive early-return collect() regresses this
/// to RunError::Other (the state-machine error is recorded after RunFailed).
#[tokio::test]
async fn end_to_end_max_turns_collects_as_run_error_agent() {
    use paigasus_helikon_core::{FinishReason, ModelEvent};
    let model = MockModel::with_scripts(vec![vec![
        ModelEvent::ToolCallDelta {
            call_id: "c1".into(),
            name: Some("noop".into()),
            args_delta: "{}".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]]);
    let tool = MockTool::new("noop", serde_json::json!({"ok": true}));
    let agent = LlmAgent::builder::<()>()
        .name("t")
        .shared_model(model)
        .instructions("go")
        .shared_tool(tool)
        .max_turns(1)
        .build();

    let ctx = ctx();
    let failure = ctx.failure_handle();
    let stream = agent
        .run(ctx, AgentInput::from_user_text("hi"))
        .await
        .expect("run starts");
    let err = RunResultStreaming::with_failure(stream, failure)
        .collect()
        .await
        .expect_err("run failed");
    assert!(
        matches!(err, RunError::Agent(AgentError::MaxTurnsExceeded(1))),
        "expected RunError::Agent(MaxTurnsExceeded(1)), got {err:?}"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --test failure_slot`
Expected: FAIL — `no function or associated item named with_failure found` (compile error).

- [ ] **Step 3: Import `FailureSlot` in `runner.rs`**

Change line 13 from:

```rust
use crate::{Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext};
```

to:

```rust
use crate::{Agent, AgentError, AgentEvent, AgentInput, ContentPart, FailureSlot, Item, RunContext};
```

- [ ] **Step 4: Add the field and `with_failure` constructor**

Change the struct (lines 178-181) from:

```rust
pub struct RunResultStreaming {
    /// The event stream produced by the agent's run.
    pub events: futures_core::stream::BoxStream<'static, crate::AgentEvent>,
}
```

to:

```rust
pub struct RunResultStreaming {
    /// The event stream produced by the agent's run.
    pub events: futures_core::stream::BoxStream<'static, crate::AgentEvent>,
    /// Side-channel carrying the run's terminal structured [`AgentError`], when
    /// a runner wired one in via [`RunResultStreaming::with_failure`]. Read only
    /// after the stream is fully drained. `None` for a bare
    /// [`RunResultStreaming::new`], in which case `collect` falls back to the
    /// string error from [`AgentEvent::RunFailed`].
    failure: Option<FailureSlot>,
}
```

Then change `new` (lines 185-187) from:

```rust
    /// Wrap an event stream.
    pub fn new(events: futures_core::stream::BoxStream<'static, crate::AgentEvent>) -> Self {
        Self { events }
    }
```

to:

```rust
    /// Wrap an event stream with no structured-error side-channel. `collect`
    /// then surfaces failures as the opaque string from `RunFailed`.
    pub fn new(events: futures_core::stream::BoxStream<'static, crate::AgentEvent>) -> Self {
        Self {
            events,
            failure: None,
        }
    }

    /// Wrap an event stream together with the [`FailureSlot`] the agent records
    /// its terminal structured [`AgentError`] into, so `collect` /
    /// `collect_typed` surface `RunError::Agent` / the real `AgentError` instead
    /// of the opaque string.
    pub fn with_failure(
        events: futures_core::stream::BoxStream<'static, crate::AgentEvent>,
        failure: FailureSlot,
    ) -> Self {
        Self {
            events,
            failure: Some(failure),
        }
    }
```

- [ ] **Step 5: Rewrite `collect` to drain-then-read**

Replace the entire `collect` method body (lines 197-231) with:

```rust
    pub async fn collect(mut self) -> Result<RunResult, RunError> {
        use futures_util::stream::StreamExt;
        let mut events = Vec::new();
        let mut final_output = String::new();
        let mut usage = crate::TokenUsage::default();
        // Capture the RunFailed string but keep draining: state-machine failures
        // record their structured error AFTER yielding RunFailed, so the slot is
        // only guaranteed populated once the stream ends.
        let mut failed: Option<String> = None;

        while let Some(ev) = self.events.next().await {
            match &ev {
                crate::AgentEvent::MessageOutput {
                    item: crate::Item::AssistantMessage { content, .. },
                } => {
                    final_output.clear();
                    for part in content {
                        if let crate::ContentPart::Text { text } = part {
                            final_output.push_str(text);
                        }
                    }
                }
                crate::AgentEvent::RunCompleted { usage: u } => usage = *u,
                crate::AgentEvent::RunFailed { error } => {
                    failed = Some(error.clone());
                }
                _ => {}
            }
            events.push(ev);
        }

        if let Some(err_msg) = failed {
            if let Some(err) = self.failure.as_ref().and_then(FailureSlot::take) {
                return Err(RunError::Agent(err));
            }
            return Err(RunError::Other(anyhow::anyhow!(err_msg)));
        }

        Ok(RunResult {
            final_output,
            events,
            usage,
        })
    }
```

- [ ] **Step 6: Rewrite `collect_typed` to drain-then-read**

Replace the entire `collect_typed` method body (lines 245-300) with:

```rust
    pub async fn collect_typed<T>(mut self) -> Result<RunResult<T>, AgentError>
    where
        T: serde::de::DeserializeOwned,
    {
        use futures_util::stream::StreamExt;
        let mut events = Vec::new();
        let mut final_text = String::new();
        let mut usage = crate::TokenUsage::default();
        let mut structured_err: Option<(Vec<String>, String)> = None;
        let mut failed: Option<String> = None;

        while let Some(ev) = self.events.next().await {
            match &ev {
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage { content, .. },
                } => {
                    final_text.clear();
                    for part in content {
                        if let ContentPart::Text { text } = part {
                            final_text.push_str(text);
                        }
                    }
                }
                AgentEvent::RunCompleted { usage: u } => usage = *u,
                AgentEvent::StructuredOutputFailed {
                    schema_errors,
                    final_text: ft,
                } => {
                    structured_err = Some((schema_errors.clone(), ft.clone()));
                }
                AgentEvent::RunFailed { error } => {
                    failed = Some(error.clone());
                }
                _ => {}
            }
            events.push(ev);
        }

        if let Some(err_msg) = failed {
            // Primary: the structured side-channel (populated post-drain).
            if let Some(err) = self.failure.as_ref().and_then(FailureSlot::take) {
                return Err(err);
            }
            // Fallback 1: reconstruct InvalidStructuredOutput from its event.
            if let Some((schema_errors, final_text)) = structured_err {
                return Err(AgentError::InvalidStructuredOutput {
                    schema_errors,
                    final_text,
                });
            }
            // Fallback 2: the opaque string.
            return Err(AgentError::Other(anyhow::anyhow!(err_msg)));
        }

        let final_output = serde_json::from_str::<T>(final_text.trim()).map_err(|e| {
            AgentError::Other(anyhow::anyhow!(
                "collect_typed: failed to deserialize final output: {e}"
            ))
        })?;
        Ok(RunResult {
            final_output,
            events,
            usage,
        })
    }
```

- [ ] **Step 7: Run the new tests**

Run: `cargo test -p paigasus-helikon-core --test failure_slot`
Expected: PASS (all tests including the new boundary + end-to-end ones).

- [ ] **Step 8: Run the existing `collect_typed` tests (backward compat)**

Run: `cargo test -p paigasus-helikon-core --test collect_typed`
Expected: PASS — `collect_typed_returns_struct`, `collect_typed_maps_structured_failure`, `collect_typed_maps_plain_run_failure_to_other` all green (the no-slot path preserves prior behavior).

- [ ] **Step 9: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/runner.rs crates/paigasus-helikon-core/tests/failure_slot.rs
git commit -m "feat(core): SMA-346 surface structured AgentError from RunResultStreaming"
```

---

## Task 5: Wire the slot through `TokioRunner` (runtime-tokio)

**Files:**
- Create: `crates/paigasus-helikon-runtime-tokio/tests/run_error.rs`
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs` (`run` lines 101-128; `run_streamed` lines 130-177)

- [ ] **Step 1: Write the failing test**

Create `crates/paigasus-helikon-runtime-tokio/tests/run_error.rs`:

```rust
//! SMA-346: TokioRunner surfaces the structured AgentError as RunError::Agent.

#[path = "common/mod.rs"]
mod common;

use std::time::Duration;

use common::{noop_run_context, run_context_with_cancel, text_agent, MockModel, PendingModel};
use paigasus_helikon_core::{AgentError, AgentInput, CancellationToken, RunConfig, RunError};
use paigasus_helikon_runtime_tokio::TokioRunner;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_surfaces_model_error_as_run_error_agent() {
    // Empty scripts => model.invoke errors => AgentError::Model recorded.
    let agent = text_agent(MockModel::with_scripts(vec![]), Vec::new());
    let err = TokioRunner
        .run(
            &agent,
            noop_run_context(),
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        )
        .await
        .expect_err("run should fail");
    assert!(
        matches!(err, RunError::Agent(AgentError::Model(_))),
        "expected RunError::Agent(AgentError::Model(..)), got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_still_maps_to_run_error_cancelled() {
    // Cancel/timeout stay runner-level (sourced from Outcome, not the slot).
    let cancel = CancellationToken::new();
    let ctx = run_context_with_cancel(cancel.clone());
    let agent = text_agent(std::sync::Arc::new(PendingModel), Vec::new());
    let res = tokio::time::timeout(Duration::from_secs(5), async {
        let run = TokioRunner.run(
            &agent,
            ctx,
            AgentInput::from_user_text("go"),
            RunConfig::default(),
        );
        let killer = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (r, _) = tokio::join!(run, killer);
        r
    })
    .await
    .expect("within 5s");
    assert!(
        matches!(res, Err(RunError::Cancelled)),
        "cancel must remain RunError::Cancelled, got {res:?}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_error`
Expected: FAIL — `run_surfaces_model_error_as_run_error_agent` gets `RunError::Other` (slot not yet wired into the runner); the cancel test passes already.

- [ ] **Step 3: Wire the slot into `run()`**

In `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, change `run()` (lines 108-118) from:

```rust
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
```

to:

```rust
        let timeout = config.timeout;
        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();
        // Clone the failure handle before moving ctx into agent.run, mirroring
        // cancel/session above. collect() reads it after the stream drains.
        let failure = ctx.failure_handle();

        let stream = agent.run(ctx, input).await?;
        let (controlled_stream, outcome) = controlled(stream, cancel, timeout);
        // Do NOT `?`-short-circuit before finalize: agent failures surface as
        // collect()=Err, and finalize must still run.
        let collected = RunResultStreaming::with_failure(controlled_stream, failure)
            .collect()
            .await;
        finalize(&session).await;
```

- [ ] **Step 4: Wire the slot into `run_streamed()`**

Change `run_streamed()` (lines 137-143) from:

```rust
        let timeout = config.timeout;
        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();

        let stream = agent.run(ctx, input).await?;
        let (mut controlled_stream, outcome) = controlled(stream, cancel, timeout);
```

to:

```rust
        let timeout = config.timeout;
        let ctx = ctx.with_run_config(config);
        let cancel = ctx.cancel().clone();
        let session = ctx.session().clone();
        let failure = ctx.failure_handle();

        let stream = agent.run(ctx, input).await?;
        let (mut controlled_stream, outcome) = controlled(stream, cancel, timeout);
```

Then change the return (line 176) from:

```rust
        Ok(RunResultStreaming::new(Box::pin(out)))
```

to:

```rust
        // A later `.collect()` on this streamed handle also surfaces structure.
        Ok(RunResultStreaming::with_failure(Box::pin(out), failure))
```

- [ ] **Step 5: Run the new test**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test run_error`
Expected: PASS (both tests).

- [ ] **Step 6: Run the full runtime-tokio suite (no regressions)**

Run: `cargo test -p paigasus-helikon-runtime-tokio`
Expected: PASS — `run_control.rs` (incl. cancel/timeout/finalize), `run_smoke.rs`, `run_streamed.rs` all green.

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-tokio --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-tokio/src/lib.rs crates/paigasus-helikon-runtime-tokio/tests/run_error.rs
git commit -m "feat(runtime-tokio): SMA-346 surface structured AgentError via TokioRunner"
```

---

## Task 6: Full workspace gate (snapshots + docs + clippy)

No code changes expected; this proves the constraint that `AgentEvent` is untouched and all CI gates pass.

**Files:** none (verification only).

- [ ] **Step 1: Confirm zero snapshot regeneration**

Run: `cargo test -p paigasus-helikon-core --test serde_roundtrip`
Expected: PASS with no `.snap.new` files created. Verify none appeared:

Run: `find crates/paigasus-helikon-core/tests/snapshots -name '*.snap.new'`
Expected: no output.

- [ ] **Step 2: Run every local CI gate (matches `.github/workflows/ci.yml`)**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: all PASS. The new public items (`FailureSlot`, `with_failure`, `failure_handle`) carry `///` docs, satisfying `missing_docs` and the doc job.

- [ ] **Step 3: Run the nightly doc-coverage gate**

Run:
```bash
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
```
Expected: PASS (≥ 80%). If it dips because of the new items, ensure each new `pub` item has a `///` doc comment (they do per Tasks 1, 2, 4) and re-run.

- [ ] **Step 4: Commit (only if any doc/format fixups were needed)**

```bash
git add -A
git commit -m "docs(core): SMA-346 doc fixups for failure-slot API"
```

(Skip if nothing changed.)

---

## Task 7: Release prep — core version bump for publish ordering

`runtime-tokio` (already released) now uses core API added in this PR (`FailureSlot`, `failure_handle`, `RunResultStreaming::with_failure`). At publish time `cargo publish --verify` builds the `runtime-tokio` tarball against the **registry** core, so core must be published first with the new API. This is the documented "ascending crate uses same-PR core API" caveat in `CLAUDE.md`. Bump core (patch) + the workspace pin + changelog so release-plz publishes core before runtime-tokio.

**Files:**
- Modify: `crates/paigasus-helikon-core/Cargo.toml` (line 4)
- Modify: root `Cargo.toml` (line 47)
- Modify: `crates/paigasus-helikon-core/CHANGELOG.md` (under `## [Unreleased]`)

- [ ] **Step 1: Bump the core crate version**

In `crates/paigasus-helikon-core/Cargo.toml`, change line 4 from:

```toml
version                = "0.2.1"
```

to:

```toml
version                = "0.2.2"
```

- [ ] **Step 2: Bump the workspace dependency pin**

In root `Cargo.toml`, change line 47 from:

```toml
paigasus-helikon-core                = { path = "crates/paigasus-helikon-core", version = "0.2.1" }
```

to:

```toml
paigasus-helikon-core                = { path = "crates/paigasus-helikon-core", version = "0.2.2" }
```

- [ ] **Step 3: Add the changelog entry**

In `crates/paigasus-helikon-core/CHANGELOG.md`, change the `## [Unreleased]` section (line 8) from:

```markdown
## [Unreleased]

## [0.2.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-core-v0.2.0...paigasus-helikon-core-v0.2.1) - 2026-05-29
```

to:

```markdown
## [Unreleased]

## [0.2.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-core-v0.2.1...paigasus-helikon-core-v0.2.2) - 2026-05-29

### Added

- *(core)* SMA-346 surface the structured `AgentError` at the runner boundary: add `FailureSlot`, `RunContext::failure_handle`, and `RunResultStreaming::with_failure`. `Runner::run` and `collect`/`collect_typed` now return `RunError::Agent(AgentError::…)` for agent failures instead of an opaque string; `AgentEvent::RunFailed { error: String }` is unchanged. Publishes the API that `paigasus-helikon-runtime-tokio` depends on in the same change.

## [0.2.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-core-v0.2.0...paigasus-helikon-core-v0.2.1) - 2026-05-29
```

- [ ] **Step 4: Verify the workspace still builds with the new pin**

Run: `cargo build --workspace --all-features`
Expected: PASS (the path dependency resolves locally; the version pin matches).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/Cargo.toml Cargo.toml crates/paigasus-helikon-core/CHANGELOG.md Cargo.lock
git commit -m "chore(release): SMA-346 bump core to 0.2.2 for runtime-tokio publish ordering"
```

- [ ] **Step 6: Release-plz sanity check (before opening the PR)**

The squashed PR title drives release-plz's version computation. Because core gains public items, a `feat(core): …` title would make release-plz compute a *minor* bump, which can conflict with the manual `0.2.2` patch pin. `CLAUDE.md` sanctions "patch for additive". Before merging, confirm release-plz will publish **core then runtime-tokio** (dependency order) and will not fight the manual `0.2.2` (run release-plz's dry-run if available, or have the maintainer confirm). If it conflicts, the documented fallback is to split into a core-first PR (let release-plz bump/publish core normally) then a runtime-tokio PR.

---

## PR

- [ ] **Open the PR** with a title satisfying `pr-title.yml` (full Conventional Commits + lowercase subject after the SMA prefix), e.g.:
  `feat(core): SMA-346 surface structured AgentError at the runner boundary`
- [ ] Ensure the design + plan docs are committed on the branch (they already are).
- [ ] Let CI run the six gates; Linear auto-closes SMA-346 on merge.

---

## Self-Review (completed during authoring)

**Spec coverage:**
- §1 `FailureSlot` → Task 1. §2 `RunContext` slot + accessor → Task 2. §3 recording at 3 direct sites + Terminate arm → Task 3. §4 `RunResultStreaming` + drain-then-read + with_failure + mapping → Task 4. §5 `TokioRunner` wiring → Task 5. §6 unchanged-`AgentEvent` constraint → Task 6 (snapshot check). Testing section → Tasks 3–5 (end-to-end MaxTurnsExceeded + Model error, slot preference, no-slot fallback, cross-carrier invariant, cancel/timeout still typed). Release sequencing → Task 7.
- The `NotImplemented` end-to-end case is covered as a **unit** slot-preference test (`collect_typed_prefers_slot`) rather than driven through the handoff state (not reachable via `MockModel` in SMA-314); the real Terminate-arm recording is exercised end-to-end by the `MaxTurnsExceeded` tests, which share the identical code path.

**Type consistency:** `FailureSlot::{new,set,take}`, `RunContext::failure_handle() -> FailureSlot`, `RunResultStreaming::with_failure(events, failure)`, `RunError::Agent(AgentError)` are used identically across Tasks 1–5.

**Placeholder scan:** none — every step shows exact code/commands and expected output.
