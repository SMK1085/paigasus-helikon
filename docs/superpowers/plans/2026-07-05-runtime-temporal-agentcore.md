# SMA-332 runtime-temporal + runtime-agentcore Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ascend `paigasus-helikon-runtime-temporal` (durable Temporal runner driving `core::transition` with per-model-turn / per-tool-call Activities) and `paigasus-helikon-runtime-agentcore` (AWS Bedrock AgentCore container shim) from stubs to published crates, in one PR.

**Architecture:** Three additive core changes (serde derives on wire types; `ModelTurnAccumulator`; public tool-call authorize/redact pipeline) feed two new crates. The Temporal crate splits into a pure `DurableDriver` (unit-testable, SDK-free), a thin `#[workflow]` adapter, worker-side activities, and a client-side `Runner` impl. The AgentCore crate is an axum shim over `TokioRunner` reusing `runtime-axum`'s providers and `paigasus-helikon-mcp`'s streamable-HTTP service.

**Tech Stack:** Rust workspace (MSRV 1.94), `temporalio-sdk`/`-client`/`-sdk-core` 0.5 (`default-features = false`, `tls-aws-lc`), axum 0.8, rmcp 1.7 via `paigasus-helikon-mcp`, Docker (arm64/musl/scratch).

**Spec:** `docs/superpowers/specs/2026-07-05-runtime-temporal-agentcore-design.md` (approved at GATE 1, all recommendations accepted).

**Spec deviation (approved rationale inline):** the spec (§5.2) had the *client* render instructions, but `Runner::run` receives `&dyn Agent`, which exposes no instructions. Rendering moves to a one-shot `render_instructions` **activity** executed first by the workflow (recorded in history ⇒ deterministic on replay), using the worker-fabricated `RunContext` — strictly more consistent with §5.8 ("worker-side posture governs durable execution").

## Global Constraints

- Branch: `feature/sma-332-paigasus-helikon-runtime-temporal-paigasus-helikon-runtime`; commit format `<type>(<scope>): SMA-332 <lowercase subject>`; scopes must be in `.versionrc` (use `core`, `mcp`, `runtime-temporal`, `runtime-agentcore`, `specs`, `plan`, `release`, `book`, `deps`).
- Workspace inheritance mandatory; new crates keep `edition.workspace = true` etc. and `[lints] workspace = true`.
- `missing_docs = "warn"` + `-D warnings` docs gate: every new `pub` item needs `///` docs. Doc-coverage threshold 80%.
- All Temporal deps: `default-features = false` with explicit features; never enable a `ring`-based TLS path (`cargo tree -i ring` must stay clean of new paths).
- Run `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings` before every commit (pre-commit hook is a no-op; pre-push catches it late).
- Never `git add -A` (untracked `.env`/`.claude` exist). Stage explicit paths.
- The exact CI test gate is `cargo test --workspace --all-features` — run it, not per-crate subsets, before declaring a task done (per-crate runs allowed mid-task for speed).
- v0 durable-runner constraint set (spec §5.7): no handoffs/hooks/guardrails (fail-fast at registration), no nested-durable runs, no `Compacting`/`NeedsApproval`.
- Temporal API calibration: exact `temporalio-sdk` 0.5 signatures (macro attribute names, `ActivityOptions`, timer/cancellation API) MUST be verified against https://docs.rs/temporalio-sdk/0.5.0 in Task 4 before Tasks 7–9 are dispatched; the code in those tasks encodes intent and the hello-world shapes from research, not gospel signatures.

---

### Task 1: Core — serde derives on wire types

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs` (derives on `ModelRequest`, `ModelSettings`, `ToolDef`, `ToolChoice`, `ResponseFormat`, `FinishReason`)
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs` (derives on `ToolCallRequest`, `ToolCallOutcome`)
- Test: `crates/paigasus-helikon-core/tests/wire_serde.rs` (new)

**Interfaces:**
- Consumes: existing type definitions (all plain data; `Item`/`ContentPart`/`AgentEvent`/`TokenUsage` already derive serde).
- Produces: `serde::{Serialize, Deserialize}` on the eight types above — Task 5's payloads embed them directly.

- [ ] **Step 1: Write the failing round-trip test**

```rust
//! Round-trip serde coverage for the wire types durable runners persist.

use paigasus_helikon_core::{
    FinishReason, ModelRequest, ModelSettings, ResponseFormat, ToolCallOutcome, ToolCallRequest,
    ToolChoice, ToolDef,
};

fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned>(v: &T) -> T {
    serde_json::from_str(&serde_json::to_string(v).expect("serialize")).expect("deserialize")
}

#[test]
fn model_request_round_trips() {
    let req = ModelRequest {
        messages: vec![],
        tools: vec![ToolDef {
            name: "echo".into(),
            description: "d".into(),
            schema: serde_json::json!({"type": "object"}),
        }],
        model_settings: ModelSettings {
            temperature: Some(0.2),
            tool_choice: Some(ToolChoice::Required),
            response_format: Some(ResponseFormat::JsonSchema {
                name: "Out".into(),
                schema: serde_json::json!({"type": "object"}),
                strict: true,
            }),
            ..ModelSettings::new()
        },
    };
    let back = round_trip(&req);
    assert_eq!(back.tools[0].name, "echo");
    assert_eq!(back.model_settings.temperature, Some(0.2));
}

#[test]
fn tool_call_types_round_trip() {
    let call = ToolCallRequest {
        call_id: "c1".into(),
        name: "echo".into(),
        args: serde_json::json!({"x": 1}),
    };
    let outcome = ToolCallOutcome {
        call_id: "c1".into(),
        result: Err("boom".into()),
    };
    assert_eq!(round_trip(&call).call_id, "c1");
    assert!(round_trip(&outcome).result.is_err());
}

#[test]
fn finish_reason_round_trips() {
    let r: FinishReason = round_trip(&FinishReason::Other("weird".into()));
    assert_eq!(r, FinishReason::Other("weird".into()));
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p paigasus-helikon-core --test wire_serde` → FAIL: the derives don't exist yet (trait-bound errors).

- [ ] **Step 3: Add the derives.** On each of `ModelRequest`, `ModelSettings`, `ToolDef`, `ToolChoice`, `ResponseFormat`, `FinishReason` (model.rs) and `ToolCallRequest`, `ToolCallOutcome` (loop_state.rs), extend the existing `#[derive(...)]` with `serde::Serialize, serde::Deserialize`. No `#[serde(...)]` attributes — plain field/variant names are the wire format. `ModelRequest`/`ModelSettings` are `#[non_exhaustive]`; that composes fine with serde derives.

- [ ] **Step 4: Verify** — `cargo test -p paigasus-helikon-core --test wire_serde` → PASS; `cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings` → clean.

- [ ] **Step 5: Commit** — `git add crates/paigasus-helikon-core/src/model.rs crates/paigasus-helikon-core/src/loop_state.rs crates/paigasus-helikon-core/tests/wire_serde.rs && git commit -m "feat(core): SMA-332 derive serde on model and loop wire types"`

---

### Task 2: Core — `ModelTurnAccumulator`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs` (add `ModelTurn`, `ModelTurnAccumulator` + unit tests in a `#[cfg(test)]` module)
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (refactor the stream loop at ~940–1035 and delete the now-moved `ToolCallAccum`/`build_items` at 442–486)

**Interfaces:**
- Consumes: `ModelEvent`, `Item`, `ContentPart`, `TokenUsage`, `FinishReason`.
- Produces (Task 7 depends on these exact signatures):

```rust
/// One fully-aggregated model turn.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ModelTurn {
    pub items: Vec<crate::Item>,
    pub usage: crate::TokenUsage,
    pub finish_reason: FinishReason,
}

/// Accumulates a streamed model response into a [`ModelTurn`].
#[derive(Debug)]
pub struct ModelTurnAccumulator { /* private: agent_name, text, reasoning, tool_accum, finish_reason, latest_usage */ }

impl ModelTurnAccumulator {
    pub fn new(agent_name: impl Into<String>) -> Self;
    /// Feed one successful model event. `Err(ModelEvent)`s are the caller's concern.
    pub fn observe(&mut self, event: &ModelEvent);
    /// Reassemble. `Err(String)` = invalid JSON in accumulated tool-call args.
    pub fn finish(self) -> Result<ModelTurn, String>;
}
```

- [ ] **Step 1: Write failing unit tests** (in `model.rs` `#[cfg(test)] mod model_turn_tests`):

```rust
#[test]
fn accumulates_text_reasoning_and_tool_calls() {
    let mut acc = ModelTurnAccumulator::new("a1");
    acc.observe(&ModelEvent::ReasoningDelta { text: "think".into() });
    acc.observe(&ModelEvent::TokenDelta { text: "hel".into() });
    acc.observe(&ModelEvent::TokenDelta { text: "lo".into() });
    acc.observe(&ModelEvent::ToolCallDelta { call_id: "c1".into(), name: Some("echo".into()), args_delta: "{\"x\"".into() });
    acc.observe(&ModelEvent::ToolCallDelta { call_id: "c1".into(), name: None, args_delta: ":1}".into() });
    acc.observe(&ModelEvent::Usage { input_tokens: 10, output_tokens: 5, cached_input_tokens: None, reasoning_tokens: None });
    acc.observe(&ModelEvent::Finish { reason: crate::FinishReason::ToolCalls });
    let turn = acc.finish().unwrap();
    assert_eq!(turn.items.len(), 2); // AssistantMessage(reasoning+text) + ToolCall
    assert_eq!(turn.usage.input_tokens, 10);
    assert_eq!(turn.usage.total_tokens, 15);
    assert_eq!(turn.finish_reason, crate::FinishReason::ToolCalls);
}

#[test]
fn usage_is_last_wins() {
    let mut acc = ModelTurnAccumulator::new("a1");
    acc.observe(&ModelEvent::Usage { input_tokens: 1, output_tokens: 1, cached_input_tokens: None, reasoning_tokens: None });
    acc.observe(&ModelEvent::Usage { input_tokens: 7, output_tokens: 3, cached_input_tokens: None, reasoning_tokens: None });
    let turn = acc.finish().unwrap();
    assert_eq!(turn.usage.input_tokens, 7); // retained last snapshot, never summed
}

#[test]
fn invalid_tool_args_error() {
    let mut acc = ModelTurnAccumulator::new("a1");
    acc.observe(&ModelEvent::ToolCallDelta { call_id: "c1".into(), name: Some("t".into()), args_delta: "{not json".into() });
    assert!(acc.finish().is_err());
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p paigasus-helikon-core model_turn_tests` → FAIL (types missing).

- [ ] **Step 3: Implement.** Move `ToolCallAccum` + `build_items` from agent.rs into model.rs as private internals of `ModelTurnAccumulator` (BTreeMap keying preserved — deterministic item order). `observe` replicates the loop's accumulation arms exactly, including the `TokenUsage` construction with `total_tokens = input + output`. `finish` calls the moved `build_items(agent_name, text, reasoning, tool_accum)` and defaults `usage`/`finish_reason` exactly as the loop did (`latest_usage.unwrap_or_default()`, initial `FinishReason::Stop`).

- [ ] **Step 4: Refactor `LlmAgent`'s loop** to hold one `ModelTurnAccumulator` instead of the five locals; each match arm calls `acc.observe(&evt)` **and keeps its existing `yield`** (TokenDelta/ReasoningDelta/ToolCallDelta arms) — live streaming unchanged; `Err(e)` arm unchanged. After the stream ends, replace the `build_items(...)` call with `acc.finish()`, splitting `ModelTurn` into the existing variables (`items`, `usage`, `finish_reason`). Note: the loop reads `usage` for span records — take it from the returned `ModelTurn`.

- [ ] **Step 5: Verify no behavior change** — `cargo test -p paigasus-helikon-core` → all green (the existing agent/loop tests are the regression net); clippy clean.

- [ ] **Step 6: Commit** — `git add crates/paigasus-helikon-core/src/model.rs crates/paigasus-helikon-core/src/agent.rs && git commit -m "feat(core): SMA-332 hoist model-turn accumulation into ModelTurnAccumulator"`

---

### Task 3: Core — public tool-call authorize/redact pipeline

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs` (add `RunContext::authorize_tool`)
- Modify: `crates/paigasus-helikon-core/src/control.rs` (`Interceptors::authorize` delegates to it)
- Create: `crates/paigasus-helikon-core/src/tool_exec.rs` (public `execute_tool_call` + `finalize_tool_output`; module registered in lib.rs with `pub use tool_exec::*;`)
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (`run_tools_concurrent` rebuilt on the shared primitives)
- Test: `crates/paigasus-helikon-core/tests/tool_exec.rs` (new)

**Interfaces:**
- Consumes: `Tool`, `ToolContext`, `ToolCallRequest`, `ToolCallOutcome`, `PermissionDecision`, `ToolEffect`, `redaction::{SecretSet, redact}`, `AgentEvent::PermissionDenied`.
- Produces (Task 7 depends on these exact signatures):

```rust
// on RunContext<Ctx>:
/// Authorize one tool call on its effective args: deny rules › guard rules ›
/// allow rules › mode › policy › approval handler (AskUser resolved here,
/// default Deny). Extracted from the loop driver for durable runners.
pub async fn authorize_tool(
    &self,
    tool: &str,
    effect: crate::ToolEffect,
    args: &serde_json::Value,
) -> crate::PermissionDecision;

// in tool_exec.rs:
/// Render a tool's raw JSON output to content parts, applying redaction last.
pub fn finalize_tool_output(
    output: serde_json::Value,
    redact_output: bool,
    secrets: &crate::redaction::SecretSet,
) -> Vec<crate::ContentPart>;

/// The hook-free single-call pipeline durable runners execute:
/// resolve → authorize → invoke → redact → convert. Returns the outcome plus
/// an optional `AgentEvent::PermissionDenied` to surface.
pub async fn execute_tool_call<Ctx>(
    tools: &[std::sync::Arc<dyn crate::Tool<Ctx>>],
    run_ctx: &crate::RunContext<Ctx>,
    tool_ctx: &crate::ToolContext<Ctx>,
    call: &crate::ToolCallRequest,
) -> (crate::ToolCallOutcome, Option<crate::AgentEvent>)
where
    Ctx: Send + Sync + 'static;
```

- [ ] **Step 1: Write failing tests** (`tests/tool_exec.rs`): build a `RunContext<()>` (same construction as `runner.rs`'s test `ctx()` helper) and a secret-leaking echo tool, then assert: (a) allow path invokes and the output is redacted when the secret is in `extra_secrets` (construct the context with a deny-free config and one extra secret; assert the returned `ContentPart::Text` does not contain the secret); (b) a `DenyRule` matching the tool yields `result: Err("permission denied: …")` **and** `Some(AgentEvent::PermissionDenied { .. })`; (c) unknown tool name yields `Err("unknown tool: nope")` and no event; (d) `finalize_tool_output(json!("plain"), false, &SecretSet::from_env_and_extra(&[]))` returns the string verbatim (`Value::String` → text, matching `tool_output_to_content_parts`'s convention).

- [ ] **Step 2: Run to verify failure** — `cargo test -p paigasus-helikon-core --test tool_exec` → FAIL (functions missing).

- [ ] **Step 3: Implement.** Move the **body** of `Interceptors::authorize` into `RunContext::authorize_tool` (same resolution chain, approval handler included); `Interceptors::authorize` becomes `self.ctx.authorize_tool(tool, effect, args).await`. `finalize_tool_output` = the redact-then-`tool_output_to_content_parts` tail of the current pipeline (move `tool_output_to_content_parts` into tool_exec.rs, `pub(crate)` re-used by agent.rs). `execute_tool_call` = resolve tool by name → `run_ctx.authorize_tool` (honoring `Replace`) → `tool.invoke(tool_ctx, args)` → `finalize_tool_output(json, run_ctx.redact_output(), &SecretSet::from_env_and_extra(run_ctx.extra_secrets()))` — error strings byte-identical to `run_tools_concurrent`'s (`"permission denied: {reason}"`, `"unknown tool: {name}"`). Add `pub fn redact_output(&self) -> bool` and `pub fn extra_secrets(&self) -> &[String]` accessors on `RunContext` if not already public.

- [ ] **Step 4: Refactor `run_tools_concurrent`** to compose the same primitives with its hook interleave: pre-hook → `ctx.authorize_tool` → invoke → post-hook → `finalize_tool_output`. Its observable behavior must not change — the existing agent tests (redaction test at agent.rs:1424, permission tests) are the regression net.

- [ ] **Step 5: Verify** — `cargo test -p paigasus-helikon-core` → all green; clippy clean. Also run `cargo test -p paigasus-helikon-runtime-tokio` (consumes core).

- [ ] **Step 6: Commit** — `git add crates/paigasus-helikon-core/src/ crates/paigasus-helikon-core/tests/tool_exec.rs && git commit -m "feat(core): SMA-332 expose tool-call authorize and redact pipeline for durable runners"`

---

### Task 4: Temporal deps + skeleton + calibration checkpoints

**Files:**
- Modify: `Cargo.toml` (workspace deps), `deny.toml` (license clarifies)
- Modify: `crates/paigasus-helikon-runtime-temporal/Cargo.toml` (real deps; **keep** `version = "0.0.0"` + `publish = false` until Task 15)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/lib.rs` (crate docs + module skeleton)

**Interfaces:**
- Produces: compiling crate skeleton with modules `payloads`, `driver`, `activities`, `worker`, `runner`, `error`, `workflow`; workspace dep lines Tasks 5–9 build on.

- [ ] **Step 1: Add workspace deps** to root `Cargo.toml` `[workspace.dependencies]`:

```toml
temporalio-sdk      = { version = "0.5", default-features = false }
temporalio-client   = { version = "0.5", default-features = false, features = ["tls-aws-lc"] }
temporalio-sdk-core = { version = "0.5", default-features = false, features = ["tls-aws-lc"] }
temporalio-common   = { version = "0.5", default-features = false }
```

(Adjust feature names to the actual 0.5 feature list if `cargo metadata` rejects these — the `tls-aws-lc` feature exists on client+core per research; `temporalio-sdk` passes `default-features = false` through to them.)

- [ ] **Step 2: Crate manifest.** Replace the stub dependency-less `Cargo.toml` dependency section with: `paigasus-helikon-core`, `paigasus-helikon-runtime-tokio` (for `RetryingModel` docs-linking only if needed — omit if unused), the four temporalio crates, `async-trait`, `tokio`, `futures-core`, `futures-util`, `serde`, `serde_json`, `thiserror`, `tracing`, `uuid`, `anyhow` (all `workspace = true`). Dev-deps: `tokio` (macros), `futures-util`.

- [ ] **Step 3: CHECKPOINT — protoc.** Run `cargo check -p paigasus-helikon-runtime-temporal`. If the build errors demanding a system `protoc`, **STOP the task and report** (spec §7.11: this changes CI + contributor setup and must be surfaced, not worked around silently). Expected per research: no protoc needed (published crates ship generated code).

- [ ] **Step 4: CHECKPOINT — TLS provider.** Run `cargo tree -p paigasus-helikon-runtime-temporal -i ring 2>&1 | head -5` and `cargo tree -p paigasus-helikon-runtime-temporal -e features | grep -i "tls-ring" | head -5`. Both must come back empty/no-match. Then the workspace-wide check: `cargo test --workspace --all-features -p paigasus-helikon-core 2>/dev/null || cargo check --workspace --all-features` compiles without a second CryptoProvider. If `ring` enters the graph, adjust features until clean; if it cannot be made clean, STOP and report.
  Record the `ephemeral-server` decision: run `cargo add --dry-run` is not enough — check `temporalio-sdk-core`'s `ephemeral-server` feature deps on docs.rs/crates.io metadata; if it pulls `reqwest` with ring-default TLS, integration tests use the external-server env gate ONLY (spec §5.10) and the feature is never enabled.

- [ ] **Step 5: deny.toml clarifies.** Append:

```toml
# temporalio-* crates declare MIT via license-file (crates.io: "non-standard").
[[licenses.clarify]]
name = "temporalio-sdk"
expression = "MIT"
license-files = [{ path = "LICENSE.txt", hash = 0 }]
```

…one block per temporalio crate that appears in `cargo deny check licenses` output (run it; add exactly the failing set; the hash values come from cargo-deny's error output — run once, copy the printed hashes).

- [ ] **Step 6: Skeleton lib.rs** with crate-level docs (one-paragraph durable-runner summary; full docs land in Task 10) and empty `pub mod` declarations gated to compile (`payloads`, `error` first; others added by their tasks). Verify `cargo check -p paigasus-helikon-runtime-temporal` and `cargo deny check licenses` pass.

- [ ] **Step 7: Commit** — `git add Cargo.toml Cargo.lock deny.toml crates/paigasus-helikon-runtime-temporal/ && git commit -m "feat(runtime-temporal): SMA-332 add temporal sdk dependencies and crate skeleton"`

---

### Task 5: Temporal payloads + error kinds

**Files:**
- Create: `crates/paigasus-helikon-runtime-temporal/src/payloads.rs`
- Create: `crates/paigasus-helikon-runtime-temporal/src/error.rs`

**Interfaces:**
- Consumes: Task 1 serde derives, Task 2 `ModelTurn`.
- Produces (Tasks 6–9 depend on these exact shapes):

```rust
// payloads.rs
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkflowInput {
    pub agent_name: String,
    /// Session snapshot ++ new-turn messages (NO system item — the workflow
    /// seeds it from the render_instructions activity result).
    pub conversation: Vec<paigasus_helikon_core::Item>,
    pub config: DriverConfig,
    /// RunConfig::timeout as milliseconds; None = no deadline.
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DriverConfig {
    pub max_turns: u32,
    pub parallel_tool_call_limit: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelTurnResult(pub paigasus_helikon_core::ModelTurn);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FinalOutputPayload {
    pub content: Vec<paigasus_helikon_core::ContentPart>,
    pub usage: paigasus_helikon_core::TokenUsage,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RunStatusPayload {
    Completed(FinalOutputPayload),
    AgentFailed(ErrorKindPayload),
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DurableRunOutcome {
    pub status: RunStatusPayload,
    pub events: Vec<paigasus_helikon_core::AgentEvent>,
    pub usage: paigasus_helikon_core::TokenUsage,
}

// error.rs
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ErrorKindPayload {
    MaxTurnsExceeded(u32),
    InvalidStructuredOutput { schema_errors: Vec<String>, final_text: String },
    Model { message: String },
    HandoffUnsupported { target: String },
    Other { message: String },
}

impl ErrorKindPayload {
    /// Lossy projection from AgentError (anyhow variants degrade to message).
    pub fn from_agent_error(e: &paigasus_helikon_core::AgentError) -> Self;
    /// Reconstruction into the typed error surface the Runner returns.
    pub fn into_agent_error(self) -> paigasus_helikon_core::AgentError;
}
```

- [ ] **Step 1: Write failing tests** (inline `#[cfg(test)]`): round-trip `DurableRunOutcome` with each `RunStatusPayload` variant; `ErrorKindPayload::from_agent_error(&AgentError::MaxTurnsExceeded(16)).into_agent_error()` matches `AgentError::MaxTurnsExceeded(16)` (use `assert_matches`-style matching, `AgentError` isn't `PartialEq`); same for `InvalidStructuredOutput`; an `AgentError::Other(anyhow!("x"))` degrades to `Other { message }` and reconstructs as `AgentError::Other` with that message.

- [ ] **Step 2: Run to verify failure**, then **Step 3: implement** both files exactly as the interface block (add `///` docs on every pub item — the docs gate). `from_agent_error` matches on `AgentError` variants (`MaxTurnsExceeded`, `InvalidStructuredOutput`, `Model(e)` → message via `to_string()`, everything else → `Other`).

- [ ] **Step 4: Verify** — `cargo test -p paigasus-helikon-runtime-temporal` → PASS; clippy clean.

- [ ] **Step 5: Commit** — `git commit -m "feat(runtime-temporal): SMA-332 add workflow and activity payload types"` (stage the two files + lib.rs module lines).

---

### Task 6: Temporal `DurableDriver` (pure step machine)

**Files:**
- Create: `crates/paigasus-helikon-runtime-temporal/src/driver.rs` (+ `pub mod driver;`)

**Interfaces:**
- Consumes: Task 5 payloads; `core::{transition, LoopState, TransitionInput, TransitionCtx, NextAction, ToolDef, ModelSettings, OutputType, Item, AgentEvent, TokenUsage, ContentPart}`.
- Produces (Task 8's workflow is a mechanical executor of this):

```rust
/// What the workflow must do next.
#[derive(Debug)]
pub enum DriverEffect {
    /// Run the render_instructions activity (always the first effect).
    RenderInstructions,
    CallModel(paigasus_helikon_core::ModelRequest),
    ExecuteTools(Vec<paigasus_helikon_core::ToolCallRequest>),
    Finished(crate::payloads::DurableRunOutcome),
}

/// Static agent definition the driver plans against (worker-registered).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentPlan {
    pub tool_defs: Vec<paigasus_helikon_core::ToolDef>,
    pub model_settings: paigasus_helikon_core::ModelSettings,
    pub output: Option<paigasus_helikon_core::OutputType>,
}

pub struct DurableDriver { /* private */ }

impl DurableDriver {
    pub fn new(input: crate::payloads::WorkflowInput, plan: AgentPlan) -> Self;
    pub fn next_effect(&mut self) -> DriverEffect;
    /// Result of RenderInstructions: seed [System] ++ conversation, emit RunStarted.
    pub fn apply_instructions(&mut self, system_text: String);
    pub fn apply_model(&mut self, turn: crate::payloads::ModelTurnResult);
    /// Model activity failed terminally (non-retryable ErrorKindPayload json).
    pub fn apply_model_failure(&mut self, kind: crate::error::ErrorKindPayload);
    /// Outcomes MUST be passed in original call order (workflow joins in order).
    pub fn apply_tools(&mut self, outcomes: Vec<paigasus_helikon_core::ToolCallOutcome>);
    /// Cancel/timeout interruption → partial outcome with events so far.
    pub fn interrupt(self, kind: InterruptKind) -> crate::payloads::DurableRunOutcome;
}

#[derive(Debug, Clone, Copy)]
pub enum InterruptKind { Cancelled, TimedOut }
```

Note `OutputType` must be `Clone + Serialize + Deserialize` for `AgentPlan` — check `agent.rs`'s `OutputType` (it is `Clone`; it wraps name + `schemars` schema value). If it lacks serde derives, add them in core in this task (same additive pattern as Task 1; include in the Task 1 test file) — flag it in the commit message trailer body.

- [ ] **Step 1: Write failing driver tests** (inline `#[cfg(test)]`; representative set — implement ALL of these):

```rust
fn input(msgs: Vec<Item>) -> WorkflowInput { /* agent_name: "a1", conversation: msgs, config: DriverConfig { max_turns: 4, parallel_tool_call_limit: None }, timeout_ms: None */ }
fn plan_no_tools() -> AgentPlan { /* empty tool_defs, default settings, no output */ }
fn user(text: &str) -> Item { Item::UserMessage { content: vec![ContentPart::Text { text: text.into() }] } }
fn model_text_turn(text: &str) -> ModelTurnResult { /* ModelTurn { items: [AssistantMessage(text)], usage: 1/1, finish_reason: Stop } */ }

#[test]
fn first_effect_is_render_instructions_then_model() { /* new → RenderInstructions; apply_instructions("sys") → CallModel; the request's messages[0] is Item::System("sys"), messages[1] the user msg; events contain RunStarted{agent:"a1"} then TurnStarted{0} */ }

#[test]
fn empty_system_text_is_omitted() { /* apply_instructions("") → CallModel request messages[0] is the user message */ }

#[test]
fn text_response_completes() { /* apply_model(text turn) → Finished(Completed); events end RunCompleted; usage aggregated */ }

#[test]
fn tool_roundtrip_appends_in_call_order() { /* model turn with 2 tool calls → ExecuteTools(2); apply_tools([c1, c2]) → next CallModel request contains ToolResult c1 BEFORE c2 and the prior assistant+toolcall items */ }

#[test]
fn conversation_appends_are_applied() { /* plan with output type; model returns non-conforming text on the Finalizing turn → next CallModel request's last message is the synthesized repair UserMessage (mirrors transition's conversation_appends) */ }

#[test]
fn handoff_terminates_with_unsupported_error() { /* plan whose transition yields NextAction::Handoff is impossible here (handoffs: &[] in TransitionCtx) — instead assert the driver's defensive arm: constructing a driver is not enough, so this test asserts TransitionCtx is built with empty handoffs AND that a Failed state from transition maps to AgentFailed */ }

#[test]
fn max_turns_exceeded_maps_to_typed_error() { /* max_turns: 1, tool-call turn → after apply_tools, next transition fails; Finished(AgentFailed(MaxTurnsExceeded(1))) */ }

#[test]
fn interrupt_returns_partial_events() { /* after one model turn: interrupt(Cancelled) → status Cancelled, events non-empty (RunStarted, TurnStarted, MessageOutput...) */ }

#[test]
fn model_failure_is_terminal_with_events() { /* apply_model_failure(Model{message}) → Finished(AgentFailed(Model)); events include RunFailed */ }
```

- [ ] **Step 2: Run to verify failure**, then **Step 3: implement** the driver:
  - State: `conversation: Vec<Item>`, `loop_state: LoopState`, `pending_input: Option<TransitionInput>`, `events: Vec<AgentEvent>`, `usage: TokenUsage` (mirror of the state machine's carried usage — read final usage from `Done`/terminal events), `phase` enum (`AwaitingInstructions` → `Driving` → `Done(DurableRunOutcome)`).
  - `apply_instructions`: prepend system item when non-empty; push `RunStarted`; set `pending_input = TransitionInput::Start { messages: <the input messages> }` and `loop_state = CallingModel { turn: 0, usage: default }`.
  - `next_effect` (in `Driving`): run `transition(&loop_state, pending_input.take(), &TransitionCtx { tools: &plan.tool_defs, model_settings: &plan.model_settings, max_turns, conversation: &self.conversation, output: plan.output.as_ref(), handoffs: &[] })`; extend events; extend conversation with `conversation_appends`; store `next_state`; map `NextAction`: `CallModel { request }` → `DriverEffect::CallModel(request)`; `ExecuteTools { calls }` → effect; `Terminate` → build `DurableRunOutcome` from `Done(FinalOutput)`/`Failed(err)` (usage from `FinalOutput.usage` or last `RunCompleted`; `Failed` → `ErrorKindPayload::from_agent_error`); `Handoff` → `AgentFailed(HandoffUnsupported)` (defensive — unreachable with empty handoffs).
  - `apply_model`: extend conversation with `turn.0.items`, set `pending_input = ModelResponse { items, usage, finish_reason }`.
  - `apply_tools`: extend conversation with one `Item::ToolResult { call_id, content }` per outcome **in vec order** (content: `Ok(parts)` or `Err(e)` → `vec![ContentPart::Text { text: e }]`, mirroring transition's own rendering), set `pending_input = ToolResults { outcomes }`.
    **Ordering note:** `transition`'s `ToolResults` arm ALSO renders `ToolOutputItem` events and the driver must NOT double-append `ToolResult` items — read `loop_state.rs:406-450` first: transition emits events but does NOT append conversation items (the ephemeral driver appends at agent.rs:1047). So: driver appends items; transition emits events. Keep exactly that split.
  - `interrupt`: consume self, wrap events+usage with the given status.

- [ ] **Step 4: Verify** — `cargo test -p paigasus-helikon-runtime-temporal driver` → all green; clippy clean.

- [ ] **Step 5: Commit** — `git commit -m "feat(runtime-temporal): SMA-332 add pure durable loop driver"`

---

### Task 7: Temporal worker + activities

**Files:**
- Create: `crates/paigasus-helikon-runtime-temporal/src/activities.rs`
- Create: `crates/paigasus-helikon-runtime-temporal/src/worker.rs`

**Interfaces:**
- Consumes: Task 2 `ModelTurnAccumulator`, Task 3 `execute_tool_call`, Task 5 payloads, Task 6 `AgentPlan`; `LlmAgent` pub fields; temporalio-sdk worker API (calibrated in Task 4).
- Produces:

```rust
// worker.rs
pub struct TemporalAgentWorker { /* private */ }
pub struct TemporalAgentWorkerBuilder<Ctx> { /* private */ }

impl TemporalAgentWorker {
    pub fn builder<Ctx: Send + Sync + 'static>() -> TemporalAgentWorkerBuilder<Ctx>;
}

impl<Ctx: Send + Sync + 'static> TemporalAgentWorkerBuilder<Ctx> {
    pub fn task_queue(self, queue: impl Into<String>) -> Self;
    pub fn client(self, client: temporalio_client::Client) -> Self;  // exact type per Task 4 calibration
    pub fn with_ctx(self, factory: impl Fn() -> Ctx + Send + Sync + 'static) -> Self;
    /// Snapshot an LlmAgent into a DurableAgentDef. Errors when the agent has
    /// hooks, guardrails, or handoffs (v0 fail-fast, spec §5.7).
    pub fn register<M, T>(self, agent: std::sync::Arc<paigasus_helikon_core::LlmAgent<Ctx, M, T>>) -> Result<Self, RegistrationError>
    where M: paigasus_helikon_core::Model + 'static, T: Send + Sync + 'static;
    pub fn model_retry_policy(self, p: RetryPolicyConfig) -> Self;
    pub fn tool_retry_policy(self, p: RetryPolicyConfig) -> Self;
    pub fn build(self) -> Result<TemporalAgentWorker, WorkerBuildError>;
}

impl TemporalAgentWorker {
    /// Serve the task queue until shutdown. 
    pub async fn run(self) -> Result<(), WorkerRunError>;
}

// activities.rs — inner functions unit-testable WITHOUT Temporal:
pub(crate) struct DurableAgentDef<Ctx> {
    pub name: String,
    pub instructions: std::sync::Arc<dyn paigasus_helikon_core::Instructions<Ctx>>,
    pub model: std::sync::Arc<dyn paigasus_helikon_core::Model>,
    pub tools: Vec<std::sync::Arc<dyn paigasus_helikon_core::Tool<Ctx>>>,
    pub plan: crate::driver::AgentPlan,
}

pub(crate) async fn call_model_inner(
    model: &dyn paigasus_helikon_core::Model,
    agent_name: &str,
    request: paigasus_helikon_core::ModelRequest,
    cancel: paigasus_helikon_core::CancellationToken,
) -> Result<crate::payloads::ModelTurnResult, crate::error::ErrorKindPayload>;

pub(crate) async fn invoke_tool_inner<Ctx: Send + Sync + 'static>(
    def: &DurableAgentDef<Ctx>,
    run_ctx: &paigasus_helikon_core::RunContext<Ctx>,
    call: paigasus_helikon_core::ToolCallRequest,
) -> paigasus_helikon_core::ToolCallOutcome;

pub(crate) async fn render_instructions_inner<Ctx: Send + Sync + 'static>(
    def: &DurableAgentDef<Ctx>,
    run_ctx: &paigasus_helikon_core::RunContext<Ctx>,
) -> String;
```

- [ ] **Step 1: Failing tests for the inner functions + registration fail-fast** (inline): (a) `call_model_inner` with a scripted mock `Model` (emit deltas+usage+finish) returns the aggregated `ModelTurn`; a mock returning `Err(ModelError::Unavailable)` at invoke returns `Err(ErrorKindPayload::Model{..})`; a stream-level `Err` event likewise; (b) `invoke_tool_inner` executes `execute_tool_call` (echo tool round-trip; unknown tool → Err string outcome — never a panic/Err return); (c) `TemporalAgentWorkerBuilder::register` rejects an `LlmAgent` with a hook (`hooks: vec![Arc::new(SomeHook)]`) with `RegistrationError::UnsupportedFeature("hooks")` — likewise `handoffs`, `input_guardrails`, `output_guardrails`; accepts a plain agent and snapshots `plan.tool_defs` from `agent.tools`.

- [ ] **Step 2: Run to verify failure**, then **Step 3: implement.** `register` checks the four vecs are empty (`!agent.hooks.is_empty()` → error, etc.), then builds `DurableAgentDef` (upcast `Arc<M>` → `Arc<dyn Model>` — works because `M: Model + 'static` is concrete here; `ToolDef` snapshots as in agent.rs:695-702). `call_model_inner` = `model.invoke(request, cancel)` → drain stream through `ModelTurnAccumulator::new(agent_name)`; stream `Err(e)` → `ErrorKindPayload::Model { message: e.to_string() }`; `finish()` Err → `Other`. `invoke_tool_inner` = `execute_tool_call(&def.tools, run_ctx, &run_ctx.to_tool_context(), &call).0` (the `PermissionDenied` event, when present, is appended to the outcome's Err string context by the pipeline already — v0 durable event log doesn't carry per-tool permission events; document that in the fn docs). `render_instructions_inner` = `def.instructions.render(run_ctx)`.

- [ ] **Step 4: Wire the Temporal activity layer** (thin, calibrated to the real 0.5 API): an `#[activities]` impl over a struct holding `Arc<DurableAgentDef<Ctx>>` + ctx factory, with `#[activity]` fns `render_instructions() -> String`, `call_model(ModelRequest) -> ModelTurnResult`, `invoke_tool(ToolCallRequest) -> ToolCallOutcome`; `call_model` converts `Err(ErrorKindPayload)` into a **non-retryable** `ActivityError` carrying `serde_json::to_string(&kind)` as its message/details (exact non-retryable constructor per calibration). Worker `build()`/`run()` assemble `CoreRuntime`/`WorkerOptions` with the workflow (Task 8) + activities registered, retry policies applied (defaults: SDK defaults for both; non-retryable typing carries the ADR-10 semantics).

- [ ] **Step 5: Verify** — `cargo test -p paigasus-helikon-runtime-temporal` (inner tests green; activity layer compiles); clippy clean. **Step 6: Commit** — `git commit -m "feat(runtime-temporal): SMA-332 add worker builder, registration fail-fast, and activities"`

---

### Task 8: Temporal workflow + TemporalRunner

**Files:**
- Create: `crates/paigasus-helikon-runtime-temporal/src/workflow.rs`
- Create: `crates/paigasus-helikon-runtime-temporal/src/runner.rs`
- Modify: `crates/paigasus-helikon-runtime-temporal/src/error.rs` (outcome→RunResult/RunError mapping)

**Interfaces:**
- Consumes: Tasks 5–7; temporalio-sdk workflow API + temporalio-client (calibrated).
- Produces:

```rust
// workflow.rs — AgentLoopWorkflow #[workflow]: loops DurableDriver::next_effect():
//   RenderInstructions → start_activity(render_instructions) → apply_instructions
//   CallModel(req)     → start_activity(call_model, req)     → apply_model / apply_model_failure (parse ErrorKindPayload from the activity failure details)
//   ExecuteTools(c)    → start c invoke_tool activities concurrently (chunked by parallel_tool_call_limit), join preserving input order → apply_tools
//   Finished(outcome)  → return outcome
// The whole loop races: (a) a durable timer of input.timeout_ms → driver.interrupt(TimedOut);
// (b) workflow cancellation → driver.interrupt(Cancelled). Both return DurableRunOutcome normally.

// runner.rs
pub struct TemporalRunner { /* private: client handle, TemporalRunnerConfig */ }
pub struct TemporalRunnerConfig {
    pub task_queue: String,
    /// Workflow id assigned per run; default "helikon-run-{uuid-v4}" (client-side).
    pub workflow_id: Option<String>,
    /// Backstop margin added to timeout_ms for the workflow execution timeout.
    pub execution_timeout_margin: std::time::Duration, // default 60s
}
impl TemporalRunner {
    pub fn new(client: temporalio_client::Client, config: TemporalRunnerConfig) -> Self;
}
// #[async_trait] impl<Ctx> Runner<Ctx> for TemporalRunner — run(), run_streamed() (buffered)

// error.rs addition:
pub(crate) fn outcome_to_run_result(
    outcome: crate::payloads::DurableRunOutcome,
) -> Result<paigasus_helikon_core::RunResult, paigasus_helikon_core::RunError>;
```

- [ ] **Step 1: Failing tests for `outcome_to_run_result`** (pure, no server): `Completed` → `Ok(RunResult { final_output: FinalOutput-as_text convention (concatenated ContentPart::Text), events, usage })`; `AgentFailed(MaxTurnsExceeded(4))` → `Err(RunError::Agent(AgentError::MaxTurnsExceeded(4)))`; `Cancelled` → `Err(RunError::Cancelled)`; `TimedOut` → `Err(RunError::Timeout)`.

- [ ] **Step 2: Run to verify failure**, then **Step 3: implement** mapping + workflow + runner:
  - `Runner::run`: (1) `load_and_record` semantics — copy `TokioRunner`'s: `session.snapshot()` (hard error on failure), `SessionRecorder::new(agent.name())` + `record_input(&input.messages)`, merged = snapshot ++ input; (2) start workflow with `WorkflowInput { agent_name: agent.name().into(), conversation: merged, config: from RunConfig, timeout_ms }`, workflow-execution timeout = timeout_ms + margin (only when Some); (3) spawn a watcher: `ctx.cancel().cancelled()` → client cancel-workflow request; (4) await result → `DurableRunOutcome`; (5) feed every event through the recorder (`recorder.observe(ev)` loop), `finalize` (best-effort append, identical to `TokioRunner::finalize`); (6) `outcome_to_run_result`. Infra failures (start/await errors): still `finalize` (recorder holds the new-turn input) then `Err(RunError::Other(anyhow!(...)))`.
  - `Runner::run_streamed`: call the same path to completion, then `Ok(RunResultStreaming::with_failure(Box::pin(futures_util::stream::iter(events)), failure_slot))` where the slot is pre-set for failed runs (set the `AgentError` before returning, so `collect()` surfaces it) — document "buffered, not live" on the method.
- [ ] **Step 4: Verify** — `cargo test -p paigasus-helikon-runtime-temporal` green; `cargo check --workspace --all-features`; clippy clean. **Step 5: Commit** — `git commit -m "feat(runtime-temporal): SMA-332 add agent loop workflow and TemporalRunner"`

---

### Task 9: Temporal env-gated integration tests + local validation

**Files:**
- Create: `crates/paigasus-helikon-runtime-temporal/tests/temporal_live.rs`

**Interfaces:** Consumes everything from Tasks 4–8. Env gate: `TEMPORAL_TEST_SERVER=<host:port>` (e.g. `localhost:7233`).

- [ ] **Step 1: Write the suite** (loud-skip pattern copied from `paigasus-helikon-tools/tests/forkd_live.rs`): a `fn gate() -> Option<String>` reading the env var, printing `SKIPPED: set TEMPORAL_TEST_SERVER=<addr> to run` when absent. Shared helpers: `ScriptedModel` (a `Model` returning pre-programmed turns: turn 1 = one tool call, turn 2 = final text; an `AtomicU32` call counter), `BlockOnceTool` (first invocation across worker generations blocks forever — flag via a tempfile path so it survives the in-process "crash"; subsequent invocations return instantly), unique task-queue names per test (uuid). Tests:
  1. `happy_path_tool_roundtrip`: start worker task, run `TemporalRunner::run`, assert final text + events shape + model called exactly twice.
  2. `crash_resume_mid_tool_call` (the AC): tool retry policy `max_attempts: 3`, start-to-close 5s; spawn worker on `tokio::task`, start the run on a `tokio::task`, wait until the tool is mid-block (tempfile flag), `abort()` the worker task; start a fresh worker on the SAME queue; await the run → `Ok`; assert model-call counter == 2 (turn-0 model activity NOT re-executed — served from history) and tool invoked twice (blocked attempt + successful retry).
  3. `cancel_returns_cancelled_and_persists_partial`: cancel mid-run via `ctx.cancel()`; assert `Err(RunError::Cancelled)` and `session.snapshot()` contains the first-turn messages.
  4. `session_round_trip`: two sequential runs on one `MemorySession`; scripted model asserts the second run's request contains the first run's user+assistant items.
- [ ] **Step 2: Run gated-off** — `cargo test -p paigasus-helikon-runtime-temporal --test temporal_live` → all print SKIPPED, exit 0.
- [ ] **Step 3: Run live locally** — `temporal server start-dev --headless` (install: `brew install temporal`), then `TEMPORAL_TEST_SERVER=localhost:7233 cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1` → 4 passed. Record the output in the task report (it goes in the PR body).
- [ ] **Step 4: Commit** — `git commit -m "test(runtime-temporal): SMA-332 add env-gated live integration suite incl. crash-resume AC"`

---

### Task 10: Temporal crate docs + README

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/lib.rs` (full crate docs)
- Modify: `crates/paigasus-helikon-runtime-temporal/README.md` (replace stub)

- [ ] **Step 1: lib.rs crate docs** covering (spec §§5.7–5.11 condensed): v0 constraint set; worker-side posture (§5.8, verbatim warning that caller permissions/hooks do not propagate and Temporal history is a persistence boundary); retry semantics + tool idempotency (§5.9); payload budget arithmetic (§5.11: ~1.5 MB conversation JSON ≈ 15–20 turns at ≤50 KB tool outputs); upgrade discipline (§5.10: drain before redeploy, blue-green task queues). Rustdoc example (` ```no_run `) showing worker + runner setup.
- [ ] **Step 2: README** — crates.io landing: what it is, `cargo add paigasus-helikon-runtime-temporal` + `cargo add paigasus-helikon --features runtime-temporal`, quickstart (worker bin + client snippet), dev-server validation one-liner, the four doc topics above as short sections linking docs.rs. Keep every fenced Rust block ` ```no_run ` or ` ```ignore ` (only the facade README is doctest-compiled, but stay consistent).
- [ ] **Step 3: Verify docs gate** — `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-runtime-temporal --no-deps` → clean. **Step 4: Commit** — `git commit -m "docs(runtime-temporal): SMA-332 add crate docs and README"`

---

### Task 11: MCP — stateless streamable-HTTP config

**Files:**
- Modify: `crates/paigasus-helikon-mcp/src/server.rs`
- Test: extend the crate's existing server tests (same file or `tests/`)

**Interfaces:**
- Produces (Task 14 consumes):

```rust
impl<Ctx: Send + Sync + 'static> McpAgentServer<Ctx> {
    /// Like streamable_http_service, with an explicit rmcp config (e.g. stateless mode).
    pub fn streamable_http_service_with(
        &self,
        config: rmcp::transport::StreamableHttpServerConfig,
    ) -> Result<StreamableHttpService<AgentMcpHandler<Ctx>, LocalSessionManager>, McpError>;
}
```

- [ ] **Step 1: Read rmcp 1.7's `StreamableHttpServerConfig`** (docs.rs) — confirm the stateless field (research says stateless mode exists; find the exact field, e.g. `stateful_mode: bool`) and that stateless mode accepts requests carrying an unknown pre-set `Mcp-Session-Id`. If stateless mode still validates session ids, note it and implement the Task 14 fallback (accept-any session manager) — record which path was taken.
- [ ] **Step 2: Failing test**: build the service via `streamable_http_service_with(StreamableHttpServerConfig { stateful_mode: false, ..Default::default() })`, mount on an axum Router, POST a `tools/list` JSON-RPC request **with header `Mcp-Session-Id: platform-generated-id-0123456789abcdef`** → 200-class response, not 4xx.
- [ ] **Step 3: Implement** (`streamable_http_service` delegates to `_with(Default::default())`). **Step 4:** tests + clippy green. **Step 5: Commit** — `git commit -m "feat(mcp): SMA-332 expose stateless streamable-http server config"`

---

### Task 12: AgentCore — crate skeleton, server builder, /ping

**Files:**
- Modify: `crates/paigasus-helikon-runtime-agentcore/Cargo.toml` (deps; keep `0.0.0` + `publish = false` until Task 15)
- Create: `crates/paigasus-helikon-runtime-agentcore/src/{lib.rs,server.rs,ping.rs,error.rs}`

**Interfaces:**
- Consumes: `runtime-axum { default-features = false }` (`SessionProvider`, `InMemorySessionProvider`, `ContextProvider`, `DefaultContextProvider`), `runtime-tokio::TokioRunner`, core.
- Produces:

```rust
pub struct AgentCoreServer<Ctx> { /* private */ }
pub struct AgentCoreServerBuilder<Ctx> { /* agent, runner (default TokioRunner), session_provider (default InMemory), context_provider, run_config */ }
impl<Ctx: Send + Sync + 'static> AgentCoreServer<Ctx> {
    pub fn builder() -> AgentCoreServerBuilder<Ctx>;
    /// Router with POST /invocations + GET /ping (testable via oneshot).
    pub fn router(&self) -> axum::Router;
    /// Bind 0.0.0.0:8080 and serve; logs "ready in {ms}" after bind.
    pub async fn serve(self) -> Result<(), AgentCoreError>;
    pub fn ping_state(&self) -> std::sync::Arc<PingState>;
}
pub struct PingState { /* AtomicBool busy + Mutex<Option<u64>> time_of_last_update */ }
impl PingState {
    pub fn set_busy(&self, busy: bool); // flips status + stamps time_of_last_update ONLY on change
}
```

- [ ] **Step 1: Failing oneshot tests** (`#[cfg(test)]` in ping.rs/server.rs): `GET /ping` → 200, body exactly `{"status":"Healthy"}` (no `time_of_last_update` field before any transition — assert via `serde_json::Value` key absence); after `set_busy(true)` → `{"status":"HealthyBusy","time_of_last_update":<n>}`; `set_busy(true)` twice does not change the stamp.
- [ ] **Step 2: fail → Step 3: implement → Step 4: green + clippy.** Builder defaults: `TokioRunner`, `InMemorySessionProvider::default()`, run_config `RunConfig::default()`; `context_provider` required for non-`()` Ctx (compile-enforced via `with_default_context()` mirror of `AgentServerBuilder`'s pattern — copy that shape).
- [ ] **Step 5: Commit** — `git commit -m "feat(runtime-agentcore): SMA-332 add server skeleton and contract ping endpoint"`

---

### Task 13: AgentCore — /invocations (JSON + SSE)

**Files:**
- Create: `crates/paigasus-helikon-runtime-agentcore/src/{invoke.rs,session.rs}`

**Interfaces:**
- Produces:

```rust
/// Accepted request bodies: {"prompt": "..."} | {"input": "..."} | {"messages":[Item,...]}
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]  // order matters: Messages first (has "messages"), then Prompt, then Input
pub enum InvocationRequest {
    Messages { messages: Vec<paigasus_helikon_core::Item> },
    Prompt { prompt: String },
    Input { input: String },
}
pub(crate) const SESSION_HEADER: &str = "x-amzn-bedrock-agentcore-runtime-session-id";
pub(crate) fn validate_session_id(v: &str) -> Result<&str, AgentCoreError>; // 33..=256 chars
```

- [ ] **Step 1: Failing tests**: (a) DTO: each of the three body forms deserializes to the right variant; `{"prompt":"x","junk":1}` still parses as Prompt (untagged tolerates extra keys — assert actual behavior and document it); (b) `POST /invocations` with `Accept: application/json` + echo agent → 200 JSON `{"final_output": "...", "usage": {...}}`; (c) default (no Accept / `text/event-stream`) → SSE: body contains `data: {"type":"run_started"...` frames and a terminal `run_completed` frame; content-type `text/event-stream`; (d) header `X-Amzn-Bedrock-AgentCore-Runtime-Session-Id: short` (< 33 chars) → 400 with JSON error body; (e) two requests with the same valid session id hit the same session (echo agent that counts prior messages via session snapshot — second response sees turn 1).
- [ ] **Step 2: fail → Step 3: implement.** Handler: extract+validate optional session header → `session_provider.session(id)` → `context_provider.build(...)` → construct `RunContext` (mirror `runtime-axum`'s handler glue — read `crates/paigasus-helikon-runtime-axum/src/handlers/` first and follow its construction pattern exactly) → JSON mode: `runner.run(...)` → map to `{final_output, usage}`; SSE mode: `runner.run_streamed(...)` → `axum::response::sse::Sse` mapping each `AgentEvent` to `Event::default().data(serde_json::to_string(&ev)?)`, keep-alive on. Errors → `error.rs` JSON shapes with appropriate status.
- [ ] **Step 4: green + clippy → Step 5: Commit** — `git commit -m "feat(runtime-agentcore): SMA-332 add invocations handler with sse and json modes"`

---

### Task 14: AgentCore — MCP mode, examples, Dockerfile, size/cold-start script

**Files:**
- Create: `crates/paigasus-helikon-runtime-agentcore/src/mcp.rs` (feature `mcp`, default on: `[features] default = ["mcp"]; mcp = ["dep:paigasus-helikon-mcp"]`)
- Create: `crates/paigasus-helikon-runtime-agentcore/examples/{echo_http.rs,agent_http.rs,mcp_server.rs}`
- Create: `crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile`
- Create: `scripts/agentcore-image-check.sh`
- Create: `docs/runbooks/agentcore-image-check.md`

- [ ] **Step 1: mcp.rs** — `AgentCoreServer::serve_mcp(self) -> Result<(), AgentCoreError>`: build `McpAgentServer` from the configured agent, `streamable_http_service_with(stateless config)` (Task 11; or the fallback session manager if Task 11 recorded that path), mount at `/mcp` on `0.0.0.0:8000` plus the trivial `/ping`. Test: in-process request with pre-set unknown `Mcp-Session-Id` → tools/list succeeds (mirror Task 11's test through the agentcore mount).
- [ ] **Step 2: Examples.** `echo_http.rs`: a 30-line `Agent` impl echoing input (no model dep), served via `AgentCoreServer` — this is the minimal-overhead image binary; startup log `ready in {ms}`. `agent_http.rs`: `cfg`-gated behind a crate example-feature `example-anthropic = ["dep:paigasus-helikon-providers-anthropic"]`; `LlmAgent` with `AnthropicModel` from env (`ANTHROPIC_API_KEY`), same server; ` [[example]] name = "agent_http" required-features = ["example-anthropic"]`. `mcp_server.rs`: echo agent served via `serve_mcp`.
- [ ] **Step 3: Dockerfile** (build-arg `EXAMPLE=echo_http`): stage 1 `rust:1.94-alpine` + `apk add musl-dev build-base cmake perl go` (aws-lc-rs build needs cmake+perl+go under musl; echo build tolerates the extras), `rustup target add aarch64-unknown-linux-musl` (no-op on arm64 alpine), `cargo build --release --locked -p paigasus-helikon-runtime-agentcore --example ${EXAMPLE} ${FEATURES:+--features $FEATURES}`, `strip` the binary; stage 2 `FROM scratch`, `COPY` binary, `EXPOSE 8080 8000`, `ENTRYPOINT`. 
- [ ] **Step 4: `scripts/agentcore-image-check.sh`** — builds `docker build --platform linux/arm64 --build-arg EXAMPLE=echo_http -t helikon-agentcore-echo .` and (with `--features example-anthropic`) the `agent_http` image; asserts `docker image inspect --format '{{.Size}}'` < 30*1024*1024 for the **agent image** (echo asserted too); runs the container, curls `/ping` in a 5 ms loop, computes exec→200 latency, asserts < 50 ms; prints a summary table. Fail loud with the measured numbers.
- [ ] **Step 5: Run it locally** (arm64 host, Docker Desktop): `bash scripts/agentcore-image-check.sh` → record both image sizes + cold-start ms in the task report. **If the agent image ≥ 30 MB or aws-lc-rs/musl fails to build: STOP and report the numbers — the recorded GATE 1 fallback (echo-gated + documented real size) needs Sven's sign-off before proceeding (spec §6.4).**
- [ ] **Step 6: Runbook** `docs/runbooks/agentcore-image-check.md`: prerequisites (Docker, arm64 host or buildx+qemu), the one-liner, expected output, the AC interpretation note (app-side cold start; AWS microVM 2–5 s is platform-side), ECR push + CDK pointer to the crate README.
- [ ] **Step 7: green + clippy (`cargo clippy -p paigasus-helikon-runtime-agentcore --all-features --all-targets -- -D warnings`) → Commit** — `git commit -m "feat(runtime-agentcore): SMA-332 add mcp mode, examples, dockerfile, and image checks"`

---

### Task 15: Release engineering (ascend ×2, core/mcp/facade bumps)

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/Cargo.toml`, `crates/paigasus-helikon-runtime-agentcore/Cargo.toml` (→ `0.1.0`, drop `publish = false`)
- Modify: `release-plz.toml` (delete both crates' `[[package]]` blocks)
- Modify: `Cargo.toml` (workspace pins: both new crates → `0.1.0`; core + mcp + facade pins per below)
- Modify: `crates/paigasus-helikon-core/Cargo.toml` + `crates/paigasus-helikon-core/CHANGELOG.md` (patch bump — read the current version, bump patch)
- Modify: `crates/paigasus-helikon-mcp/Cargo.toml` + `CHANGELOG.md` (patch bump)
- Modify: `crates/paigasus-helikon/Cargo.toml` + `CHANGELOG.md` (patch bump + self-pin)
- Create: `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md`, `crates/paigasus-helikon-runtime-agentcore/CHANGELOG.md` (initial `## 0.1.0` sections, format copied from `crates/paigasus-helikon-runtime-tokio/CHANGELOG.md`)
- Modify: `crates/paigasus-helikon/src/lib.rs` (verify the two `pub use` re-exports carry `///` docs; the features already exist)

- [ ] **Step 1:** Read current versions from the three released crates' Cargo.tomls (do NOT trust remembered numbers), apply patch bumps + matching `[workspace.dependencies]` pins.
- [ ] **Step 2:** 4-step ascend on both new crates (version `0.1.0`, drop `publish = false`, drop `release-plz.toml` blocks, workspace pins `0.1.0`).
- [ ] **Step 3:** CHANGELOG entries (core: serde derives + `ModelTurnAccumulator` + tool pipeline + `authorize_tool`; mcp: `streamable_http_service_with`; facade: dependency refresh; new crates: initial release notes).
- [ ] **Step 4:** Verify: `cargo build --workspace --all-features` + `cargo package -p paigasus-helikon-runtime-temporal --no-verify` and same for `-agentcore` (manifest sanity; `--no-verify` because registry deps for same-PR core/mcp bumps don't exist yet — that's exactly what release-plz's dependency-ordered publish handles post-merge).
- [ ] **Step 5: Commit** — `git commit -m "chore(release): SMA-332 lift stage-1 gates for runtime-temporal and runtime-agentcore"`

---

### Task 16: Docs sweep + full CI parity

**Files:**
- Modify: `docs/book/src/introduction.md` (stub-roster paragraph: only `-evals` remains a stub), the installation/features page under `docs/book/src/getting-started/`, and the runtimes concept page under `docs/book/src/concepts/` (add Temporal + AgentCore sections: when to use, constraint sets, contract tables, CDK snippet pointer) — locate exact files via `grep -rn "runtime-tokio\|Runtimes" docs/book/src/`
- Modify: `crates/paigasus-helikon-runtime-agentcore/README.md` (replace stub: contract tables, quickstart, Dockerfile + verified CDK L2 snippet from the spec §2, size/cold-start numbers from Task 14)
- Modify: `crates/paigasus-helikon/README.md` (feature table rows for the two now-real features), root `README.md` (crate roster)
- Verify only (no blind edits): all other book pages mentioning stubs

- [ ] **Step 1:** Make the edits; `mdbook build docs/book` → zero warnings (linkcheck `warning-policy = "error"`).
- [ ] **Step 2: Full CI parity run** (fix all fallout before committing):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
cargo build -p paigasus-helikon-runtime-axum --no-default-features
mdbook build docs/book
```

- [ ] **Step 3: Commit** — `git commit -m "docs(book): SMA-332 document runtime-temporal and runtime-agentcore"` (split a second `fix(...)` commit if CI-parity fallout touched crate code).

---

## Self-review record

- **Spec coverage:** §5.2–5.13 → Tasks 4–10; §6.1–6.5 → Tasks 11–14; §7 → Tasks 4 (deps/deny), 15, 16; §8 ACs → Tasks 9 (crash-resume), 14 (size/cold-start). Core changes §5.3(serde)/§5.4(pipeline)/§5.6(accumulator) → Tasks 1/3/2. GATE 1 decisions all encoded (single PR; buffered run_streamed in Task 8; fail-fast registration in Task 7; app-side cold-start + model-backed size gate in Task 14 with STOP rule).
- **Placeholder scan:** none; two deliberate calibration points (temporalio 0.5 exact signatures — Task 4 gate; rmcp stateless field — Task 11 step 1) are explicit STOP/verify steps, not hand-waves.
- **Type consistency:** `ModelTurn`/`ModelTurnAccumulator` (T2→T7), `execute_tool_call` signature (T3→T7), payload names (T5→T6/T8), `AgentPlan` (T6→T7), `DriverEffect` (T6→T8), `streamable_http_service_with` (T11→T14) cross-checked.
