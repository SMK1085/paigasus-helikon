# SMA-314 — LlmAgent + LoopState Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the concrete `LlmAgent<Ctx, M>` and an explicit `LoopState` state machine in `paigasus-helikon-core`, satisfying SMA-314's three acceptance criteria with `MockModel` / `MockTool` fixtures.

**Architecture:** Pure `transition(state, input, ctx) → outcome` function lives in `loop_state.rs` (no async, no tokio). `LlmAgent::run` is a thin `async_stream::stream!` block that calls `transition`, awaits the returned `NextAction` (model invocation or `join_all` over tool invocations), and feeds the result back as `TransitionInput`. Four placeholder types (`ModelRequest`, `AgentInput`, `RunConfig`, `RunResultStreaming`) graduate just far enough to drive the loop — every other field stays for SMA-321 (TokioRunner) and SMA-316 / SMA-317 (provider crates).

**Tech Stack:** Rust 1.75 (workspace MSRV), `async-trait` for object-safe traits, `futures-core` + `futures-util` for `BoxStream` + `join_all`, `async-stream 0.3` for the `stream!` macro, `tokio` (dev-dep only) for `#[tokio::test]` and `tokio::sync::Barrier`, `insta` for event-sequence snapshots.

**Spec:** `docs/superpowers/specs/2026-05-21-sma-314-llmagent-loopstate-design.md`

**Branch:** `feature/sma-314-llmagent-explicit-loopstate-state-machine` (already created on this worktree)

**Commit convention:** Every code commit uses `feat(core): SMA-314 <message>`. The local commit-msg hook enforces Conventional Commits with this scope allowlist; the `pr-title.yml` workflow re-validates on PR. Never use `--no-verify`. The spec commit (already landed, `8a6066d`) used `docs(spec): SMA-314 …` per the same hook rules.

---

## Phase A — Carrier-type promotions (no behavior yet)

Goal: graduate the four placeholder types just enough to satisfy the loop driver's needs, and update the `object_safety.rs` test so the existing SMA-312 AC stays green. Each task is type-shape-only — no `LlmAgent`, no `transition`, no integration tests yet.

### Task A1: Add `futures-util` + `async-stream` to workspace deps

**Files:**
- Modify: `Cargo.toml` (workspace root, `[workspace.dependencies]` block)

- [ ] **Step 1: Add the two pins**

Insert two lines into `[workspace.dependencies]` (alphabetical placement keeps the block tidy — `async-stream` after `async-trait`, `futures-util` after `futures-core`):

```toml
async-stream  = "0.3"
futures-util  = { version = "0.3", default-features = false, features = ["std"] }
```

- [ ] **Step 2: Verify cargo metadata resolves**

Run: `cargo metadata --format-version 1 --no-deps > /dev/null`
Expected: exits 0 with no output. (Confirms TOML parses and the new deps don't conflict.)

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "$(cat <<'EOF'
chore(workspace): SMA-314 pin futures-util and async-stream

Used by SMA-314's loop driver: futures-util for join_all over
parallel tool calls; async-stream for the stream! macro the driver
uses to yield AgentEvents executor-agnostically.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

The `chore(workspace)` type is correct here (not `feat`) because this commit only modifies workspace metadata — no crate gains public API. Matches the SMA-307 rule that touches-every-Cargo.toml commits must be `chore` or `docs`.

---

### Task A2: Wire the new deps into `paigasus-helikon-core`'s manifest

**Files:**
- Modify: `crates/paigasus-helikon-core/Cargo.toml`

- [ ] **Step 1: Add runtime + dev deps**

In `[dependencies]`, append:

```toml
futures-util = { workspace = true }
async-stream = { workspace = true }
```

In `[dev-dependencies]`, append:

```toml
tokio        = { workspace = true, features = ["macros", "rt-multi-thread", "time", "sync"] }
```

`tokio` is already pinned at the workspace level with `features = ["full"]` (from the SMA-304 bootstrap), so this dev-dep entry only declares which features `paigasus-helikon-core` uses — Cargo unions feature sets, so the workspace pin's `full` is what's actually compiled. Listing the narrow set documents intent.

- [ ] **Step 2: Verify the crate still compiles**

Run: `cargo check -p paigasus-helikon-core --all-features`
Expected: exits 0 with no warnings. No code uses the new deps yet — this just confirms they resolve.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-core/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 wire futures-util, async-stream, tokio dev-dep

Pulls in the deps SMA-314's loop driver needs. No code changes yet;
this commit makes the manifest ready for the LlmAgent::run impl.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task A3: Promote `ModelRequest` + add `ToolDef`, `ModelSettings` (`model.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs` (replace the placeholder `ModelRequest` block; add two new types)

- [ ] **Step 1: Replace the `ModelRequest` block**

Find the existing block in `model.rs`:

```rust
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModelRequest {}

impl ModelRequest {
    pub fn new() -> Self { Self::default() }
}
```

Replace it with:

```rust
/// The request envelope crossing the model boundary.
///
/// Carries the conversation, the tools available for the model to
/// invoke, and provider-tuning knobs. Field shape is the minimum SMA-314
/// needs to drive the loop; SMA-316 / SMA-317 add `tool_choice`,
/// `response_format`, `temperature`, and `previous_response_id`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModelRequest {
    /// The full accumulated conversation so far.
    pub messages: Vec<crate::Item>,
    /// Tool definitions the model may invoke this turn.
    pub tools: Vec<ToolDef>,
    /// Provider-tuning knobs.
    pub model_settings: ModelSettings,
}

impl ModelRequest {
    /// Construct an empty request. Callers populate fields directly.
    pub fn new() -> Self { Self::default() }
}

/// Owned snapshot of a [`crate::Tool`] for cross-async-boundary use
/// inside [`ModelRequest`].
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// Identifier the model uses when emitting a tool call.
    pub name: String,
    /// One-line tool description shown to the model.
    pub description: String,
    /// JSON Schema for the tool's argument object.
    pub schema: serde_json::Value,
}

/// Provider-tuning knobs (temperature, max tokens, sampling, ...).
///
/// Field shape lands with SMA-316 / SMA-317. Today this is a
/// `#[non_exhaustive]` placeholder so [`ModelRequest::model_settings`]
/// has a type.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ModelSettings {}

impl ModelSettings {
    /// Construct default model settings.
    pub fn new() -> Self { Self::default() }
}
```

- [ ] **Step 2: Verify the crate compiles**

Run: `cargo check -p paigasus-helikon-core --all-features`
Expected: exits 0.

- [ ] **Step 3: Verify clippy still passes for this crate**

Run: `cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings`
Expected: exits 0. (`object_safety.rs` may produce warnings or errors that we address in Task A6 — if clippy fails here only on `tests/object_safety.rs`, that's expected and resolved later.)

If clippy fails on a non-`tests/` file, stop and investigate before proceeding.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/model.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 promote ModelRequest with messages and tools

Adds messages: Vec<Item>, tools: Vec<ToolDef>, model_settings:
ModelSettings to ModelRequest. ToolDef and ModelSettings are new
public types. SMA-316/SMA-317 own the remaining ModelRequest fields
(tool_choice, response_format, temperature, previous_response_id).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task A4: Promote `RunConfig` + `RunResultStreaming` (`runner.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs`

- [ ] **Step 1: Replace `RunConfig` block**

Find:

```rust
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RunConfig {}
```

Replace with:

```rust
/// Per-run configuration.
///
/// SMA-314 ships only `max_turns`. SMA-321 (TokioRunner) adds
/// `timeout`, `parallel_tool_call_limit`, `retry_policy`, and
/// `cancellation`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunConfig {
    /// Maximum number of model turns before the loop fails with
    /// [`crate::AgentError::MaxTurnsExceeded`]. Default `16`.
    pub max_turns: u32,
}

impl Default for RunConfig {
    fn default() -> Self { Self { max_turns: 16 } }
}

impl RunConfig {
    /// Construct a default config (`max_turns = 16`).
    pub fn new() -> Self { Self::default() }
}
```

- [ ] **Step 2: Replace `RunResultStreaming` block**

Find:

```rust
#[non_exhaustive]
pub struct RunResultStreaming {}
```

Replace with:

```rust
/// Streaming counterpart of [`RunResult`].
///
/// Wraps the unified [`crate::AgentEvent`] stream emitted by an agent
/// and offers an `async fn collect` that drains the stream into a
/// `RunResult<String>`. Callers may consume `events` directly for raw
/// streaming.
pub struct RunResultStreaming {
    /// The event stream produced by the agent's run.
    pub events: futures_core::stream::BoxStream<'static, crate::AgentEvent>,
}

impl RunResultStreaming {
    /// Wrap an event stream.
    pub fn new(events: futures_core::stream::BoxStream<'static, crate::AgentEvent>) -> Self {
        Self { events }
    }

    /// Drain the stream and aggregate into a `RunResult<String>`.
    ///
    /// `final_output` is the concatenated text from every
    /// `AgentEvent::TokenDelta`. Structured-output callers go through
    /// `RunResult::<String>::parse_final::<T>()` (SMA-313).
    pub async fn collect(mut self) -> Result<RunResult, RunError> {
        use futures_util::stream::StreamExt;
        let mut events = Vec::new();
        let mut final_output = String::new();
        let mut usage = crate::TokenUsage::default();
        let mut failed: Option<String> = None;

        while let Some(ev) = self.events.next().await {
            match &ev {
                crate::AgentEvent::TokenDelta { text } => final_output.push_str(text),
                crate::AgentEvent::RunCompleted { usage: u } => usage = *u,
                crate::AgentEvent::RunFailed { error } => failed = Some(error.clone()),
                _ => {}
            }
            events.push(ev);
        }

        if let Some(e) = failed {
            return Err(RunError::Other(anyhow::anyhow!(e)));
        }

        Ok(RunResult { final_output, events, usage })
    }
}
```

- [ ] **Step 3: Verify the crate compiles**

Run: `cargo check -p paigasus-helikon-core --all-features`
Expected: exits 0.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/runner.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 promote RunConfig and RunResultStreaming

RunConfig gains max_turns (default 16). SMA-321 owns the rest of the
RunConfig field set (timeout, parallel_tool_call_limit, retry_policy,
cancellation). RunResultStreaming gains its real shape: events stream
plus async collect() that aggregates into RunResult<String>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task A5: Promote `AgentInput`, add `OutputType` + new `AgentError` variants (`agent.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs`

- [ ] **Step 1: Replace `AgentInput` block**

Find:

```rust
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AgentInput {}
```

Replace with:

```rust
/// User-supplied input that seeds the run.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AgentInput {
    /// The initial conversation. Typically one [`crate::Item::UserMessage`].
    pub messages: Vec<crate::Item>,
}

impl AgentInput {
    /// Construct an empty input. Populate `messages` directly.
    pub fn new() -> Self { Self::default() }

    /// Seed the run with one user text message — the common case.
    pub fn from_user_text(text: impl Into<String>) -> Self {
        Self {
            messages: vec![crate::Item::UserMessage {
                content: vec![crate::ContentPart::Text { text: text.into() }],
            }],
        }
    }
}

/// Structured-output type marker — the JSON Schema the model is asked
/// to produce.
///
/// SMA-320 promotes the typed-output path (`output_type::<T>()`
/// honesty); SMA-314 only defines the field type so [`LlmAgent`] has a
/// place to store it.
#[derive(Debug, Clone)]
pub struct OutputType {
    /// The JSON Schema the model should produce.
    pub schema: schemars::Schema,
}

impl OutputType {
    /// Construct from a type that derives [`schemars::JsonSchema`].
    pub fn from_schema<T: schemars::JsonSchema>() -> Self {
        Self { schema: schemars::schema_for!(T) }
    }
}
```

- [ ] **Step 2: Add new `AgentError` variants**

Find the existing `AgentError` enum and add two variants (keep `#[non_exhaustive]` and the existing variant order; add these before `Other(#[from] anyhow::Error)` so the catch-all stays last):

```rust
    /// New in SMA-314: `max_turns` budget exhausted.
    #[error("max turns ({0}) exceeded")]
    MaxTurnsExceeded(u32),

    /// New in SMA-314: reached a `LoopState` variant SMA-314 does not
    /// yet drive (handoff, compaction, approval).
    #[error("not yet implemented: {feature}")]
    NotImplemented {
        /// The unimplemented loop feature.
        feature: &'static str
    },
```

- [ ] **Step 3: Verify the crate compiles**

Run: `cargo check -p paigasus-helikon-core --all-features`
Expected: exits 0. `tests/object_safety.rs` may fail to compile here because it constructs `AgentInput {}`; that's expected — Task A6 fixes it.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 promote AgentInput, add OutputType and AgentError variants

AgentInput gains messages: Vec<Item> + AgentInput::from_user_text
convenience. New OutputType { schema: schemars::Schema } marker
(SMA-320 makes typed output honest). Two new AgentError variants:
MaxTurnsExceeded(u32) and NotImplemented { feature: &'static str }.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task A6: Update `tests/object_safety.rs` to use new constructors

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/object_safety.rs`

- [ ] **Step 1: Find any uses of `AgentInput {}`, `RunConfig {}`, `RunResultStreaming {}`, `ModelRequest {}`**

Run: `grep -n 'AgentInput\|RunConfig\|RunResultStreaming\|ModelRequest' crates/paigasus-helikon-core/tests/object_safety.rs`

For each occurrence that constructs the value, replace:

- `AgentInput {}` → `AgentInput::default()`
- `RunConfig {}` → `RunConfig::default()`
- `RunResultStreaming {}` → `RunResultStreaming::new(Box::pin(futures_util::stream::empty()))`
- `ModelRequest {}` → `ModelRequest::default()`

If `futures_util` is not imported, add `use futures_util::stream;` near the other `use` lines (or use the fully qualified path inline).

- [ ] **Step 2: Run the object-safety test**

Run: `cargo test -p paigasus-helikon-core --test object_safety`
Expected: PASS. The point of the test is the `Box<dyn Trait>` ascriptions — they should still compile.

- [ ] **Step 3: Run all tests for the crate**

Run: `cargo test -p paigasus-helikon-core --all-features`
Expected: PASS (existing SMA-312/313 tests still green).

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/tests/object_safety.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 update object-safety test to new constructors

Switches from RunConfig {} / AgentInput {} / RunResultStreaming {}
literals to the new ::default() / ::new(...) constructors after the
A3-A5 type promotions. The Box<dyn Trait> ascriptions (the actual
SMA-312 AC locks) are unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase B — Pure transition function (TDD)

Goal: build `loop_state.rs` types and the pure `transition(...)` function with deterministic unit tests. No tokio, no async. Each transition cell is added via a failing test → implementation → passing test cycle.

### Task B1: Create `loop_state.rs` with type definitions only

**Files:**
- Create: `crates/paigasus-helikon-core/src/loop_state.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs`

- [ ] **Step 1: Create the file with all types (no `transition` yet)**

Write `crates/paigasus-helikon-core/src/loop_state.rs`:

```rust
//! Explicit state machine for the agent loop.
//!
//! Per ADR *"Explicit `LoopState` enum, not a callback maze"*, the
//! state machine is data: a pure [`transition`] function takes the
//! current state plus the most recent input and returns the next
//! state, the events to emit, and the side effect to perform. Durable
//! runners (Temporal, AgentCore in later tickets) reuse the same
//! function with their own driver.

use crate::{
    AgentError, AgentEvent, ContentPart, FinishReason, Item,
    ModelRequest, ModelSettings, TokenUsage, ToolDef,
};

/// The explicit, observable state of the agent loop.
///
/// One variant per high-level phase. Does **not** derive `Clone`:
/// `Failed(AgentError)` wraps `anyhow::Error` (not `Clone`). The
/// transition function takes input and returns outcome by value; tests
/// use `assert_matches!` on `next_state` instead of equality.
#[derive(Debug)]
#[non_exhaustive]
pub enum LoopState {
    /// About to call the model for turn `turn`.
    CallingModel {
        /// Zero-indexed turn counter.
        turn: u32,
    },
    /// The model produced tool calls; about to execute them. `turn` is
    /// the turn that produced the calls — the next [`CallingModel`]
    /// state will be `turn + 1`.
    ExecutingTools {
        /// The tool calls to execute concurrently.
        calls: Vec<ToolCallRequest>,
        /// The turn that produced these calls.
        turn: u32,
    },
    /// Handing off to another agent.
    ///
    /// **Not driveable in SMA-314.** Reaching this variant via
    /// [`transition`] returns
    /// [`LoopState::Failed`]`([`AgentError::NotImplemented`]` { feature: "handoff" })`.
    ApplyingHandoff {
        /// Name of the target agent.
        target: String,
        /// Conversation transcript to hand off.
        transcript: Vec<Item>,
    },
    /// Compacting session history. **Not driveable in SMA-314.**
    Compacting,
    /// Awaiting approval for a sensitive tool call.
    /// **Not driveable in SMA-314.**
    NeedsApproval {
        /// The tool calls awaiting approval.
        pending: Vec<ToolCallRequest>,
    },
    /// Terminal: run completed successfully.
    Done(FinalOutput),
    /// Terminal: run failed.
    Failed(AgentError),
}

/// One tool call the model has requested. Pure data.
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    /// The provider-assigned call id (echoed back in `Item::ToolResult`).
    pub call_id: String,
    /// Tool name (matches [`crate::Tool::name`]).
    pub name: String,
    /// JSON-encoded arguments object.
    pub args: serde_json::Value,
}

/// Outcome of one tool execution. Errors are stringified so the
/// outcome implements `Clone` — `ToolError` carries `anyhow::Error`,
/// which is not `Clone`.
#[derive(Debug, Clone)]
pub struct ToolCallOutcome {
    /// The call id this outcome corresponds to.
    pub call_id: String,
    /// Either the tool's content output or a stringified error.
    pub result: Result<Vec<ContentPart>, String>,
}

/// Final assistant content + aggregated token usage at termination.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FinalOutput {
    /// The terminal assistant content.
    pub content: Vec<ContentPart>,
    /// Aggregated token usage across all turns.
    pub usage: TokenUsage,
}

impl FinalOutput {
    /// Concatenate all `ContentPart::Text` parts. This is the
    /// canonical rendering that feeds `RunResult.final_output` when
    /// `T = String`.
    pub fn as_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// Data fed *into* the next [`transition`] call.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TransitionInput {
    /// Seeds the loop with the initial conversation.
    Start {
        /// The user's input messages.
        messages: Vec<Item>,
    },
    /// One model turn aggregated.
    ModelResponse {
        /// Items produced this turn (assistant message + tool calls).
        items: Vec<Item>,
        /// Token usage for this turn.
        usage: TokenUsage,
        /// Why the model stopped emitting tokens.
        finish_reason: FinishReason,
    },
    /// All tool calls for one turn have completed.
    ToolResults {
        /// Per-call outcomes.
        outcomes: Vec<ToolCallOutcome>,
    },
}

/// Side effect the async driver must run before the next transition.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum NextAction {
    /// Call the model with this request; produce a `ModelResponse`.
    CallModel {
        /// The request to send.
        request: ModelRequest,
    },
    /// Execute these tool calls concurrently; produce a `ToolResults`.
    ExecuteTools {
        /// The calls to fan out.
        calls: Vec<ToolCallRequest>,
    },
    /// The current state is terminal; stop driving.
    Terminate,
}

/// What [`transition`] needs to know about the agent and config for
/// one step. Doesn't carry user `Ctx` — that's the driver's concern.
pub struct TransitionCtx<'a> {
    /// Tool definitions available this run.
    pub tools: &'a [ToolDef],
    /// Provider-tuning knobs.
    pub model_settings: &'a ModelSettings,
    /// Maximum number of turns before the loop fails.
    pub max_turns: u32,
    /// The driver's accumulated conversation so far. The driver
    /// appends incoming items before calling [`transition`] and passes
    /// the slice in — [`transition`] reads but never mutates.
    pub conversation: &'a [Item],
}

/// One transition step's result. Not `Clone` (carries `LoopState`).
#[derive(Debug)]
pub struct TransitionOutcome {
    /// The state after this step.
    pub next_state: LoopState,
    /// Events to yield through the driver's event stream.
    pub events: Vec<AgentEvent>,
    /// Side effect the driver must run before the next step.
    pub next_action: NextAction,
}

/// Pure state-machine step. **No async, no tokio, no IO.**
///
/// Given the current state and the most recent input, produce the
/// next state, the events to emit, and the side effect to perform.
/// Resumable by construction: a durable runner can persist
/// [`LoopState`] plus the accumulated conversation and rehydrate the
/// loop at any transition boundary.
pub fn transition(
    state: &LoopState,
    input: TransitionInput,
    ctx: &TransitionCtx<'_>,
) -> TransitionOutcome {
    // Implementation lands in subsequent tasks via TDD.
    let _ = (state, input, ctx);
    unimplemented!("transition cases implemented in Phase B tasks B2-B7")
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

Find the `pub mod ...` block in `crates/paigasus-helikon-core/src/lib.rs` and add `loop_state` in alphabetical order:

```rust
pub mod agent;
pub mod context;
pub mod guardrail;
pub mod hook;
pub mod item;
pub mod loop_state;        // ← NEW
pub mod model;
pub mod runner;
pub mod session;
pub mod tool;
```

Then the matching `pub use` (also alphabetical):

```rust
pub use agent::*;
pub use context::*;
pub use guardrail::*;
pub use hook::*;
pub use item::*;
pub use loop_state::*;     // ← NEW
pub use model::*;
pub use runner::*;
pub use session::*;
pub use tool::*;
```

- [ ] **Step 3: Verify the crate compiles**

Run: `cargo check -p paigasus-helikon-core --all-features`
Expected: exits 0. The `unimplemented!()` in `transition` is fine — it compiles, just panics if called.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs crates/paigasus-helikon-core/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 add loop_state module type definitions

LoopState, ToolCallRequest, ToolCallOutcome, FinalOutput,
TransitionInput, NextAction, TransitionCtx, TransitionOutcome. The
transition() function exists but is unimplemented!() — case rules
land via TDD in the next tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B2: TDD case 1 — `Start` seeds turn 0

**Files:**
- Create: `crates/paigasus-helikon-core/tests/transition_unit.rs`
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/paigasus-helikon-core/tests/transition_unit.rs`:

```rust
//! Pure-function unit tests on `transition(...)`. No tokio, no async,
//! no IO. Locks SMA-314's state-machine determinism guarantees.

use std::assert_matches::assert_matches;

use paigasus_helikon_core::{
    AgentEvent, ContentPart, FinishReason, Item, LoopState, ModelSettings,
    NextAction, TokenUsage, TransitionCtx, TransitionInput, transition,
};

fn ctx_with(max_turns: u32, conversation: &[Item]) -> TransitionCtx<'_> {
    TransitionCtx {
        tools: &[],
        model_settings: &ModelSettings::new(),
        max_turns,
        conversation,
    }
}

#[test]
fn start_seeds_turn_zero_and_emits_call_model() {
    let state = LoopState::CallingModel { turn: 0 };
    let user_msg = Item::UserMessage {
        content: vec![ContentPart::Text { text: "hi".into() }],
    };
    let conversation = vec![user_msg.clone()];
    let input = TransitionInput::Start { messages: vec![user_msg] };

    let outcome = transition(&state, input, &ctx_with(16, &conversation));

    assert_matches!(outcome.next_state, LoopState::CallingModel { turn: 0 });
    assert_matches!(outcome.next_action, NextAction::CallModel { .. });
    assert_eq!(outcome.events.len(), 1);
    assert_matches!(&outcome.events[0], AgentEvent::TurnStarted { turn: 0 });
}
```

Note: `assert_matches!` requires the unstable `assert_matches` feature. Use the stable alternative — match macro:

```rust
use paigasus_helikon_core::{
    AgentEvent, ContentPart, FinishReason, Item, LoopState, ModelSettings,
    NextAction, TokenUsage, TransitionCtx, TransitionInput, transition,
};

macro_rules! assert_matches {
    ($expr:expr, $pat:pat $(,)?) => {
        let val = $expr;
        match val {
            $pat => {}
            other => panic!("assertion failed: {other:?} does not match {}", stringify!($pat)),
        }
    };
}

fn ctx_with(max_turns: u32, conversation: &[Item]) -> TransitionCtx<'_> {
    TransitionCtx {
        tools: &[],
        model_settings: &ModelSettings::new(),
        max_turns,
        conversation,
    }
}

#[test]
fn start_seeds_turn_zero_and_emits_call_model() {
    let state = LoopState::CallingModel { turn: 0 };
    let user_msg = Item::UserMessage {
        content: vec![ContentPart::Text { text: "hi".into() }],
    };
    let conversation = vec![user_msg.clone()];
    let input = TransitionInput::Start { messages: vec![user_msg] };

    let outcome = transition(&state, input, &ctx_with(16, &conversation));

    assert_matches!(outcome.next_state, LoopState::CallingModel { turn: 0 });
    assert_matches!(outcome.next_action, NextAction::CallModel { .. });
    assert_eq!(outcome.events.len(), 1);
    assert_matches!(&outcome.events[0], AgentEvent::TurnStarted { turn: 0 });
}
```

(Define `assert_matches!` as a local macro at the top of the file because the std macro is unstable on 1.75.)

- [ ] **Step 2: Run the test — expect FAIL with `unimplemented`**

Run: `cargo test -p paigasus-helikon-core --test transition_unit -- start_seeds_turn_zero_and_emits_call_model --nocapture`
Expected: FAIL with panic message `not implemented: transition cases implemented in Phase B tasks B2-B7`.

- [ ] **Step 3: Implement the CallingModel + Start case**

In `crates/paigasus-helikon-core/src/loop_state.rs`, replace the `transition` function body:

```rust
pub fn transition(
    state: &LoopState,
    input: TransitionInput,
    ctx: &TransitionCtx<'_>,
) -> TransitionOutcome {
    match (state, input) {
        // Start seeds the loop: emit TurnStarted, request CallModel.
        (LoopState::CallingModel { turn }, TransitionInput::Start { .. })
            if *turn < ctx.max_turns =>
        {
            let request = ModelRequest {
                messages: ctx.conversation.to_vec(),
                tools: ctx.tools.to_vec(),
                model_settings: ctx.model_settings.clone(),
            };
            TransitionOutcome {
                next_state: LoopState::CallingModel { turn: *turn },
                events: vec![AgentEvent::TurnStarted { turn: *turn }],
                next_action: NextAction::CallModel { request },
            }
        }
        // Other cases land in subsequent tasks.
        (s, i) => TransitionOutcome {
            next_state: LoopState::Failed(AgentError::Other(anyhow::anyhow!(
                "invalid transition: {s:?} ← {i:?}"
            ))),
            events: vec![AgentEvent::RunFailed {
                error: format!("invalid transition: {s:?} ← {i:?}"),
            }],
            next_action: NextAction::Terminate,
        },
    }
}
```

- [ ] **Step 4: Run the test — expect PASS**

Run: `cargo test -p paigasus-helikon-core --test transition_unit -- start_seeds_turn_zero_and_emits_call_model`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs crates/paigasus-helikon-core/tests/transition_unit.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 transition handles CallingModel + Start

First transition case: a Start input on CallingModel{turn} (within
max_turns budget) emits TurnStarted and returns a CallModel action
with a freshly-built ModelRequest. Unit test in transition_unit.rs.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B3: TDD case 2 — Pure assistant response terminates

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/transition_unit.rs`
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs`

- [ ] **Step 1: Add the failing test**

Append to `tests/transition_unit.rs`:

```rust
#[test]
fn model_response_without_tool_calls_terminates() {
    let state = LoopState::CallingModel { turn: 0 };
    let assistant = Item::AssistantMessage {
        content: vec![ContentPart::Text { text: "hello".into() }],
        agent: Some("test".into()),
    };
    let conversation = vec![]; // not used in this case
    let input = TransitionInput::ModelResponse {
        items: vec![assistant.clone()],
        usage: TokenUsage::default(),
        finish_reason: FinishReason::Stop,
    };

    let outcome = transition(&state, input, &ctx_with(16, &conversation));

    assert_matches!(outcome.next_state, LoopState::Done(_));
    assert_matches!(outcome.next_action, NextAction::Terminate);
    // Expected events: MessageOutput { item: AssistantMessage }, RunCompleted { .. }.
    assert_eq!(outcome.events.len(), 2);
    assert_matches!(&outcome.events[0], AgentEvent::MessageOutput { .. });
    assert_matches!(&outcome.events[1], AgentEvent::RunCompleted { .. });
}
```

- [ ] **Step 2: Run — expect FAIL with the catch-all "invalid transition" error**

Run: `cargo test -p paigasus-helikon-core --test transition_unit -- model_response_without_tool_calls_terminates`
Expected: FAIL. The current catch-all transitions to `Failed`, not `Done`.

- [ ] **Step 3: Implement the case**

In `loop_state.rs`, add this arm to the `match` before the catch-all:

```rust
        // Model produced a response with no tool calls → terminate.
        (LoopState::CallingModel { .. }, TransitionInput::ModelResponse { items, usage, .. })
            if !items.iter().any(|i| matches!(i, Item::ToolCall { .. })) =>
        {
            let mut events: Vec<AgentEvent> = items
                .iter()
                .filter(|i| matches!(i, Item::AssistantMessage { .. }))
                .cloned()
                .map(|item| AgentEvent::MessageOutput { item })
                .collect();
            // Extract terminal content from the last AssistantMessage.
            let content = items
                .iter()
                .rev()
                .find_map(|i| match i {
                    Item::AssistantMessage { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            events.push(AgentEvent::RunCompleted { usage });
            TransitionOutcome {
                next_state: LoopState::Done(FinalOutput { content, usage }),
                events,
                next_action: NextAction::Terminate,
            }
        }
```

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test -p paigasus-helikon-core --test transition_unit`
Expected: both tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs crates/paigasus-helikon-core/tests/transition_unit.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 transition terminates on tool-less ModelResponse

When the model returns one or more AssistantMessages without any
ToolCall items, transition produces LoopState::Done(FinalOutput) and
emits MessageOutput + RunCompleted events.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B4: TDD case 3 — Tool calls fan out to ExecutingTools

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/transition_unit.rs`
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs`

- [ ] **Step 1: Add the failing test**

Append:

```rust
#[test]
fn model_response_with_tool_calls_fans_out() {
    let state = LoopState::CallingModel { turn: 0 };
    let assistant = Item::AssistantMessage {
        content: vec![ContentPart::Text { text: "calling tools".into() }],
        agent: Some("test".into()),
    };
    let call_a = Item::ToolCall {
        call_id: "1".into(),
        name: "a".into(),
        args: serde_json::json!({}),
    };
    let call_b = Item::ToolCall {
        call_id: "2".into(),
        name: "b".into(),
        args: serde_json::json!({}),
    };
    let conversation = vec![];
    let input = TransitionInput::ModelResponse {
        items: vec![assistant, call_a, call_b],
        usage: TokenUsage::default(),
        finish_reason: FinishReason::ToolCalls,
    };

    let outcome = transition(&state, input, &ctx_with(16, &conversation));

    match outcome.next_state {
        LoopState::ExecutingTools { ref calls, turn } => {
            assert_eq!(calls.len(), 2);
            assert_eq!(turn, 0);
        }
        other => panic!("expected ExecutingTools, got {other:?}"),
    }
    match outcome.next_action {
        NextAction::ExecuteTools { ref calls } => assert_eq!(calls.len(), 2),
        ref other => panic!("expected ExecuteTools, got {other:?}"),
    }
    // Expected events: MessageOutput, ToolCallItem, ToolCallItem.
    assert_eq!(outcome.events.len(), 3);
    assert_matches!(&outcome.events[0], AgentEvent::MessageOutput { .. });
    assert_matches!(&outcome.events[1], AgentEvent::ToolCallItem { .. });
    assert_matches!(&outcome.events[2], AgentEvent::ToolCallItem { .. });
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p paigasus-helikon-core --test transition_unit -- model_response_with_tool_calls_fans_out`
Expected: FAIL (catch-all returns Failed).

- [ ] **Step 3: Implement the case**

In `loop_state.rs`, add this arm before the no-tool-calls arm (more-specific-first matters because the no-tool-calls arm has an `if` guard that requires *no* tool calls):

```rust
        // Model produced tool calls → fan out to ExecutingTools.
        (LoopState::CallingModel { turn }, TransitionInput::ModelResponse { items, .. })
            if items.iter().any(|i| matches!(i, Item::ToolCall { .. })) =>
        {
            let mut events: Vec<AgentEvent> = Vec::new();
            let mut calls: Vec<ToolCallRequest> = Vec::new();
            for item in &items {
                match item {
                    Item::AssistantMessage { .. } => {
                        events.push(AgentEvent::MessageOutput { item: item.clone() });
                    }
                    Item::ToolCall { call_id, name, args } => {
                        events.push(AgentEvent::ToolCallItem { item: item.clone() });
                        calls.push(ToolCallRequest {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            args: args.clone(),
                        });
                    }
                    _ => {}
                }
            }
            TransitionOutcome {
                next_state: LoopState::ExecutingTools { calls: calls.clone(), turn: *turn },
                events,
                next_action: NextAction::ExecuteTools { calls },
            }
        }
```

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test -p paigasus-helikon-core --test transition_unit`
Expected: all three tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs crates/paigasus-helikon-core/tests/transition_unit.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 transition fans out tool calls to ExecutingTools

When ModelResponse contains at least one ToolCall item, transition
moves to ExecutingTools { calls, turn } and emits MessageOutput +
one ToolCallItem per call. NextAction = ExecuteTools { calls }.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B5: TDD case 4 — Tool results re-enter the model

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/transition_unit.rs`
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs`

- [ ] **Step 1: Add the failing test**

Append:

```rust
#[test]
fn tool_results_advance_to_next_call_model() {
    let calls = vec![
        ToolCallRequest {
            call_id: "1".into(),
            name: "a".into(),
            args: serde_json::json!({}),
        },
        ToolCallRequest {
            call_id: "2".into(),
            name: "b".into(),
            args: serde_json::json!({}),
        },
    ];
    let state = LoopState::ExecutingTools { calls: calls.clone(), turn: 0 };
    let outcomes = vec![
        ToolCallOutcome {
            call_id: "1".into(),
            result: Ok(vec![ContentPart::Text { text: "ok-a".into() }]),
        },
        ToolCallOutcome {
            call_id: "2".into(),
            result: Ok(vec![ContentPart::Text { text: "ok-b".into() }]),
        },
    ];
    let conversation = vec![];
    let input = TransitionInput::ToolResults { outcomes };

    let outcome = transition(&state, input, &ctx_with(16, &conversation));

    assert_matches!(outcome.next_state, LoopState::CallingModel { turn: 1 });
    assert_matches!(outcome.next_action, NextAction::CallModel { .. });
    // Expected: ToolOutputItem × 2 + TurnStarted { turn: 1 }
    assert_eq!(outcome.events.len(), 3);
    assert_matches!(&outcome.events[0], AgentEvent::ToolOutputItem { .. });
    assert_matches!(&outcome.events[1], AgentEvent::ToolOutputItem { .. });
    assert_matches!(&outcome.events[2], AgentEvent::TurnStarted { turn: 1 });
}
```

Also add this use to the top of the test file (if not already there):

```rust
use paigasus_helikon_core::{ToolCallOutcome, ToolCallRequest};
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p paigasus-helikon-core --test transition_unit -- tool_results_advance_to_next_call_model`
Expected: FAIL.

- [ ] **Step 3: Implement the case**

Add this arm in `loop_state.rs` (before the catch-all):

```rust
        // Tool results complete → bump turn and ask the model again.
        (LoopState::ExecutingTools { turn, .. }, TransitionInput::ToolResults { outcomes }) => {
            let next_turn = turn + 1;
            if next_turn >= ctx.max_turns {
                return TransitionOutcome {
                    next_state: LoopState::Failed(AgentError::MaxTurnsExceeded(ctx.max_turns)),
                    events: vec![AgentEvent::RunFailed {
                        error: format!("max turns ({}) exceeded", ctx.max_turns),
                    }],
                    next_action: NextAction::Terminate,
                };
            }
            let mut events: Vec<AgentEvent> = outcomes
                .into_iter()
                .map(|o| AgentEvent::ToolOutputItem {
                    item: Item::ToolResult {
                        call_id: o.call_id,
                        content: o.result.unwrap_or_else(|e| vec![ContentPart::Text { text: e }]),
                    },
                })
                .collect();
            events.push(AgentEvent::TurnStarted { turn: next_turn });
            let request = ModelRequest {
                messages: ctx.conversation.to_vec(),
                tools: ctx.tools.to_vec(),
                model_settings: ctx.model_settings.clone(),
            };
            TransitionOutcome {
                next_state: LoopState::CallingModel { turn: next_turn },
                events,
                next_action: NextAction::CallModel { request },
            }
        }
```

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test -p paigasus-helikon-core --test transition_unit`
Expected: all four tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs crates/paigasus-helikon-core/tests/transition_unit.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 transition advances to next turn after tool results

ExecutingTools { calls, turn } + ToolResults { outcomes } transitions
to CallingModel { turn: turn + 1 } with events ToolOutputItem ×N +
TurnStarted { turn + 1 }. Returns CallModel with a fresh ModelRequest.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B6: TDD case 5 — Max turns trips at the CallingModel boundary

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/transition_unit.rs`
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs`

- [ ] **Step 1: Add the failing test**

Append:

```rust
#[test]
fn calling_model_at_max_turns_fails() {
    let max_turns = 4;
    let state = LoopState::CallingModel { turn: max_turns };
    let conversation = vec![];
    let input = TransitionInput::Start { messages: vec![] };

    let outcome = transition(&state, input, &ctx_with(max_turns, &conversation));

    match outcome.next_state {
        LoopState::Failed(paigasus_helikon_core::AgentError::MaxTurnsExceeded(n)) => {
            assert_eq!(n, max_turns);
        }
        other => panic!("expected Failed(MaxTurnsExceeded), got {other:?}"),
    }
    assert_matches!(outcome.next_action, NextAction::Terminate);
    assert_eq!(outcome.events.len(), 1);
    assert_matches!(&outcome.events[0], AgentEvent::RunFailed { .. });
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p paigasus-helikon-core --test transition_unit -- calling_model_at_max_turns_fails`
Expected: FAIL.

- [ ] **Step 3: Implement the case**

The existing `(CallingModel, Start)` arm has `if *turn < ctx.max_turns`. Add a paired arm (before the existing Start arm, since Rust match ordering matters):

```rust
        // Max turns reached at the CallingModel boundary → fail fast.
        (LoopState::CallingModel { turn }, _) if *turn >= ctx.max_turns => {
            TransitionOutcome {
                next_state: LoopState::Failed(AgentError::MaxTurnsExceeded(ctx.max_turns)),
                events: vec![AgentEvent::RunFailed {
                    error: format!("max turns ({}) exceeded", ctx.max_turns),
                }],
                next_action: NextAction::Terminate,
            }
        }
```

This must come **before** the `(CallingModel, Start)` arm. Rust matches arms top-to-bottom and stops at the first match — putting this guard arm first means the budget check runs before any happy-path arm.

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test -p paigasus-helikon-core --test transition_unit`
Expected: all five tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs crates/paigasus-helikon-core/tests/transition_unit.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 transition fails on max_turns at CallingModel

When CallingModel { turn } has turn >= ctx.max_turns, transition
yields LoopState::Failed(MaxTurnsExceeded(max_turns)) and emits
RunFailed before Terminate. Locks the runaway-loop safety bound.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task B7: TDD case 6 — Unreachable variants surface NotImplemented

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/transition_unit.rs`
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs`

- [ ] **Step 1: Add the failing tests**

Append:

```rust
#[test]
fn applying_handoff_surfaces_not_implemented() {
    let state = LoopState::ApplyingHandoff {
        target: "other".into(),
        transcript: vec![],
    };
    let conversation = vec![];
    let outcome = transition(
        &state,
        TransitionInput::Start { messages: vec![] },
        &ctx_with(16, &conversation),
    );

    match outcome.next_state {
        LoopState::Failed(paigasus_helikon_core::AgentError::NotImplemented { feature }) => {
            assert_eq!(feature, "handoff");
        }
        other => panic!("expected Failed(NotImplemented), got {other:?}"),
    }
    assert_matches!(outcome.next_action, NextAction::Terminate);
}

#[test]
fn compacting_surfaces_not_implemented() {
    let state = LoopState::Compacting;
    let conversation = vec![];
    let outcome = transition(
        &state,
        TransitionInput::Start { messages: vec![] },
        &ctx_with(16, &conversation),
    );

    match outcome.next_state {
        LoopState::Failed(paigasus_helikon_core::AgentError::NotImplemented { feature }) => {
            assert_eq!(feature, "compaction");
        }
        other => panic!("expected Failed(NotImplemented), got {other:?}"),
    }
}

#[test]
fn needs_approval_surfaces_not_implemented() {
    let state = LoopState::NeedsApproval { pending: vec![] };
    let conversation = vec![];
    let outcome = transition(
        &state,
        TransitionInput::Start { messages: vec![] },
        &ctx_with(16, &conversation),
    );

    match outcome.next_state {
        LoopState::Failed(paigasus_helikon_core::AgentError::NotImplemented { feature }) => {
            assert_eq!(feature, "approval");
        }
        other => panic!("expected Failed(NotImplemented), got {other:?}"),
    }
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p paigasus-helikon-core --test transition_unit`
Expected: the three new tests FAIL (catch-all returns Other, not NotImplemented).

- [ ] **Step 3: Implement the cases**

Add these arms in `loop_state.rs` before the catch-all:

```rust
        // Unreachable-in-SMA-314 variants surface NotImplemented and Terminate.
        (LoopState::ApplyingHandoff { .. }, _) => not_implemented("handoff"),
        (LoopState::Compacting, _) => not_implemented("compaction"),
        (LoopState::NeedsApproval { .. }, _) => not_implemented("approval"),
```

And the helper, anywhere in the file:

```rust
fn not_implemented(feature: &'static str) -> TransitionOutcome {
    TransitionOutcome {
        next_state: LoopState::Failed(AgentError::NotImplemented { feature }),
        events: vec![AgentEvent::RunFailed {
            error: format!("not yet implemented: {feature}"),
        }],
        next_action: NextAction::Terminate,
    }
}
```

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test -p paigasus-helikon-core --test transition_unit`
Expected: all eight tests PASS.

- [ ] **Step 5: Clippy check on the new code**

Run: `cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings`
Expected: exits 0.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs crates/paigasus-helikon-core/tests/transition_unit.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 transition surfaces NotImplemented for deferred variants

ApplyingHandoff, Compacting, and NeedsApproval are forward-compat
slots in LoopState; reaching them from transition() yields
Failed(AgentError::NotImplemented { feature }) and Terminate. Locks
the SMA-314 scope boundary at the state-machine level.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase C — `LlmAgent` + async driver

Goal: define the concrete `LlmAgent<Ctx, M>` struct, the `Instructions<Ctx>` trait with three blanket impls, and the `Agent::run` implementation that wraps the pure transition function in an `async_stream::stream!` driver.

### Task C1: Define `Instructions<Ctx>` trait + three blanket impls

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs`

- [ ] **Step 1: Append the trait + impls to `agent.rs`**

```rust
/// Renders the system prompt for one turn of the loop.
///
/// Implemented for `String`, `&'static str`, and any
/// `Fn(&RunContext<Ctx>) -> String + Send + Sync`.
///
/// ```
/// use std::sync::Arc;
/// use paigasus_helikon_core::{Instructions, RunContext};
///
/// let a: Arc<dyn Instructions<()>> = Arc::new("You are a helpful assistant.".to_string());
/// let b: Arc<dyn Instructions<()>> = Arc::new(|_: &RunContext<()>| "Dynamic".into());
/// let _ = (a, b);
/// ```
pub trait Instructions<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Produce the system-prompt text for this run.
    fn render(&self, ctx: &crate::RunContext<Ctx>) -> String;
}

impl<Ctx> Instructions<Ctx> for String
where
    Ctx: Send + Sync + 'static,
{
    fn render(&self, _ctx: &crate::RunContext<Ctx>) -> String { self.clone() }
}

impl<Ctx> Instructions<Ctx> for &'static str
where
    Ctx: Send + Sync + 'static,
{
    fn render(&self, _ctx: &crate::RunContext<Ctx>) -> String { (*self).to_owned() }
}

impl<Ctx, F> Instructions<Ctx> for F
where
    Ctx: Send + Sync + 'static,
    F: Fn(&crate::RunContext<Ctx>) -> String + Send + Sync,
{
    fn render(&self, ctx: &crate::RunContext<Ctx>) -> String { (self)(ctx) }
}
```

- [ ] **Step 2: Run the doctest**

Run: `cargo test --doc -p paigasus-helikon-core`
Expected: PASS — the inline `Instructions` doctest builds two `Arc<dyn Instructions<()>>` values.

- [ ] **Step 3: Verify the crate still compiles + clippy clean**

Run: `cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings`
Expected: exits 0.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 add Instructions trait with blanket impls

Trait + blanket impls for String, &'static str, and any
Fn(&RunContext<Ctx>) -> String + Send + Sync. LlmAgent holds
Arc<dyn Instructions<Ctx>>. Doctest exercises the two common
constructors.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task C2: Define `LlmAgent<Ctx, M>` struct

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs`

- [ ] **Step 1: Append the struct**

```rust
/// The concrete LLM-driven agent. Implements [`crate::Agent`].
///
/// Constructed via direct field assignment in SMA-314; the ergonomic
/// typestate builder lands in SMA-319. **Not** `#[non_exhaustive]` —
/// the typestate builder needs struct-literal construction from
/// outside the crate.
pub struct LlmAgent<Ctx, M>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
{
    /// Agent identifier (used in events and trace spans).
    pub name: String,
    /// One-line description.
    pub description: String,
    /// System-prompt renderer.
    pub instructions: std::sync::Arc<dyn Instructions<Ctx>>,
    /// The model the agent calls each turn.
    pub model: std::sync::Arc<M>,
    /// Tools the model may call. Each invocation snapshots these into
    /// `ModelRequest.tools` via [`crate::ToolDef`].
    pub tools: Vec<std::sync::Arc<dyn crate::Tool<Ctx>>>,
    /// Candidate agents this one may hand off to. Stored but not
    /// driven in SMA-314.
    pub handoffs: Vec<std::sync::Arc<dyn crate::Agent<Ctx>>>,
    /// Structured-output type marker. SMA-320 makes this honest.
    pub output_type: Option<OutputType>,
    /// Pre-input guardrails. Stored but not driven in SMA-314.
    pub input_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    /// Post-output guardrails. Stored but not driven in SMA-314.
    pub output_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    /// Lifecycle hooks. Stored but not driven in SMA-314.
    pub hooks: Vec<std::sync::Arc<dyn crate::Hook<Ctx>>>,
    /// Provider-tuning knobs. Field shape lands with SMA-316 / SMA-317.
    pub model_settings: crate::ModelSettings,
    /// Per-run config. At SMA-314 only `max_turns` is meaningful.
    pub config: crate::RunConfig,
}
```

- [ ] **Step 2: Verify the crate compiles**

Run: `cargo check -p paigasus-helikon-core --all-features`
Expected: exits 0.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 add LlmAgent struct (no Agent impl yet)

11 public fields per the ticket scope: name, description,
instructions, model, tools, handoffs, output_type, input_guardrails,
output_guardrails, hooks, model_settings, config. Not
#[non_exhaustive] because SMA-319's typestate builder needs
struct-literal construction.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task C3: Implement `Agent` for `LlmAgent` (the async driver)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs`

- [ ] **Step 1: Append the helpers**

```rust
/// Accumulates the in-progress tool call across `ModelEvent::ToolCallDelta` chunks.
#[derive(Default)]
struct ToolCallAccum {
    name: Option<String>,
    args_str: String,
}

/// Reassemble streamed model output into [`Item`]s.
fn build_items(
    agent_name: &str,
    text: String,
    reasoning: String,
    tool_accum: std::collections::HashMap<String, ToolCallAccum>,
) -> Vec<crate::Item> {
    let mut items = Vec::new();
    if !text.is_empty() || !reasoning.is_empty() {
        let mut content = Vec::new();
        if !reasoning.is_empty() {
            content.push(crate::ContentPart::Reasoning { text: reasoning });
        }
        if !text.is_empty() {
            content.push(crate::ContentPart::Text { text });
        }
        items.push(crate::Item::AssistantMessage {
            content,
            agent: Some(agent_name.to_owned()),
        });
    }
    for (call_id, accum) in tool_accum {
        items.push(crate::Item::ToolCall {
            call_id,
            name: accum.name.unwrap_or_default(),
            args: serde_json::from_str(&accum.args_str)
                .unwrap_or(serde_json::Value::Null),
        });
    }
    items
}

/// Conversion convention: `ToolOutput.content` (SMA-313's
/// `serde_json::Value`) becomes one `ContentPart::Text`.
/// `Value::String(s) -> ContentPart::Text { text: s }`; other JSON
/// values are stringified via `Value::to_string()`.
fn tool_output_to_content_parts(output: &crate::ToolOutput) -> Vec<crate::ContentPart> {
    let text = match &output.content {
        serde_json::Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    vec![crate::ContentPart::Text { text }]
}

async fn run_tools_concurrent<Ctx>(
    tools: &[std::sync::Arc<dyn crate::Tool<Ctx>>],
    calls: &[crate::ToolCallRequest],
    tool_ctx: &crate::ToolContext<Ctx>,
) -> Vec<crate::ToolCallOutcome>
where
    Ctx: Send + Sync + 'static,
{
    let futures = calls.iter().map(|call| {
        let tool = tools.iter().find(|t| t.name() == call.name).cloned();
        let call_id = call.call_id.clone();
        let args = call.args.clone();
        let name = call.name.clone();
        async move {
            match tool {
                Some(t) => match t.invoke(tool_ctx, args).await {
                    Ok(output) => crate::ToolCallOutcome {
                        call_id,
                        result: Ok(tool_output_to_content_parts(&output)),
                    },
                    Err(e) => crate::ToolCallOutcome {
                        call_id,
                        result: Err(e.to_string()),
                    },
                },
                None => crate::ToolCallOutcome {
                    call_id,
                    result: Err(format!("unknown tool: {name}")),
                },
            }
        }
    });
    futures_util::future::join_all(futures).await
}
```

- [ ] **Step 2: Append the `Agent` impl**

```rust
#[async_trait::async_trait]
impl<Ctx, M> crate::Agent<Ctx> for LlmAgent<Ctx, M>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
{
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }

    async fn run(
        &self,
        ctx: crate::RunContext<Ctx>,
        input: AgentInput,
    ) -> Result<futures_core::stream::BoxStream<'static, crate::AgentEvent>, AgentError> {
        use futures_util::stream::StreamExt;

        // Snapshot everything the stream needs — it outlives `&self`.
        let model = std::sync::Arc::clone(&self.model);
        let tools = self.tools.clone();
        let max_turns = self.config.max_turns;
        let model_settings = self.model_settings.clone();
        let agent_name = self.name.clone();
        let instructions_text = self.instructions.render(&ctx);
        let tool_defs: Vec<crate::ToolDef> = tools
            .iter()
            .map(|t| crate::ToolDef {
                name: t.name().to_owned(),
                description: t.description().to_owned(),
                schema: t.schema().clone(),
            })
            .collect();

        let stream = async_stream::stream! {
            // Seed conversation: optional system message + user input.
            let mut conversation: Vec<crate::Item> = Vec::new();
            if !instructions_text.is_empty() {
                conversation.push(crate::Item::System {
                    content: vec![crate::ContentPart::Text { text: instructions_text }],
                });
            }
            conversation.extend(input.messages.iter().cloned());

            let mut loop_state = crate::LoopState::CallingModel { turn: 0 };
            let mut tx_input = crate::TransitionInput::Start { messages: input.messages };

            yield crate::AgentEvent::RunStarted { agent: agent_name.clone() };

            loop {
                let tx_ctx = crate::TransitionCtx {
                    tools: &tool_defs,
                    model_settings: &model_settings,
                    max_turns,
                    conversation: &conversation,
                };
                let outcome = crate::transition(&loop_state, tx_input, &tx_ctx);
                let crate::TransitionOutcome { next_state, events, next_action } = outcome;
                for ev in events { yield ev; }
                loop_state = next_state;

                match next_action {
                    crate::NextAction::CallModel { request } => {
                        let mut model_stream = match model
                            .invoke(request, ctx.cancel().clone())
                            .await
                        {
                            Ok(s) => s,
                            Err(e) => {
                                yield crate::AgentEvent::RunFailed { error: e.to_string() };
                                return;
                            }
                        };

                        let mut text = String::new();
                        let mut reasoning = String::new();
                        let mut tool_accum: std::collections::HashMap<String, ToolCallAccum> =
                            std::collections::HashMap::new();
                        let mut finish_reason = crate::FinishReason::Stop;

                        while let Some(evt) = model_stream.next().await {
                            match evt {
                                Ok(crate::ModelEvent::TokenDelta { text: t }) => {
                                    text.push_str(&t);
                                    yield crate::AgentEvent::TokenDelta { text: t };
                                }
                                Ok(crate::ModelEvent::ReasoningDelta { text: t }) => {
                                    reasoning.push_str(&t);
                                    yield crate::AgentEvent::ReasoningDelta { text: t };
                                }
                                Ok(crate::ModelEvent::ToolCallDelta { call_id, name, args_delta }) => {
                                    let a = tool_accum.entry(call_id.clone()).or_default();
                                    if let Some(n) = name.as_deref() { a.name = Some(n.into()); }
                                    a.args_str.push_str(&args_delta);
                                    yield crate::AgentEvent::ToolCallDelta { call_id, name, args_delta };
                                }
                                Ok(crate::ModelEvent::Finish { reason }) => { finish_reason = reason; }
                                Err(e) => {
                                    yield crate::AgentEvent::RunFailed { error: e.to_string() };
                                    return;
                                }
                            }
                        }

                        let items = build_items(&agent_name, text, reasoning, tool_accum);
                        conversation.extend(items.iter().cloned());
                        // Usage stubbed until SMA-316/SMA-317 add ModelEvent::Usage.
                        let usage = crate::TokenUsage::default();
                        tx_input = crate::TransitionInput::ModelResponse {
                            items,
                            usage,
                            finish_reason,
                        };
                    }
                    crate::NextAction::ExecuteTools { calls } => {
                        let tool_ctx = ctx.to_tool_context();
                        let outcomes = run_tools_concurrent(&tools, &calls, &tool_ctx).await;
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
                    crate::NextAction::Terminate => return,
                }
            }
        };

        Ok(Box::pin(stream))
    }
}
```

- [ ] **Step 3: Verify the crate compiles**

Run: `cargo check -p paigasus-helikon-core --all-features`
Expected: exits 0.

- [ ] **Step 4: Verify clippy clean**

Run: `cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings`
Expected: exits 0.

- [ ] **Step 5: Run all existing tests (no behavior change to them)**

Run: `cargo test -p paigasus-helikon-core --all-features`
Expected: all PASS — the loop driver compiles, integration tests don't exist yet.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-314 implement LlmAgent::run loop driver

Thin async_stream::stream! block that calls transition(),
dispatches NextAction (model invocation aggregated inline with raw
delta yields, or join_all over tool invocations), and feeds the
result back as TransitionInput. Three private helpers: build_items
reassembles streamed model output, tool_output_to_content_parts
applies the SMA-313 conversion convention, run_tools_concurrent
fans out via futures_util::future::join_all.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase D — Integration tests (the AC locks)

Goal: build shared `MockModel` / `MockTool` / `MockToolBarrier` / `NoopSession` fixtures, then write the three integration test files that lock the three Linear acceptance criteria.

### Task D1: Create `tests/common/mod.rs` with `MockModel`

**Files:**
- Create: `crates/paigasus-helikon-core/tests/common/mod.rs`

- [ ] **Step 1: Create the file**

```rust
//! Shared test fixtures for SMA-314 integration tests. Compiled once
//! per test binary via `#[path = "common/mod.rs"] mod common;` at the
//! top of each integration test file.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use std::time::Instant;

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::stream;

use paigasus_helikon_core::{
    AgentEvent, CancellationToken, ContentPart, FinishReason, HookRegistry,
    Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
    RunContext, Session, SessionError, SessionEvent, SequenceId,
    ConversationSnapshot, Tool, ToolContext, ToolError, ToolOutput,
    TracerHandle,
};

/// A scripted [`Model`] that emits a pre-recorded sequence of
/// [`ModelEvent`]s per call to [`Model::invoke`]. Pop one script per
/// invocation; running out of scripts yields a `ModelError`.
pub struct MockModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
}

impl MockModel {
    pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(VecDeque::from(scripts)),
        })
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
        ModelCapabilities {
            streaming: true,
            tools: true,
            parallel_tool_calls: true,
            ..Default::default()
        }
    }
}
```

- [ ] **Step 2: Verify the file compiles in isolation**

Create a one-line dummy integration test to force `tests/common/mod.rs` to be compiled. Add a placeholder test file:

```bash
mkdir -p crates/paigasus-helikon-core/tests
```

Skip this step — `common/mod.rs` will compile when D2-D4 add more fixtures and D5-D7 include it from integration tests.

- [ ] **Step 3: Commit (compile verification deferred until D5)**

```bash
git add crates/paigasus-helikon-core/tests/common/mod.rs
git commit -m "$(cat <<'EOF'
test(core): SMA-314 add MockModel fixture

Scripted Model impl: each call to invoke pops one Vec<ModelEvent>
from a queue and streams them. Used by the loop_happy_path and
loop_parallel_tools integration tests.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

The commit-msg hook allows `test(core)`. If it rejects, change to `feat(core)`.

---

### Task D2: Add `MockTool` + `MockToolBarrier` to `common/mod.rs`

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/common/mod.rs`

- [ ] **Step 1: Append the two tool fixtures**

```rust
/// A [`Tool`] that records every invocation and returns a static
/// `serde_json::Value` as its output.
pub struct MockTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    invocations: Mutex<Vec<(serde_json::Value, Instant)>>,
    output: serde_json::Value,
}

impl MockTool {
    pub fn new(name: &str, output: serde_json::Value) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            description: format!("mock tool {name}"),
            schema: serde_json::json!({"type": "object"}),
            invocations: Mutex::new(Vec::new()),
            output,
        })
    }

    pub fn invocations(&self) -> Vec<(serde_json::Value, Instant)> {
        self.invocations.lock().unwrap().clone()
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for MockTool
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn schema(&self) -> &serde_json::Value { &self.schema }

    async fn invoke(
        &self,
        _ctx: &ToolContext<Ctx>,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.invocations.lock().unwrap().push((args, Instant::now()));
        Ok(ToolOutput { content: self.output.clone() })
    }
}

/// A [`Tool`] that synchronizes its invocations through a
/// [`tokio::sync::Barrier`]. Use with `Barrier::new(N)` and N tool
/// instances to verify concurrent execution: if the tools run
/// serially, the first invocation blocks forever waiting for the
/// second waiter.
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
            description: format!("barrier-synced mock tool {name}"),
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
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { &self.description }
    fn schema(&self) -> &serde_json::Value { &self.schema }

    async fn invoke(
        &self,
        _ctx: &ToolContext<Ctx>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.barrier.wait().await;
        Ok(ToolOutput { content: serde_json::json!({"ok": true}) })
    }
}
```

- [ ] **Step 2: Commit (compile verification still deferred)**

```bash
git add crates/paigasus-helikon-core/tests/common/mod.rs
git commit -m "$(cat <<'EOF'
test(core): SMA-314 add MockTool and MockToolBarrier fixtures

MockTool records invocations (args + Instant) and returns a static
JSON output. MockToolBarrier blocks on tokio::sync::Barrier — paired
with Barrier::new(N), N instances verify concurrent fan-out by
deadlocking under serial execution.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task D3: Add `NoopSession` + `noop_run_context` helper

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/common/mod.rs`

- [ ] **Step 1: Append the session + ctx helpers**

```rust
/// A no-op [`Session`] implementation. `append` discards;
/// `events` / `snapshot` return empty.
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

/// Build a minimal [`RunContext`] suitable for integration tests.
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

- [ ] **Step 2: Commit**

```bash
git add crates/paigasus-helikon-core/tests/common/mod.rs
git commit -m "$(cat <<'EOF'
test(core): SMA-314 add NoopSession and noop_run_context helper

NoopSession satisfies Session for tests that don't exercise
persistence. noop_run_context builds RunContext<Ctx> from
Default::default() — used by every SMA-314 integration test.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task D4: Write `loop_happy_path.rs` — AC #1 (single-turn run completes)

**Files:**
- Create: `crates/paigasus-helikon-core/tests/loop_happy_path.rs`

- [ ] **Step 1: Write the test**

```rust
//! AC #1: single-turn run on a fixture MockModel completes with
//! RunCompleted. AC #2 lock lives in the second test (multi-turn
//! with tool call).

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, FinishReason, Instructions, LlmAgent,
    ModelEvent, ModelSettings, OutputType, RunConfig, RunResultStreaming,
    RunContext,
};

use common::{noop_run_context, MockModel};

fn build_agent<M>(model: Arc<M>) -> LlmAgent<(), M>
where
    M: paigasus_helikon_core::Model + 'static,
{
    LlmAgent {
        name: "test".into(),
        description: "test agent".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools: Vec::new(),
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
    }
}

#[tokio::test]
async fn single_turn_run_completes() {
    let model = MockModel::with_scripts(vec![vec![
        ModelEvent::TokenDelta { text: "hello".into() },
        ModelEvent::Finish { reason: FinishReason::Stop },
    ]]);
    let agent = build_agent(model);
    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("hi"))
        .await
        .expect("agent.run should succeed");

    let result = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect("collect should succeed");

    assert_eq!(result.final_output, "hello");
    assert!(
        matches!(result.events.last(), Some(AgentEvent::RunCompleted { .. })),
        "expected RunCompleted as last event, got: {:?}", result.events.last(),
    );
    assert!(
        result.events.iter().any(|e| matches!(e, AgentEvent::TokenDelta { .. })),
        "expected at least one TokenDelta",
    );
    let _ = OutputType::from_schema::<String>; // ensure the import compiles
}
```

- [ ] **Step 2: Run — expect PASS**

Run: `cargo test -p paigasus-helikon-core --test loop_happy_path -- single_turn_run_completes`
Expected: PASS.

If it fails: the most likely cause is a missing re-export or a typo in the `build_agent` field order. Compare against the struct definition in `agent.rs`. Do not modify production code to make the test pass — the loop driver was already implemented in Task C3.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-core/tests/loop_happy_path.rs
git commit -m "$(cat <<'EOF'
test(core): SMA-314 lock AC #1 with single-turn run integration test

#[tokio::test] drives LlmAgent::run with a one-script MockModel
that emits one TokenDelta + Finish. Asserts: final_output == "hello",
last event is RunCompleted, at least one TokenDelta in the stream.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task D5: Add the multi-turn test to `loop_happy_path.rs` — AC #2

**Files:**
- Modify: `crates/paigasus-helikon-core/tests/loop_happy_path.rs`

- [ ] **Step 1: Add the second test**

Append:

```rust
fn event_kind(ev: &AgentEvent) -> &'static str {
    match ev {
        AgentEvent::RunStarted { .. } => "RunStarted",
        AgentEvent::TurnStarted { .. } => "TurnStarted",
        AgentEvent::TokenDelta { .. } => "TokenDelta",
        AgentEvent::ReasoningDelta { .. } => "ReasoningDelta",
        AgentEvent::ToolCallDelta { .. } => "ToolCallDelta",
        AgentEvent::MessageOutput { .. } => "MessageOutput",
        AgentEvent::ToolCallItem { .. } => "ToolCallItem",
        AgentEvent::ToolOutputItem { .. } => "ToolOutputItem",
        AgentEvent::HandoffItem { .. } => "HandoffItem",
        AgentEvent::AgentUpdated { .. } => "AgentUpdated",
        AgentEvent::GuardrailTriggered { .. } => "GuardrailTriggered",
        AgentEvent::ApprovalRequested { .. } => "ApprovalRequested",
        AgentEvent::RunCompleted { .. } => "RunCompleted",
        AgentEvent::RunFailed { .. } => "RunFailed",
    }
}

#[tokio::test]
async fn multi_turn_with_tool_call() {
    use common::MockTool;

    // Turn 0: model emits one ToolCall. Turn 1: model emits final text.
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "1".into(),
                name: Some("echo".into()),
                args_delta: "{\"msg\":\"hi\"}".into(),
            },
            ModelEvent::Finish { reason: FinishReason::ToolCalls },
        ],
        vec![
            ModelEvent::TokenDelta { text: "done".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
    ]);
    let tool = MockTool::new("echo", serde_json::json!("ok"));
    let mut agent = build_agent(model);
    agent.tools = vec![tool.clone()];

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .expect("agent.run should succeed");
    let result = RunResultStreaming::new(stream).collect().await.expect("collect");

    assert_eq!(result.final_output, "done");
    assert_eq!(tool.invocations().len(), 1);

    let kinds: Vec<&'static str> = result.events.iter().map(event_kind).collect();
    insta::assert_yaml_snapshot!(kinds);
}
```

- [ ] **Step 2: Run the test — first run will fail with a missing snapshot**

Run: `cargo test -p paigasus-helikon-core --test loop_happy_path -- multi_turn_with_tool_call`
Expected: FAIL with insta complaining about a missing snapshot at `tests/snapshots/loop_happy_path__multi_turn_with_tool_call.snap`.

- [ ] **Step 3: Generate the snapshot and review it**

Run: `INSTA_UPDATE=always cargo test -p paigasus-helikon-core --test loop_happy_path -- multi_turn_with_tool_call`
Expected: PASS, with a new `.snap` file written.

Then inspect the snapshot at `crates/paigasus-helikon-core/tests/snapshots/loop_happy_path__multi_turn_with_tool_call.snap`. Expected sequence (kinds only):

```
RunStarted
TurnStarted
ToolCallDelta
MessageOutput? (depends on whether build_items emitted an empty assistant or not — should not appear if text is empty)
ToolCallItem
ToolOutputItem
TurnStarted
TokenDelta
MessageOutput
RunCompleted
```

If the snapshot looks wrong (e.g. duplicate `RunStarted`, or `RunFailed` appears), do not accept it — go back and debug the production code in `agent.rs` / `loop_state.rs`.

If the snapshot looks right, accept it (it's already on disk from `INSTA_UPDATE=always`).

- [ ] **Step 4: Run again without `INSTA_UPDATE` to confirm stability**

Run: `cargo test -p paigasus-helikon-core --test loop_happy_path`
Expected: both tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/tests/loop_happy_path.rs crates/paigasus-helikon-core/tests/snapshots/
git commit -m "$(cat <<'EOF'
test(core): SMA-314 lock AC #2 with multi-turn tool-call test

Two-script MockModel: turn 0 emits one ToolCall; turn 1 emits final
text. The expected event-kind sequence is insta-snapshotted so the
test stays forgiving of inner-field churn but strict on ordering.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task D6: Write `loop_parallel_tools.rs` — AC #3

**Files:**
- Create: `crates/paigasus-helikon-core/tests/loop_parallel_tools.rs`

- [ ] **Step 1: Write the test**

```rust
//! AC #3: two parallel tool calls execute concurrently. Verified via
//! tokio::sync::Barrier — serial execution would deadlock the first
//! waiter; tokio::time::timeout surfaces that as a clear failure.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, FinishReason, Instructions, LlmAgent,
    ModelEvent, ModelSettings, RunConfig, RunResultStreaming,
};

use common::{noop_run_context, MockModel, MockToolBarrier};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_tool_calls_execute_concurrently() {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let tool_a = MockToolBarrier::new("a", Arc::clone(&barrier));
    let tool_b = MockToolBarrier::new("b", Arc::clone(&barrier));

    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "1".into(),
                name: Some("a".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::ToolCallDelta {
                call_id: "2".into(),
                name: Some("b".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::Finish { reason: FinishReason::ToolCalls },
        ],
        vec![
            ModelEvent::TokenDelta { text: "done".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
    ]);

    let agent = LlmAgent::<(), _> {
        name: "test".into(),
        description: "parallel test".into(),
        instructions: Arc::new("") as Arc<dyn Instructions<()>>,
        model,
        tools: vec![tool_a, tool_b],
        handoffs: Vec::new(),
        output_type: None,
        input_guardrails: Vec::new(),
        output_guardrails: Vec::new(),
        hooks: Vec::new(),
        model_settings: ModelSettings::new(),
        config: RunConfig::default(),
    };

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("go"))
        .await
        .expect("agent.run should succeed");

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        RunResultStreaming::new(stream).collect(),
    )
    .await
    .expect("timeout — tools likely ran serially (Barrier deadlocked)")
    .expect("collect should succeed");

    assert!(
        matches!(result.events.last(), Some(AgentEvent::RunCompleted { .. })),
        "expected RunCompleted as last event, got: {:?}", result.events.last(),
    );
}
```

- [ ] **Step 2: Run — expect PASS**

Run: `cargo test -p paigasus-helikon-core --test loop_parallel_tools`
Expected: PASS.

If it times out: the `join_all` call in `run_tools_concurrent` is not actually running futures concurrently. Verify the test runtime is `multi_thread` and that the helper builds the futures iterator before awaiting (the `.map(...)` is lazy; `join_all(futures).await` is what drives them concurrently).

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-core/tests/loop_parallel_tools.rs
git commit -m "$(cat <<'EOF'
test(core): SMA-314 lock AC #3 with barrier-based concurrent-tools test

tokio::sync::Barrier::new(2) + two MockToolBarrier tools: serial
execution would deadlock the first waiter; tokio::time::timeout
turns that into a clear test failure. Flavor = multi_thread so the
parallel futures can actually run on separate workers.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase E — Full CI gate + branch hand-off

### Task E1: Run the full local CI matrix and fix anything that fails

**Files:** (none modified unless a verification step fails)

- [ ] **Step 1: Format check**

Run: `cargo fmt --all -- --check`
Expected: exits 0. If it fails, run `cargo fmt --all` and stage the resulting diff in a `style(core): SMA-314 cargo fmt` commit.

- [ ] **Step 2: Clippy across the workspace**

Run: `cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: exits 0.

Common failure modes specific to this ticket:
- Unused imports in `tests/common/mod.rs` (the `#![allow(dead_code)]` at the top covers the fixtures themselves, but a fixture's `use` lines for items it doesn't reference are still warned). Remove any genuinely unused imports.
- Clippy's `needless_borrow` lint may fire on `(&tool_ctx)` arguments in `run_tools_concurrent` — if so, drop the leading `&` since `tool_ctx` is already a reference.

- [ ] **Step 3: Test the full workspace**

Run: `cargo test --workspace --all-features`
Expected: exits 0. Every test in this PR passes (SMA-312/313 tests, transition_unit, object_safety, loop_happy_path, loop_parallel_tools) plus the existing serde_roundtrip and compile_run_result_typed suites.

- [ ] **Step 4: Docs build clean**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
Expected: exits 0. Watch for:
- Broken intra-doc links (`[`crate::Foo`]` referencing types that moved).
- The known `paigasus-helikon` lib + CLI binary filename-collision warning (CLAUDE.md documents this as expected — not a regression).

- [ ] **Step 5: Doc coverage**

Run: `DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh`
Expected: exits 0. If it fails, a recently-added public item lacks a `///` doc comment — the script reports which one.

Prerequisite: `rustup toolchain install nightly-2026-05-01` must have been run at least once.

- [ ] **Step 6: MSRV verify**

Run: `cargo msrv --path crates/paigasus-helikon-core verify`
Expected: exits 0. Both new deps (`futures-util 0.3` and `async-stream 0.3`) have MSRV well below 1.75.

- [ ] **Step 7: Final commit (style/fixes if needed)**

If any step 1-6 produced changes, commit them with the appropriate type — `style(core)` for cargo fmt, `fix(core)` for missing docs, etc. Do not amend earlier commits; create new ones so the chain reads naturally.

If everything was already clean, skip this step.

- [ ] **Step 8: Push the branch**

```bash
git push -u origin feature/sma-314-llmagent-explicit-loopstate-state-machine
```

Expected: branch published; GitHub returns a PR creation link in the output.

- [ ] **Step 9: Open the PR**

Do **not** open the PR via this plan — that's a human decision (the spec calls for review before merge). Note the branch is ready and the next action is the user creating the PR via `gh pr create` (the user prefers this approach per the repo's workflow conventions in CLAUDE.md).

---

## Verification summary

| Acceptance criterion | Test |
|---|---|
| **AC #1** Single-turn run on a fixture `MockModel` completes with `RunCompleted` | `tests/loop_happy_path.rs::single_turn_run_completes` (Task D4) |
| **AC #2** Multi-turn run with tool calls emits the expected event sequence | `tests/loop_happy_path.rs::multi_turn_with_tool_call` + insta snapshot (Task D5) |
| **AC #3** Two parallel tool calls execute concurrently | `tests/loop_parallel_tools.rs::two_tool_calls_execute_concurrently` (Task D6) |
| Pure state-machine determinism (sanity) | `tests/transition_unit.rs` (8 unit tests, Tasks B2-B7) |
| Object-safety preserved from SMA-312 | `tests/object_safety.rs` (Task A6) |
| Workspace lints clean | Task E1 step 2 |
| Rustdoc clean | Task E1 step 4 |
| Doc coverage ≥ 80% | Task E1 step 5 |
| MSRV holds | Task E1 step 6 |
