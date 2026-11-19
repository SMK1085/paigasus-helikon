# SMA-803 Deferred Tool Calls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A permission check can defer a tool call; the run then ends cleanly at the turn boundary with `StopReason::Deferred`, and a later run resumes the call with the approver's `ToolResolution`.

**Architecture:** All new types, the authorization change, the loop change, the session-log helpers (`open_deferrals`, `ResumePlan::from_events`) and the recorder changes live in `paigasus-helikon-core`. `TokioRunner` is the only runner that resumes; `TemporalRunner`, the five workflow agents and the three HTTP runtimes reject or disable deferral. A resume is a normal `Runner::run` call whose `AgentInput` carries resolutions.

**Tech Stack:** Rust 1.94 (workspace MSRV), `async-stream`, `serde`/`serde_json`, `thiserror`, `tracing`, `uuid` (workspace pin, feature `v4`), `insta` (snapshot tests), `tokio` (tests).

**Spec:** `docs/superpowers/specs/2026-11-18-sma-803-deferred-tool-calls-design.md` (Gate 1 approved 2026-11-19). §7 holds the decided open questions. §9 lists rejected findings: do NOT implement a local `project()` read in the runner, an args digest, all-`Denied` closing of a missing `ToolCall`, or a core `DeferringApprovalHandler`.

## Global Constraints

- Implementers work synchronously in the foreground; never background cargo runs; never run git commands that move HEAD or switch branches (no checkout, switch, reset, rebase, stash); stage files by explicit path, never `git add -A` or `git add .`.
- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-803-deferred-tool-calls`. Use absolute paths with that prefix for every Read/Edit/Write.
- Commit format: `<type>(<scope>): SMA-803 <lowercase message>`. Allowed scopes used in this plan (all in `.versionrc`): `core`, `runtime-tokio`, `runtime-temporal`, `runtime`, `docs`. End every commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Before each commit run `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings`. Both must be clean.
- Do NOT hand-bump any crate version and do NOT edit any `CHANGELOG.md`. Release is pure-auto release-plz.
- Compatibility: new variants and fields go only on `#[non_exhaustive]` types; no public signature changes. `parse_final` keeps `Result<RunResult<T>, serde_json::Error>`. `collect_typed` keeps `Result<RunResult<T>, AgentError>`. `TransitionInput::ToolResults { outcomes }` keeps its shape.
- Every new `pub` item gets a `///` doc (workspace `missing_docs`, 80% doc-coverage gate). A `pub` item's doc must not intra-doc-link to a `pub(crate)` or private item (rustdoc `private_intra_doc_links` fails the `-D warnings` docs gate); use prose.
- New tracing warnings use existing components with a free leaf: `paigasus::runtime_temporal::runner`, `paigasus::runtime_axum::context`, `paigasus::runtime_actix::context`, `paigasus::runtime_agentcore::invoke`. Do not add a new `paigasus::<component>` (the `tracing_target_docs` workspace lint compares components with the mdBook).
- New insta snapshots: run the test with `INSTA_UPDATE=always`, read each new `.snap` file, and stage it by explicit path.
- Exact texts (copy verbatim):
  - flag off: `approval deferred, but this run cannot defer: {prompt}`
  - denial result: `permission denied: {reason}`
  - renamed root / missing handoff target: `agent no longer available: {name}`
  - missing tool on re-check: reason `tool no longer available: {name}`
  - changed args on re-check: reason `arguments changed on re-check`
  - not-run synthetic result: `tool call did not run: the run ended before the resume completed`
  - cancel synthetic result (existing): `tool call did not complete (run cancelled/timed out)`
  - direct `agent.run` with resolutions: `resolutions require a deferral-aware runner`
  - workflow agents: `this agent cannot resume deferred calls`
  - unknown tool (existing): `unknown tool: {name}`
- Rollback note (verbatim, used in Task 14): "Once a host stores a `tool_deferred` event, it cannot roll back below this version, because an older reader cannot parse the row. Enable deferral only after every reader of the session store runs this version."

## Review Focus

1. **A repeated `call_id` inside one resumed run.** The resumed call `fc_0` gets its result in the resume block, and a new model turn calls `fc_0` again and is cancelled mid-tool. The new `ToolCalled` must still get the cancel synthetic result. Today's `drain` matches by a `HashSet` of call ids and would treat the old result as a match. Test: Task 7 `drain_matches_results_by_position_not_by_id`.
2. **A failed append on a resuming run.** Spec §4.6 step 4 requires `SessionWrite`, but §5 lists only the deferring-run case. Test: Task 9 `failed_append_on_resume_returns_session_write`.
3. **A resume with the flag off, and a chained deferral.** The resume must run with the flag off (§4.8). A resumed run with the flag on that defers again must store a second open deferral that a third run can resume. Test: Task 9 `resume_works_with_flag_off` and `chained_deferral_resumes_twice`.
4. **Mixed resolutions for two deferred calls of one turn.** One `Approved` and one `Denied`: both results land, in the original call order, before the new input. A resume that names only one of the two returns `UnresolvedDeferrals`. Test: Task 9 `mixed_resolutions_keep_call_order`.
5. **Secrets in every emitted copy.** A secret added by a `PreToolUse` `ReplaceInput` must not appear in `ApprovalRequested.args`, in `RunDeferred.calls[*].args`, or in `RunResult.stop_reason`. Only the stored `ToolDeferred` has it. Test: Task 5 `deferred_event_args_are_redacted` (asserts all three copies) and Task 9 `deferral_round_trip_stores_raw_args`.

---

## File map

| File | Responsibility | Tasks |
|---|---|---|
| `crates/paigasus-helikon-core/Cargo.toml` | add `uuid` dependency | 1 |
| `crates/paigasus-helikon-core/src/deferral.rs` (new) | `PendingToolCall`, `ToolResolution`, `StopReason`, `InvalidResume`, `DeferralSlot`, `open_deferrals`, `ResumePlan` | 1, 6, 7 |
| `crates/paigasus-helikon-core/src/lib.rs` | `pub mod deferral; pub use deferral::*;`, crate docs | 1, 14 |
| `crates/paigasus-helikon-core/src/permission.rs` | `PermissionDecision::Defer`, `ApprovalOutcome::Defer`, `ApprovalHandler` docs, test-only guard constructor | 1, 4 |
| `crates/paigasus-helikon-core/src/agent.rs` | `AgentInput` fields, `AgentEvent::RunDeferred`, `AgentError::Incomplete`, loop driver, resume entry | 2, 5, 7, 8a, 8b |
| `crates/paigasus-helikon-core/src/session.rs` | `SessionEvent::ToolDeferred`, `project()`, `SessionRecorder` | 2, 6, 7 |
| `crates/paigasus-helikon-core/src/runner.rs` | `RunResult.stop_reason`, `collect`, `collect_typed`, `parse_final`, `RunError` variants, `Runner` docs | 2, 9, 14 |
| `crates/paigasus-helikon-core/src/context.rs` | flag, handoff path, deferral slot, copy sites, authorization pipeline | 3, 4 |
| `crates/paigasus-helikon-core/src/control.rs` | `Interceptors::authorize_detailed`, `authorize_tests` | 4 |
| `crates/paigasus-helikon-core/src/tool_exec.rs` | `execute_tool_call` maps `Defer` to `Deny` | 1, 4 |
| `crates/paigasus-helikon-core/src/loop_state.rs` | `TransitionInput::ToolResultsWithDeferrals`, `NeedsApproval` terminal, `thread_handoff_transcript` visibility | 5, 7 |
| `crates/paigasus-helikon-core/src/resume_exec.rs` (new, crate-private) | execute resolved calls, re-check | 8a |
| `crates/paigasus-helikon-core/src/{workflow,graph,swarm}.rs` | reject resume input | 10 |
| `crates/paigasus-helikon-core/src/compacting_session.rs` | no compaction while a deferral is open | 13 |
| `crates/paigasus-helikon-runtime-tokio/src/lib.rs` | resume, strict finalize | 9 |
| `crates/paigasus-helikon-runtime-temporal/src/runner.rs` | reject resume, clear flag | 11 |
| `crates/paigasus-helikon-runtime-{axum,actix}/src/context.rs`, `.../handlers/runs.rs` | `restrict_unsupported_run_features` | 12 |
| `crates/paigasus-helikon-runtime-agentcore/src/{invoke,ws}.rs`, `src/agui/{sse,ws}.rs`, `src/a2a/rpc.rs` | `restrict_unsupported_run_features` | 12 |
| `docs/book/src/concepts/*.md` | mdBook | 14 |

Tests (new files): `crates/paigasus-helikon-core/tests/deferred_calls.rs` (Task 5), `crates/paigasus-helikon-core/tests/deferred_resume.rs` (Tasks 8a, 8b), `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs` (Task 9).

---

### Task 1: Core deferral types and the two new decision variants

**Files:**
- Modify: `crates/paigasus-helikon-core/Cargo.toml` (`[dependencies]`)
- Create: `crates/paigasus-helikon-core/src/deferral.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs:17-67` (module list and re-exports)
- Modify: `crates/paigasus-helikon-core/src/permission.rs:34-56` (`PermissionDecision`), `:264-279` (`ApprovalOutcome`)
- Modify: `crates/paigasus-helikon-core/src/context.rs:469-562` (`authorize_tool`, both `ApprovalOutcome` matches), `crates/paigasus-helikon-core/src/agent.rs:575-597` (match in `run_tools_concurrent`), `crates/paigasus-helikon-core/src/tool_exec.rs:72-88`
- Test: `crates/paigasus-helikon-core/tests/serde_roundtrip.rs`, unit tests in `deferral.rs`

**Interfaces:**
- Consumes: nothing.
- Produces (all `pub`, re-exported from the crate root):
  - `PendingToolCall` — `#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)] #[non_exhaustive]`, private fields `deferral_id: String`, `call_id: String`, `tool: String`, `args: serde_json::Value`, `prompt: String`, `handoff_path: Vec<String>`.
    - `pub fn new(deferral_id: impl Into<String>, call_id: impl Into<String>, tool: impl Into<String>, args: serde_json::Value, prompt: impl Into<String>, handoff_path: Vec<String>) -> Self`
    - accessors `deferral_id(&self) -> &str`, `call_id(&self) -> &str`, `tool(&self) -> &str`, `args(&self) -> &serde_json::Value`, `prompt(&self) -> &str`, `handoff_path(&self) -> &[String]`, `agent(&self) -> Option<&str>` (last path name).
    - `pub(crate) fn with_args(&self, args: serde_json::Value) -> Self` (used to make the redacted event copy).
  - `pub(crate) fn new_deferral_id() -> String` — `uuid::Uuid::new_v4().to_string()`.
  - `ToolResolution` — `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(tag = "type", rename_all = "snake_case")] #[non_exhaustive]`: `Approved`, `Denied { reason: String }`.
  - `StopReason` — same derives plus `Default`, `#[serde(tag = "type", rename_all = "snake_case")]`: `#[default] Completed`, `Deferred { calls: Vec<PendingToolCall> }`.
  - `InvalidResume` — `#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)] #[non_exhaustive]` with variants and `#[error]` texts:
    - `NoOpenDeferrals` — "the session has no open deferred tool calls"
    - `UnresolvedDeferrals { missing: Vec<String> }` — "open deferred tool calls have no resolution: {missing:?}"
    - `UnknownDeferral { deferral_id: String }` — "no open deferred tool call has the id {deferral_id}"
    - `DuplicateResolution { deferral_id: String }` — "two resolutions name the deferral {deferral_id}"
    - `SessionModified` — "the session changed after the deferred tool calls were recorded"
    - `CallNotInHistory { call_id: String }` — "the session history has no open tool call {call_id} for a deferral"
    - `ToolItemsInInput` — "the resume input contains tool call or tool result items"
    - `PathTooDeep { len: usize, max_agent_depth: u32 }` — "the handoff path has {len} agents, more than max_agent_depth + 1 (max_agent_depth = {max_agent_depth})"
    - `ResumeUnsupported` — "this runner cannot resume deferred tool calls"
  - `DeferralSlot` — `#[derive(Clone, Default)]` around `Arc<Mutex<Vec<PendingToolCall>>>`; manual `Debug` that prints only the count (the slot holds raw args). `pub fn new() -> Self`, `pub fn calls(&self) -> Vec<PendingToolCall>` (clone), `pub(crate) fn record(&self, calls: Vec<PendingToolCall>)` (append).
  - `PermissionDecision::Defer { prompt: String }` (doc: "Defer the call to a later run; see `RunContext::with_deferred_tool_calls`").
  - `ApprovalOutcome::Defer` (doc: "Defer the call to a later run").

- [ ] **Step 1: Write the failing tests**

In `crates/paigasus-helikon-core/tests/serde_roundtrip.rs` add (use the existing `roundtrip` helper):

```rust
#[test]
fn stop_reason_completed_roundtrip() {
    insta::assert_snapshot!(roundtrip(&StopReason::Completed));
}

#[test]
fn stop_reason_deferred_roundtrip() {
    let call = PendingToolCall::new(
        "6f1c1d3e-0000-4000-8000-000000000001", "call_1", "pay",
        serde_json::json!({ "amount": 10 }), "Pay 10?", vec!["triage".into(), "budgeting".into()],
    );
    insta::assert_snapshot!(roundtrip(&StopReason::Deferred { calls: vec![call] }));
}

#[test]
fn tool_resolution_approved_roundtrip() {
    insta::assert_snapshot!(roundtrip(&ToolResolution::Approved));
}

#[test]
fn tool_resolution_denied_roundtrip() {
    insta::assert_snapshot!(roundtrip(&ToolResolution::Denied { reason: "no".into() }));
}
```

In `deferral.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn pending_tool_call_agent_is_last_path_name() {
    let c = PendingToolCall::new("d", "c", "t", serde_json::json!({}), "p", vec!["a".into(), "b".into()]);
    assert_eq!(c.agent(), Some("b"));
    let empty = PendingToolCall::new("d", "c", "t", serde_json::json!({}), "p", vec![]);
    assert_eq!(empty.agent(), None);
}

#[test]
fn stop_reason_default_is_completed() {
    assert_eq!(StopReason::default(), StopReason::Completed);
}

#[test]
fn new_deferral_id_is_a_v4_uuid() {
    let id = new_deferral_id();
    assert_eq!(uuid::Uuid::parse_str(&id).unwrap().get_version_num(), 4);
    assert_ne!(id, new_deferral_id());
}

#[test]
fn deferral_slot_shares_records_across_clones() {
    let a = DeferralSlot::new();
    let b = a.clone();
    b.record(vec![PendingToolCall::new("d", "c", "t", serde_json::json!({}), "p", vec![])]);
    assert_eq!(a.calls().len(), 1);
    assert!(!format!("{a:?}").contains("\"t\""));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --test serde_roundtrip stop_reason`
Expected: FAIL to compile ("cannot find type `StopReason`").

- [ ] **Step 3: Implement**

Add `uuid = { workspace = true }` to core `[dependencies]` (the workspace pin already has `default-features = false, features = ["v4"]`). Create `deferral.rs` with the types above, add `pub mod deferral;` and `pub use deferral::*;` to `lib.rs`. Add the two variants. Keep every existing match compiling with fail-closed arms until Task 4:
- `context.rs` both `ApprovalOutcome` matches: `ApprovalOutcome::Defer` → `PermissionDecision::Deny { reason: format!("approval deferred, but this run cannot defer: {prompt}") }`.
- `agent.rs` `run_tools_concurrent` and `tool_exec.rs`: add `PermissionDecision::Defer { prompt }` to the deny arm with reason `format!("approval deferred, but this run cannot defer: {prompt}")`.
- A policy that returns `PermissionDecision::Defer` falls through `other => other` in `authorize_tool` for now; the loop maps it to deny as above.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `INSTA_UPDATE=always cargo test -p paigasus-helikon-core --test serde_roundtrip && cargo test -p paigasus-helikon-core --lib deferral`
Expected: PASS. Four new `.snap` files under `crates/paigasus-helikon-core/tests/snapshots/`. Read them: `{"type": "completed"}`, `{"type": "deferred", "calls": [...]}`, `{"type": "approved"}`, `{"type": "denied", "reason": "no"}`.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/Cargo.toml Cargo.lock crates/paigasus-helikon-core/src/deferral.rs crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/src/permission.rs crates/paigasus-helikon-core/src/context.rs crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/src/tool_exec.rs crates/paigasus-helikon-core/tests/serde_roundtrip.rs crates/paigasus-helikon-core/tests/snapshots/serde_roundtrip__stop_reason_completed_roundtrip.snap crates/paigasus-helikon-core/tests/snapshots/serde_roundtrip__stop_reason_deferred_roundtrip.snap crates/paigasus-helikon-core/tests/snapshots/serde_roundtrip__tool_resolution_approved_roundtrip.snap crates/paigasus-helikon-core/tests/snapshots/serde_roundtrip__tool_resolution_denied_roundtrip.snap
git commit -m "feat(core): SMA-803 add the deferral types and the defer decision variants"
```

---

### Task 2: Event, session and result surface

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs:85-109` (`AgentInput`), `:346-487` (`AgentEvent` + doc "Seventeen variants"), `:489-500` (`is_terminal` + doc "the two events"), `:1156-1240` (`AgentError`), `:1466-1602` (`terminal_tests`)
- Modify: `crates/paigasus-helikon-core/src/session.rs:66-144` (`SessionEvent`), `:146-205` (constructors), `:207-230` (`kind`, `ts`), `:313-402` (`project`), `:797-858` (`kind_matches_serde_tag_for_every_variant`)
- Modify: `crates/paigasus-helikon-core/src/runner.rs:230-261` (`RunResult`, `parse_final`), `:310-353` (`collect`), `:367-432` (`collect_typed`), `:472-497` (`RunError`)
- Test: `crates/paigasus-helikon-core/tests/serde_roundtrip.rs`, `crates/paigasus-helikon-core/tests/session_projection.rs`, `crates/paigasus-helikon-core/tests/collect_typed.rs`, unit tests in `runner.rs`

**Interfaces:**
- Consumes: Task 1 `PendingToolCall`, `StopReason`, `ToolResolution`, `InvalidResume`.
- Produces:
  - `AgentEvent::RunDeferred { calls: Vec<PendingToolCall>, usage: TokenUsage }` (serde tag `run_deferred`, placed in the `// --- Terminal ---` group). `is_terminal()` returns `true` for `RunCompleted | RunFailed | RunDeferred`.
  - `AgentEvent::ApprovalRequested.args` doc: "Effective JSON arguments (after `PreToolUse` and any policy `Replace`), redacted with the run's secret set when output redaction is on."
  - `SessionEvent::ToolDeferred { deferral_id: String, call_id: String, name: String, args: serde_json::Value, prompt: String, handoff_path: Vec<String>, ts: Timestamp }`; `kind()` → `"tool_deferred"`; `ts()` arm; `project()` adds no item (contribution `0`).
  - `SessionEvent::tool_deferred(call: &PendingToolCall) -> Self` (`ts = Timestamp::now()`).
  - `AgentInput` private field `resolutions: Vec<(String, ToolResolution)>`; `pub fn with_resolution(mut self, deferral_id: impl Into<String>, resolution: ToolResolution) -> Self`; `pub fn resolutions(&self) -> &[(String, ToolResolution)]`. Replace in-crate struct literals (`from_user_text`, `agent.rs` handoff `AgentInput { messages: transcript }`, `graph.rs:258`) with `AgentInput { messages, ..Default::default() }`.
  - `RunResult<T>` gets `pub stop_reason: StopReason`. `parse_final` copies it.
  - `AgentError::Incomplete { stop_reason: StopReason }`, `#[error("run did not complete")]`.
  - `RunError::InvalidResume(#[from] InvalidResume)` with `#[error("invalid resume: {0}")]`; `RunError::SessionWrite(#[source] SessionError)` with `#[error("session write failed: {0}")]`.

- [ ] **Step 1: Write the failing tests**

`terminal_tests` in `agent.rs`: add `AgentEvent::RunDeferred { .. } => true` to `classify`; add one `RunDeferred` instance to `every_variant()`; change the expected count in `every_variant_covers_the_whole_enum` to `18`; rename `exactly_two_variants_are_terminal` to `exactly_three_variants_are_terminal` with expected `3` and message `"expected exactly RunCompleted + RunFailed + RunDeferred"`.

`serde_roundtrip.rs`:

```rust
#[test]
fn agent_event_run_deferred_roundtrip() {
    let mut usage = TokenUsage::default();
    usage.input_tokens = 7;
    let call = PendingToolCall::new("6f1c1d3e-0000-4000-8000-000000000001", "call_1", "pay",
        serde_json::json!({ "amount": 10 }), "Pay 10?", vec!["triage".into()]);
    insta::assert_snapshot!(roundtrip(&AgentEvent::RunDeferred { calls: vec![call], usage }));
}

#[test]
fn session_event_tool_deferred_roundtrip() {
    let ev = SessionEvent::ToolDeferred {
        deferral_id: "6f1c1d3e-0000-4000-8000-000000000001".into(), call_id: "call_1".into(),
        name: "pay".into(), args: serde_json::json!({ "amount": 10 }), prompt: "Pay 10?".into(),
        handoff_path: vec!["triage".into()], ts: pinned_ts(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}
```

`session_projection.rs`:

```rust
#[test]
fn tool_deferred_produces_no_message_and_compaction_math_holds() {
    // user, tool_called c1, tool_deferred c1 → 2 messages (user, ToolCall); the deferral adds none.
    // Then compacted(n = 3) removes exactly those 2 messages and pushes 1 System.
}
```

Assert `project(&evs).messages.len() == 2` before the compaction and `== 1` (a `System`) after.

`session.rs` `kind_matches_serde_tag_for_every_variant`: add a `ToolDeferred` case with `"tool_deferred"`.

`runner.rs` unit tests (`mod tests`):

```rust
#[tokio::test]
async fn collect_maps_run_deferred_to_ok_with_stop_reason() {
    // stream: MessageOutput("asking"), RunDeferred { calls: [c], usage: input_tokens 5 }
    // assert: Ok; final_output == "asking"; usage.input_tokens == 5;
    //         stop_reason == StopReason::Deferred { calls: vec![c] }
}

#[tokio::test]
async fn collect_without_terminal_is_completed() {
    // stream: MessageOutput("hi") only → Ok, stop_reason == StopReason::Completed
}

#[test]
fn parse_final_copies_stop_reason() {
    // RunResult { final_output: "1", stop_reason: Deferred{..}, ..Default::default() }
    // .parse_final::<u32>() → Ok, stop_reason preserved
}

#[test]
fn run_error_new_variants_render() {
    assert_eq!(RunError::from(InvalidResume::NoOpenDeferrals).to_string(),
        "invalid resume: the session has no open deferred tool calls");
    assert!(RunError::SessionWrite(SessionError::Unavailable).to_string()
        .starts_with("session write failed: "));
}
```

`collect_typed.rs`:

```rust
#[tokio::test]
async fn collect_typed_returns_incomplete_on_run_deferred() {
    // stream ends with RunDeferred { calls: [c], usage }
    // assert matches Err(AgentError::Incomplete { stop_reason: StopReason::Deferred { .. } })
}
```

`agent.rs` unit test:

```rust
#[test]
fn with_resolution_accumulates_in_order() {
    let i = AgentInput::new()
        .with_resolution("a", ToolResolution::Approved)
        .with_resolution("b", ToolResolution::Denied { reason: "no".into() });
    assert_eq!(i.resolutions().len(), 2);
    assert_eq!(i.resolutions()[0].0, "a");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --lib terminal_tests`
Expected: FAIL to compile (`RunDeferred` not found).

- [ ] **Step 3: Implement the variants and fields listed under Interfaces**

Update the `AgentEvent` type doc from "Seventeen variants" to "Eighteen variants", and the `is_terminal` doc to name the three terminal events. In `collect()`: `RunDeferred { calls, usage: u }` sets `usage = *u` and `stop_reason = StopReason::Deferred { calls: calls.clone() }`. In `collect_typed()`: same capture; after the failure checks and before the JSON parse, return `Err(AgentError::Incomplete { stop_reason })` when `stop_reason != StopReason::Completed`. Doc `parse_final`: "Check `stop_reason` first: a deferred run has no final output to parse."

- [ ] **Step 4: Run the tests to verify they pass**

Run: `INSTA_UPDATE=always cargo test -p paigasus-helikon-core` (whole crate, so nothing else broke)
Expected: PASS. Two new `.snap` files; read them (tags `run_deferred` and `tool_deferred`).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/src/session.rs crates/paigasus-helikon-core/src/runner.rs crates/paigasus-helikon-core/src/graph.rs crates/paigasus-helikon-core/tests/serde_roundtrip.rs crates/paigasus-helikon-core/tests/session_projection.rs crates/paigasus-helikon-core/tests/collect_typed.rs crates/paigasus-helikon-core/tests/snapshots/serde_roundtrip__agent_event_run_deferred_roundtrip.snap crates/paigasus-helikon-core/tests/snapshots/serde_roundtrip__session_event_tool_deferred_roundtrip.snap
git commit -m "feat(core): SMA-803 add the run-deferred event, the tool-deferred session event and the stop reason"
```

---

### Task 3: RunContext flag, handoff path and deferral slot

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs:50-102` (fields), `:107-131` (`new`), `:291-313` (`handoff_child`), `:320-342` (`subagent_child`), getters near `:255-275`
- Test: `crates/paigasus-helikon-core/src/context.rs` `runcontext_tests`

**Interfaces:**
- Consumes: Task 1 `DeferralSlot`.
- Produces (on `RunContext<Ctx>`):
  - fields `deferred_tool_calls: bool` (default `false`), `handoff_path: Vec<String>` (default empty), `deferrals: DeferralSlot` (default fresh).
  - `pub fn with_deferred_tool_calls(mut self, enabled: bool) -> Self`
  - `pub fn deferred_tool_calls_enabled(&self) -> bool`
  - `pub fn handoff_path(&self) -> &[String]` — "agent names from the root agent to the current agent"
  - `pub(crate) fn push_handoff_path(&mut self, agent: impl Into<String>)`
  - `pub fn deferral_handle(&self) -> DeferralSlot` (clone of the shared slot; doc: runner plumbing that gives the session recorder the raw effective args).
- Copy-site rules: `handoff_child()` copies the flag and the path, and shares the slot (clone). `subagent_child()` sets the flag `false`, the path empty, and a fresh slot. `to_tool_context()` and `clone_permission_fields()` do not project any of the three. `AgentAsTool` uses `RunContext::new`, so all three are defaults there (regression test in Task 5).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn deferred_tool_calls_default_off_and_round_trips() {
    let c: RunContext<()> = RunContext::ephemeral(());
    assert!(!c.deferred_tool_calls_enabled());
    assert!(c.with_deferred_tool_calls(true).deferred_tool_calls_enabled());
}

#[test]
fn handoff_child_keeps_flag_and_path_and_shares_deferral_slot() {
    let mut p: RunContext<()> = RunContext::ephemeral(()).with_deferred_tool_calls(true);
    p.push_handoff_path("root");
    let c = p.handoff_child();
    assert!(c.deferred_tool_calls_enabled());
    assert_eq!(c.handoff_path(), &["root".to_owned()]);
    c.deferral_handle().record(vec![/* one PendingToolCall */]);
    assert_eq!(p.deferral_handle().calls().len(), 1);
}

#[test]
fn subagent_child_clears_flag_path_and_slot() {
    let mut p: RunContext<()> = RunContext::ephemeral(()).with_deferred_tool_calls(true);
    p.push_handoff_path("root");
    let c = p.subagent_child();
    assert!(!c.deferred_tool_calls_enabled());
    assert!(c.handoff_path().is_empty());
    c.deferral_handle().record(vec![/* one PendingToolCall */]);
    assert!(p.deferral_handle().calls().is_empty());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --lib runcontext_tests`
Expected: FAIL to compile (`with_deferred_tool_calls` not found).

- [ ] **Step 3: Implement the fields and methods listed under Interfaces**

Doc `with_deferred_tool_calls` with three facts from the spec: (a) a direct `agent.run(ctx, input)` with the flag on can end with `RunDeferred`, and only a deferral-aware runner can resume it; (b) a `PreToolUse` hook that adds a secret to the args of a call that can defer makes the secret persist in the session store; (c) runtimes without deferral support clear the flag.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core --lib runcontext_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/context.rs
git commit -m "feat(core): SMA-803 add the deferral opt-in flag and the handoff path to the run context"
```

---

### Task 4: Authorization pipeline with Defer and the re-check mode

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs:461-562` (`authorize_tool` → wrapper over a new crate-private function)
- Modify: `crates/paigasus-helikon-core/src/control.rs:110-125` (`Interceptors`), `:276-586` (`authorize_tests`)
- Modify: `crates/paigasus-helikon-core/src/permission.rs:255-262` (`ApprovalHandler` docs), `:48-54` (`Replace` doc), `GuardRule` impl (test-only constructor)
- Modify: `crates/paigasus-helikon-core/src/tool_exec.rs:72-88`

**Interfaces:**
- Consumes: Task 1 variants, Task 3 `deferred_tool_calls_enabled()`.
- Produces (crate-private, in `context.rs`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AskPoints {
    /// Normal authorization: each ask point calls the approval handler.
    Ask,
    /// Re-check of an approved deferred call: every ask point counts as answered.
    Answered,
}

#[derive(Debug, Clone)]
pub(crate) enum Authorization {
    Allow,
    Replace { args: serde_json::Value },
    Deny { reason: String },
    /// Only with `AskPoints::Ask` and the flag on. `args` are the effective args
    /// (the policy's replaced args when a `Replace` met the deferred mark).
    Defer { prompt: String, args: serde_json::Value },
}

impl<Ctx> RunContext<Ctx> {
    pub(crate) async fn authorize_tool_detailed(
        &self, tool: &str, effect: ToolEffect, args: &serde_json::Value, asks: AskPoints,
    ) -> Authorization;
}
```

- `pub async fn authorize_tool(..) -> PermissionDecision` stays; it calls `authorize_tool_detailed(.., AskPoints::Ask)` and maps `Allow→Allow`, `Replace{args}→Replace{args}`, `Deny→Deny`, `Defer{prompt, ..}→Defer{prompt}`.
- `Interceptors::authorize` keeps its `PermissionDecision` return. Add `pub(crate) async fn authorize_detailed(&self, tool: &str, effect: ToolEffect, args: &serde_json::Value, asks: AskPoints) -> Authorization`.
- `#[cfg(test)] pub(crate) fn GuardRule::dotpath_with_action(action: GuardAction) -> GuardRule` (matcher `ProtectedDotPathWrite`), so `control.rs` tests can build a guard `Deny`.

**Pipeline rules** (spec §4.3; keep stage order deny rules › guard rules › allow rules › mode › policy › approval handler):

1. Keep `mark: bool` and `prompts: Vec<String>`. Each guard `Ask` and a policy `AskUser` call the handler (with `AskPoints::Ask`), also after the mark is set.
2. Handler `Allow` clears that ask point. Handler `Deny { reason }` returns `Deny` at once. Handler `Defer` sets the mark and pushes the prompt.
3. Policy `Defer { prompt }` sets the mark and pushes the prompt (no handler needed). Guard `Ask` without a handler stays `Deny { "destructive command requires approval: {prompt}" }`; `AskUser` without a handler stays `Deny { "no approval handler installed" }`.
4. Any `Deny` wins (deny rule, later guard `Deny`, `Plan` on a non-read-only tool, `DontAsk`, policy `Deny`, handler `Deny`).
5. With the mark set, every exit that would return `Allow` or `Replace` returns `Defer` (allow-rule short-circuit, `Bypass`, `AcceptEdits` on `Write`, no policy, policy `Allow`).
6. Policy `Replace { args }` with the mark set returns `Defer` with those replaced args.
7. `prompt` = distinct prompts in pipeline order, joined with `"\n"`.
8. With `AskPoints::Answered`: guard `Ask`, policy `AskUser` and policy `Defer` count as answered; the handler is never called; the mark is never set; the result is never `Defer`.
9. At the end, a `Defer` with `deferred_tool_calls_enabled() == false` becomes `Deny { reason: format!("approval deferred, but this run cannot defer: {prompt}") }`.

`tool_exec.rs`: `execute_tool_call` maps `PermissionDecision::Defer { prompt }` to the deny arm with reason `approval deferred, but this run cannot defer: {prompt}` (always, regardless of the flag).

- [ ] **Step 1: Write the failing tests** in `control.rs` `authorize_tests`

Add test handlers: `DeferHandler` (always `Defer`), `CountingHandler` (an `AtomicUsize` count + a scripted `Vec<ApprovalOutcome>` answered in call order), `PanicHandler` (panics on `decide`). Add policies: `DeferPolicy` (returns `Defer { prompt: "policy says wait" }`), `ReplacePolicy(Value)`. Use `ctx().with_deferred_tool_calls(true)` unless a test says otherwise. Call `c.authorize_tool_detailed(tool, effect, &args, AskPoints::Ask)`.

Use these args: `rm_root = json!({ "command": "rm -rf /" })` (one builtin guard `Ask`), `rm_root_etc = json!({ "command": "rm -rf / > /etc/passwd" })` (two builtin guard `Ask`s with distinct prompts), `env_write = json!({ "command": "rm -rf / > .env" })`.

```rust
#[tokio::test] async fn policy_defer_defers() // Default mode, DeferPolicy → Defer { prompt: "policy says wait", .. }
#[tokio::test] async fn handler_defer_on_guard_ask_defers() // rm_root, DeferHandler → Defer
#[tokio::test] async fn handler_defer_on_ask_user_defers() // AskPolicy + DeferHandler → Defer
#[tokio::test] async fn two_guard_asks_call_the_handler_twice_and_defer_then_deny_denies()
    // rm_root_etc, CountingHandler [Defer, Deny{"no"}] → Deny { reason: "no" }; count == 2
#[tokio::test] async fn two_guard_asks_both_deferred_join_prompts()
    // rm_root_etc, DeferHandler → Defer { prompt } where prompt ==
    // "destructive command: recursive force-remove of / or ~\nwrite to a protected system path"
#[tokio::test] async fn defer_then_later_guard_deny_denies()
    // env_write; with_guard_rules(vec![GuardRule::dotpath_with_action(GuardAction::Deny { reason: "never".into() })]); DeferHandler → Deny { reason: "never" }
#[tokio::test] async fn defer_then_plan_on_write_tool_denies()   // Plan, ToolEffect::Write, rm_root, DeferHandler → Deny
#[tokio::test] async fn defer_then_dont_ask_denies()              // DontAsk, rm_root, DeferHandler → Deny
#[tokio::test] async fn deferred_guard_beats_allow_rule()         // AllowRule::tool("Bash") → Defer
#[tokio::test] async fn deferred_guard_beats_bypass()             // Bypass → Defer
#[tokio::test] async fn deferred_guard_beats_accept_edits_on_write() // AcceptEdits, ToolEffect::Write → Defer
#[tokio::test] async fn deferred_guard_with_no_policy_defers()    // no policy → Defer
#[tokio::test] async fn deferred_guard_beats_policy_allow()       // AllowPolicy → Defer
#[tokio::test] async fn deferred_guard_with_policy_replace_defers_with_replaced_args()
    // ReplacePolicy(json!({"command":"echo safe"})) → Defer { args == json!({"command":"echo safe"}), .. }
#[tokio::test] async fn flag_off_maps_defer_to_deny()
    // ctx() without the flag, DeferPolicy → Deny { reason: "approval deferred, but this run cannot defer: policy says wait" }
#[tokio::test] async fn public_authorize_tool_returns_defer_variant()
    // c.authorize_tool(..) with DeferPolicy → PermissionDecision::Defer { prompt: "policy says wait" }
```

Re-check tests (same module, `AskPoints::Answered`, `PanicHandler` installed so a handler call fails the test):

```rust
#[tokio::test] async fn recheck_counts_guard_ask_as_answered()       // rm_root → Allow
#[tokio::test] async fn recheck_counts_ask_user_as_answered()        // AskPolicy, no handler → Allow
#[tokio::test] async fn recheck_counts_policy_defer_as_answered()    // DeferPolicy, flag off → Allow
#[tokio::test] async fn recheck_runs_deny_rules()                    // DenyRule::tool("Bash") → Deny
#[tokio::test] async fn recheck_runs_guard_deny()                    // env_write + dotpath Deny guard → Deny
#[tokio::test] async fn recheck_runs_mode()                          // Plan + ToolEffect::Write → Deny
#[tokio::test] async fn recheck_runs_policy_deny()                   // DenyPolicy → Deny
#[tokio::test] async fn recheck_allow_rule_short_circuits()          // AllowRule::tool("Bash") + DenyPolicy → Allow
#[tokio::test] async fn recheck_policy_replace_is_returned_for_comparison()
    // ReplacePolicy(x) → Replace { args: x } (Task 8a compares it with the recorded args)
```

Add a `tool_exec` unit test (in `crates/paigasus-helikon-core/tests/tool_exec.rs`): a ctx with the flag on and `DeferPolicy` → `execute_tool_call` returns `Err("permission denied: approval deferred, but this run cannot defer: policy says wait")` and a `PermissionDenied` event, and the tool is not invoked.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --lib authorize_tests`
Expected: FAIL to compile (`AskPoints` not found).

- [ ] **Step 3: Implement `authorize_tool_detailed` per the rules above, rewrite `authorize_tool` as the wrapper, add `Interceptors::authorize_detailed`, update `tool_exec.rs`**

Docs: on `ApprovalHandler::decide` — "One tool call can produce more than one `decide` call (one per ask point). A handler that returns `ApprovalOutcome::Defer` gives a deny in every context where deferred tool calls are off; the handler cannot see that flag." On `PermissionDecision::Replace` — "Must be idempotent: a deferred call is re-checked on resume, and a `Replace` with different args then denies it." Remove the Task 1 temporary arms in `context.rs` (the new function replaces them).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core --lib authorize_tests && cargo test -p paigasus-helikon-core --test tool_exec --test permissions`
Expected: PASS (old and new tests).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/context.rs crates/paigasus-helikon-core/src/control.rs crates/paigasus-helikon-core/src/permission.rs crates/paigasus-helikon-core/src/tool_exec.rs crates/paigasus-helikon-core/tests/tool_exec.rs
git commit -m "feat(core): SMA-803 let the permission pipeline defer a call and re-check an approval"
```

---

### Task 5: The loop ends a turn that has deferred calls

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs:521-665` (`run_tools_concurrent`), `:681-1150` (`LlmAgent::run`: run span fields, event loop arm, `ExecuteTools` arm, handoff re-yield)
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs:56-63` (`NeedsApproval` doc), `:134-157` (`TransitionInput`), `:404-455` (new arm before the `ToolResults` arm), `:589-591` (remove the `NeedsApproval` `not_implemented` arm)
- Create: `crates/paigasus-helikon-core/tests/deferred_calls.rs`
- Modify: `crates/paigasus-helikon-core/tests/transition_unit.rs:246-264`, `crates/paigasus-helikon-core/tests/subagent_propagation.rs`

**Interfaces:**
- Consumes: Task 4 `Interceptors::authorize_detailed`, `Authorization`, `AskPoints`; Task 3 `handoff_path()`, `push_handoff_path`, `deferral_handle()`; Task 1 `PendingToolCall::new`, `with_args`, `new_deferral_id`, `DeferralSlot::record`.
- Produces:
  - `TransitionInput::ToolResultsWithDeferrals { outcomes: Vec<ToolCallOutcome>, deferred: Vec<PendingToolCall> }` (doc: "`deferred` are the event copies, already redacted").
  - crate-private `enum ConcurrentOutcome { Done(ToolCallOutcome), Deferred(PendingToolCall) }`; `run_tools_concurrent` returns `(Vec<ConcurrentOutcome>, Vec<AgentEvent>)` in call order.
  - `LoopState::NeedsApproval { pending }` is a terminal state (doc update; `pending` stays `Vec<ToolCallRequest>`).

**Loop rules:**
- `LlmAgent::run` calls `ctx.push_handoff_path(agent_name)` once, before the stream starts (needs `let mut ctx = ctx;`).
- `run_tools_concurrent` uses `authorize_detailed(.., AskPoints::Ask)`. `Defer { prompt, args }` with a known tool → `Deferred(PendingToolCall::new(new_deferral_id(), call_id, name, args, prompt, ctx.handoff_path().to_vec()))`, tool not invoked. `Defer` with an unknown tool → `Done(Err("unknown tool: {name}"))` (spec rule 8; no `PermissionDenied`).
- `ExecuteTools` arm: push a `ToolResult` into `conversation` only for `Done` outcomes. When at least one call deferred: `ctx.deferral_handle().record(raw_pendings)`; build event copies with `p.with_args(redact(p.args(), &secrets))` when `ctx.redact_output()` (else unchanged); `tx_input = ToolResultsWithDeferrals { outcomes, deferred: event_copies }`. Else `ToolResults { outcomes }` as today.
- New transition arm `(ExecutingTools { calls, usage: prior, .. }, ToolResultsWithDeferrals { outcomes, deferred })`, placed before any max-turns check: events = one `ToolOutputItem` per outcome (as the `ToolResults` arm), then one `ApprovalRequested { call_id, tool, args }` per deferred, then `RunDeferred { calls: deferred, usage: *prior }`. `next_state = NeedsApproval { pending }` where `pending` = the `calls` entries whose `call_id` is in `deferred`; `next_action = Terminate`. Remove the `(NeedsApproval, _) => not_implemented("approval")` arm (the catch-all then fails any input on `NeedsApproval`).
- Driver event loop: a `RunDeferred { calls, usage }` arm records `gen_ai.usage.input_tokens`/`output_tokens` like `RunCompleted` and records the new run-span field `paigasus.deferred_calls` (declare it `tracing::field::Empty`) = `calls.len() as i64`.
- `Terminate` still fires `OnRunComplete` once. Output guardrails do not run (they run only on `Done`).
- Handoff re-yield (`agent.rs:1126-1137`): add an explicit `RunDeferred { calls, usage }` arm that yields `RunDeferred { calls, usage: parent_usage + usage }` and records the span usage, like the `RunCompleted` arm.

- [ ] **Step 1: Write the failing tests**

`crates/paigasus-helikon-core/tests/deferred_calls.rs` (use `tests/common` `MockModel`, `MockTool`; a `DeferPolicy` that defers only tool `"b"`; a `RecordingModel` that counts invocations):

```rust
#[tokio::test]
async fn turn_with_allowed_and_deferred_call_ends_with_run_deferred()
// model turn 0: ToolCall a(call_a), ToolCall b(call_b), ToolCall c(call_c); ctx flag on.
// Assert the tail of the event kinds is exactly:
//   ToolOutputItem(call_a), ToolOutputItem(call_c), ApprovalRequested(call_b), RunDeferred
// Assert RunDeferred.calls.len() == 1, calls[0].call_id() == "call_b", tool() == "b",
//   prompt() == "policy says wait", handoff_path() == ["root"], deferral_id parses as a UUID.
// Assert the model was invoked exactly once, tool b was invoked 0 times, a and c once each.
// Assert ctx.deferral_handle().calls() == RunDeferred.calls (no secrets here).
// Assert RunResultStreaming::collect() → Ok with stop_reason == Deferred { calls }.

#[tokio::test]
async fn permission_denied_events_come_first()
// add tool d denied by a DenyRule in the same turn; assert PermissionDenied(d) precedes
// every ToolOutputItem.

#[tokio::test]
async fn deferral_on_the_last_allowed_turn_ends_with_run_deferred()
// RunConfig max_turns = 1; turn 0 calls b → last event RunDeferred, no RunFailed.

#[tokio::test]
async fn deferred_event_args_are_redacted()
// ctx.with_extra_secrets(vec!["sk-test-SECRET-1234".into()]); an agent-level hook returns
// ReplaceInput { value: json!({"token": "sk-test-SECRET-1234"}) } on PreToolUse for b.
// Assert: ApprovalRequested.args, RunDeferred.calls[0].args() and collect().stop_reason
//   do not contain "sk-test-SECRET-1234"; ctx.deferral_handle().calls()[0].args()
//   == json!({"token": "sk-test-SECRET-1234"}).

#[tokio::test]
async fn unknown_tool_is_not_deferred()
// model calls "ghost" (no such tool), DeferPolicy defers everything → ToolOutputItem with
// "unknown tool: ghost", the run continues to turn 1 (script a final text), RunCompleted.

#[tokio::test]
async fn flag_off_turns_defer_into_permission_denied()
// same as the first test without the flag → PermissionDenied(b) with reason starting
// "approval deferred, but this run cannot defer: ", run continues, RunCompleted.

#[tokio::test]
async fn on_run_complete_fires_once_on_deferral()
// counting hook on OnRunComplete → 1.

#[tokio::test]
async fn deferral_in_handoff_child_carries_the_path_and_summed_usage()
// root "triage" hands off to "budgeting" (transfer turn with usage input 3), budgeting calls b
// (usage input 4) → RunDeferred.calls[0].handoff_path() == ["triage", "budgeting"],
// RunDeferred.usage.input_tokens == 7.
```

`transition_unit.rs`: replace `needs_approval_surfaces_not_implemented` with `needs_approval_is_terminal` — `transition(&NeedsApproval { pending: vec![] }, Start { .. })` gives `next_state` `LoopState::Failed(AgentError::Other(_))` and `NextAction::Terminate`. Add `tool_results_with_deferrals_emits_approval_then_run_deferred` — from `ExecutingTools { calls: [a, b], turn: 15, .. }` with `max_turns = 16`, one outcome for `a` and one deferred `b`: events are `ToolOutputItem(a)`, `ApprovalRequested(b)`, `RunDeferred`; state `NeedsApproval { pending }` with one entry `b`; action `Terminate`.

`subagent_propagation.rs`:

```rust
#[tokio::test]
async fn deferral_is_denied_inside_agent_as_tool_sub_run()
// parent ctx flag on + DeferPolicy; the parent calls an AgentAsTool whose inner agent calls b.
// Inner b result text starts "permission denied: approval deferred, but this run cannot defer: ";
// the parent run ends with RunCompleted, never RunDeferred.

#[tokio::test]
async fn deferral_is_denied_inside_a_workflow_child()
// SequentialAgent with one LlmAgent child that calls b; parent ctx flag on + DeferPolicy →
// PermissionDenied for b in the stream; the run ends with RunCompleted.
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --test deferred_calls`
Expected: FAIL (the last event is `RunFailed`/`RunCompleted`, not `RunDeferred`).

- [ ] **Step 3: Implement the loop rules above**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (all core tests; the handoff, hooks and loop tests must stay green).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/src/loop_state.rs crates/paigasus-helikon-core/tests/deferred_calls.rs crates/paigasus-helikon-core/tests/transition_unit.rs crates/paigasus-helikon-core/tests/subagent_propagation.rs
git commit -m "feat(core): SMA-803 end the run at the turn boundary when a call is deferred"
```

---

### Task 6: Record a deferral and find open deferrals

**Files:**
- Modify: `crates/paigasus-helikon-core/src/session.rs:412-546` (`SessionRecorder`), recorder tests `:597+`
- Modify: `crates/paigasus-helikon-core/src/deferral.rs` (add `open_deferrals`)
- Test: `crates/paigasus-helikon-core/tests/session_projection.rs`

**Interfaces:**
- Consumes: Task 1 `DeferralSlot`, Task 2 `SessionEvent::ToolDeferred`, `tool_deferred`.
- Produces:
  - `pub fn open_deferrals(events: &[SessionEvent]) -> Vec<PendingToolCall>` — every `ToolDeferred` at index `i` with no `ToolReturned` for the same `call_id` at any index `j > i`; log order; built from the event fields (raw args). Doc: while a deferral is open, the log holds a `ToolCall` without a `ToolResult`, so `snapshot()` of that session is not valid provider input.
  - `pub fn SessionRecorder::with_deferral_slot(mut self, slot: DeferralSlot) -> Self`.
  - `SessionRecorder::observe(RunDeferred { calls, .. })`: for each call, take the slot entry with the same `deferral_id` (raw args) and push `SessionEvent::tool_deferred(&raw)`. When no slot is wired or the id is missing, push `tool_deferred(call)` from the event and `tracing::warn!(target: "paigasus::core::session", ..)` once. `ApprovalRequested` stays unrecorded.
  - `drain()`: no synthetic result for a `ToolCalled` whose `call_id` has a `ToolDeferred` later in this run.

- [ ] **Step 1: Write the failing tests**

`session.rs` `recorder_tests`:

```rust
#[test]
fn observe_run_deferred_records_raw_args_from_the_slot()
// slot holds raw {"token":"sk-raw"}; event copy holds {"token":"***"} (same deferral_id)
// → drained ToolDeferred.args == {"token":"sk-raw"}.

#[test]
fn observe_does_not_record_approval_requested()

#[test]
fn drain_does_not_synthesize_for_a_deferred_call()
// observe ToolCallItem(a), ToolCallItem(b), ToolOutputItem(a), RunDeferred{[b]} →
// drained kinds: tool_called, tool_called, tool_returned, tool_deferred (no synthetic for b)
```

`session_projection.rs`:

```rust
#[test]
fn open_deferrals_uses_the_position_rule()
// log: tool_called fc_0, tool_deferred fc_0 (id d1), tool_returned fc_0,
//      tool_called fc_0, tool_deferred fc_0 (id d2)
// → open_deferrals == [d2] only.

#[test]
fn open_deferrals_ignores_a_result_before_the_deferral()
// log: tool_returned fc_0, tool_called fc_0, tool_deferred fc_0 (d1) → [d1].
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --lib recorder_tests`
Expected: FAIL (`with_deferral_slot` not found).

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core --lib recorder_tests && cargo test -p paigasus-helikon-core --test session_projection`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/session.rs crates/paigasus-helikon-core/src/deferral.rs crates/paigasus-helikon-core/tests/session_projection.rs
git commit -m "feat(core): SMA-803 record deferred calls with raw args and find open deferrals"
```

---

### Task 7: ResumePlan and the recorder's resume order

**Files:**
- Modify: `crates/paigasus-helikon-core/src/deferral.rs` (add `ResumePlan`, `PlannedCall`)
- Modify: `crates/paigasus-helikon-core/src/agent.rs:85-109` (`AgentInput` plan field)
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs:717` (`thread_handoff_transcript` → `pub(crate)`)
- Modify: `crates/paigasus-helikon-core/src/session.rs` (`SessionRecorder`)
- Test: unit tests in `deferral.rs` and `session.rs`

**Interfaces:**
- Consumes: Task 6 `open_deferrals`, Task 2 `AgentInput::resolutions`, `project`, `thread_handoff_transcript`.
- Produces:

```rust
#[derive(Debug, Clone)]
pub(crate) struct PlannedCall { pub(crate) pending: PendingToolCall, pub(crate) resolution: ToolResolution }

/// A validated resume. Only `from_events` makes one.
#[derive(Debug, Clone)]
pub struct ResumePlan {
    calls: Vec<PlannedCall>,       // open deferrals in log order, each with its resolution
    path: Vec<String>,             // the shared handoff path of the open deferrals
    histories: Vec<Vec<Item>>,     // histories[i] = the conversation of path[i]; [0] = snapshot
    level: usize,                  // which path entry the receiving agent is
}

impl ResumePlan {
    pub fn from_events(
        events: &[SessionEvent], snapshot: &ConversationSnapshot, root_agent_name: &str,
        input: &AgentInput, max_agent_depth: u32,
    ) -> Result<ResumePlan, InvalidResume>;
    pub(crate) fn calls(&self) -> &[PlannedCall];
    pub(crate) fn call_ids(&self) -> Vec<String>;
    pub(crate) fn expected_agent(&self) -> &str;          // path[level]
    pub(crate) fn next_agent(&self) -> Option<&str>;      // path[level + 1]
    pub(crate) fn history(&self) -> &[Item];              // histories[level]
    pub(crate) fn descend(&self) -> ResumePlan;           // level + 1
    pub(crate) fn with_all_denied(&self, reason: &str) -> ResumePlan; // every resolution → Denied { reason }
}
```

- `AgentInput` private field `resume_plan: Option<ResumePlan>`; `pub fn with_resume_plan(mut self, plan: ResumePlan) -> Self` (doc: set by a deferral-aware runner); `pub fn resume_plan(&self) -> Option<&ResumePlan>`.
- `pub fn SessionRecorder::with_resume_plan(mut self, plan: &ResumePlan) -> Self` — stores `plan.call_ids()`.

**`from_events` checks, in this order:**
1. `ToolItemsInInput` — `input.messages` has an `Item::ToolCall` or `Item::ToolResult`.
2. `DuplicateResolution` — two resolutions with the same id.
3. `NoOpenDeferrals` — `open_deferrals(events)` is empty (with or without resolutions).
4. `UnknownDeferral` — a resolution id is not open.
5. `UnresolvedDeferrals { missing }` — an open id has no resolution (all ids, log order).
6. `SessionModified` — a `UserMessage`, `AssistantMessage` or `ToolCalled` event after the index of the first open `ToolDeferred`; or the open deferrals have different `handoff_path`s; or the path has more than one name and a needed `HandoffOccurred` is missing (see below).
7. `CallNotInHistory { call_id }` — in `snapshot.messages`, the last `Item::ToolCall` with that `call_id` is missing, has a different `name`, or has an `Item::ToolResult` with that `call_id` after it.
8. `PathTooDeep { len, max_agent_depth }` — `path.len() > max_agent_depth as usize + 1`.

`root_agent_name` that differs from `path[0]` is not an error (Task 8b denies the calls). It only skips the child-history build (store `histories = vec![snapshot]`).

**Child histories (spec §4.7).** Let `d` = index of the first open `ToolDeferred`. For `i` in `1..path.len()`, `k_i` = index of the last `HandoffOccurred { from: path[i-1], to: path[i] }` with `k_{i-1} < k_i < d` (`k_0 = 0`, search from `0` for `i = 1`). If one is missing → `SessionModified`. Define `view(0, x) = project(&events[..x]).messages` and `view(i, x) = thread_handoff_transcript(&view(i-1, k_i)) ++ project(&events[k_i + 1..x]).messages`. Then `histories[0] = snapshot.messages` and `histories[i] = view(i, events.len())`.

**Recorder order** (`drain` with a plan):
- `observe`: until the first `TurnStarted`, each `ToolOutputItem` goes into a `resume_block` (in arrival order). `TurnStarted` ends the window.
- `drain` returns, in order: `resume_block`; one `tool_returned(call_id, "tool call did not complete (run cancelled/timed out)")` per planned call id with no entry in `resume_block`; the recorded input and run events; the position-based synthetic results.
- Replace the `HashSet` match in `drain` with a position rule over the final ordered list: a `ToolCalled` at index `i` is matched only by a `ToolReturned` with the same `call_id` at an index `> i`; no synthetic result when a `ToolDeferred` with that `call_id` follows it.

- [ ] **Step 1: Write the failing tests**

`deferral.rs` unit tests (build logs with `SessionEvent` constructors; `snapshot = project(&events)`):

```rust
#[test] fn from_events_rejects_tool_items_in_input()
#[test] fn from_events_rejects_duplicate_resolution()
#[test] fn from_events_rejects_resolutions_without_open_deferrals()   // → NoOpenDeferrals
#[test] fn from_events_rejects_empty_input_without_open_deferrals()   // → NoOpenDeferrals
#[test] fn from_events_rejects_unknown_deferral()
#[test] fn from_events_rejects_missing_resolution()                   // two open, one resolved → UnresolvedDeferrals { missing: [d2] }
#[test] fn from_events_rejects_empty_input_with_open_deferrals()      // Runner::resume case → UnresolvedDeferrals
#[test] fn from_events_rejects_session_modified()                     // user_message after the deferral
#[test] fn from_events_rejects_call_not_in_history()                  // snapshot passed without the ToolCall
#[test] fn from_events_rejects_path_too_deep()                        // path of 3, max_agent_depth = 1
#[test] fn from_events_accepts_a_renamed_root()                       // root_agent_name "other" → Ok
#[test] fn from_events_builds_the_child_history_without_transfer_or_root_tool_calls()
// log: user "hi"; assistant "routing" (triage); tool_called t1 "transfer_to_budgeting";
//      handoff_occurred triage→budgeting; assistant "checking" (budgeting);
//      tool_called c1 "pay"; tool_deferred c1 (path [triage, budgeting])
// → descend().history() == [User "hi", Assistant "routing", User <transfer note>,
//                            Assistant "checking", ToolCall c1]
//   and contains no ToolCall t1.
```

`session.rs` recorder tests:

```rust
#[test]
fn drain_puts_resume_results_before_the_input()
// with_resume_plan(plan with call b); record_input([User "next"]);
// observe ToolOutputItem(b), TurnStarted, MessageOutput("done")
// → kinds: tool_returned(b), user_message, assistant_message

#[test]
fn drain_closes_an_unanswered_planned_call_with_the_cancel_result()
// with_resume_plan(b); record_input([User "next"]); no observe
// → tool_returned(b, "tool call did not complete (run cancelled/timed out)"), user_message

#[test]
fn drain_matches_results_by_position_not_by_id()
// with_resume_plan(fc_0); observe ToolOutputItem(fc_0), TurnStarted, ToolCallItem(fc_0)
// → the final list ends with a synthetic tool_returned(fc_0, cancel text)
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --lib deferral`
Expected: FAIL to compile (`ResumePlan` not found).

- [ ] **Step 3: Implement**

Docs: `InvalidResume::CallNotInHistory` — "A compactor removed the deferred `ToolCall` item. No resume can fix such a session." `AgentInput::with_resolution` — "One writer per session at a time: two resumes of one session at the same time can both run an approved tool."

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/deferral.rs crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/src/loop_state.rs crates/paigasus-helikon-core/src/session.rs
git commit -m "feat(core): SMA-803 validate a resume into a resume plan and order its session events"
```

---

### Task 8a: LlmAgent resumes resolved calls (no handoff)

**Files:**
- Create: `crates/paigasus-helikon-core/src/resume_exec.rs` (add `mod resume_exec;` to `lib.rs`, not `pub`)
- Modify: `crates/paigasus-helikon-core/src/agent.rs:681-830` (`LlmAgent::run`: seed, guardrail exits, entry before `loop`)
- Create: `crates/paigasus-helikon-core/tests/deferred_resume.rs`

**Interfaces:**
- Consumes: Task 7 `ResumePlan` crate API and `AgentInput::resume_plan`; Task 4 `authorize_tool_detailed`, `AskPoints::Answered`; `finalize_tool_output`; `Interceptors::fire`.
- Produces (crate-private, `resume_exec.rs`):

```rust
pub(crate) async fn recheck_approved<Ctx: Send + Sync + 'static>(
    ctx: &RunContext<Ctx>, tool: Option<&Arc<dyn Tool<Ctx>>>, name: &str, recorded_args: &serde_json::Value,
) -> Result<(), String>; // Err(reason)
// None tool → Err(format!("tool no longer available: {name}"));
// Allow → Ok; Replace{args} == recorded → Ok; Replace{args} != recorded → Err("arguments changed on re-check");
// Deny{reason} → Err(reason); Defer → Err (unreachable with Answered; fail closed).

pub(crate) async fn run_resolved_calls<Ctx: Send + Sync + 'static>(
    tools: &[Arc<dyn Tool<Ctx>>], calls: &[PlannedCall], interceptors: &Interceptors<'_, Ctx>,
    tool_ctx: &ToolContext<Ctx>, limit: Option<NonZeroUsize>, parent: &tracing::Span,
) -> (Vec<ToolCallOutcome>, Vec<AgentEvent>); // outcomes in `calls` order; PermissionDenied events

pub(crate) fn not_run_results(calls: &[PlannedCall]) -> Vec<AgentEvent>;
// one ToolOutputItem per call, text "tool call did not run: the run ended before the resume completed"
```

**Execution rules** (spec §4.6 agent steps 3-5):
- No `PreToolUse`. Concurrent with `limit` (same `join_all` / `buffered` as `run_tools_concurrent`). Each call gets a `tool.execute` span with the existing fields plus `paigasus.tool.resumed = true`.
- `Denied { reason }` → `Err(format!("permission denied: {reason}"))` + `PermissionDenied { tool, reason }`.
- `Approved` → `recheck_approved`; `Err(reason)` → `Err("permission denied: {reason}")` + `PermissionDenied`; `Ok` → `tool.invoke(tool_ctx, recorded_args)`, then `PostToolUse` (its `Deny` → `Err("blocked by PostToolUse hook: {reason}")`), then `finalize_tool_output` (redaction last).

**`LlmAgent::run` changes:**
- At the top of the stream, after `RunStarted`: when `input.resolutions()` is non-empty and `input.resume_plan()` is `None` → `failure.set(AgentError::Other(anyhow!("resolutions require a deferral-aware runner")))`, yield `RunFailed { error: "resolutions require a deferral-aware runner" }`, return.
- With a plan: seed `conversation = [System?] + plan.history() + input.messages`; input guardrails see this conversation (unchanged code path). Any failure exit before the results are emitted (OnRunStart deny, input tripwire) yields `not_run_results(plan.calls())` before its `RunFailed`.
- This task handles only `plan.expected_agent() == agent_name && plan.next_agent().is_none()`. Other cases are Task 8b; until then route them to `with_all_denied(&format!("agent no longer available: {}", plan.expected_agent()))` only for the renamed-root case, and fail closed (`RunFailed`) for a path that continues.
- Before the `loop`: run `run_resolved_calls`; yield the `PermissionDenied` events, then one `ToolOutputItem` per outcome in order; insert the `ToolResult` items into `conversation` after the history and before the new input messages; then `tx_input = TransitionInput::Start { messages: input.messages }` with `CallingModel { turn: 0, .. }`.

- [ ] **Step 1: Write the failing tests**

`resume_exec.rs` `#[cfg(test)]`:

```rust
#[tokio::test] async fn recheck_denies_a_missing_tool()           // Err("tool no longer available: pay")
#[tokio::test] async fn recheck_passes_replace_with_equal_args()
#[tokio::test] async fn recheck_denies_replace_with_different_args() // Err("arguments changed on re-check")
#[tokio::test] async fn recheck_passes_an_allow_rule_without_calling_the_handler() // PanicHandler installed
#[tokio::test] async fn recheck_passes_with_the_flag_off()        // DeferPolicy, flag off → Ok
```

`tests/deferred_resume.rs` (build the deferring run with `LlmAgent` + `MemorySession` + a `SessionRecorder::new(..).with_deferral_slot(ctx.deferral_handle())` fed by the stream; append the drained events; then `ResumePlan::from_events(..)` and `AgentInput::from_user_text("next").with_resume_plan(plan)`):

```rust
#[tokio::test]
async fn approved_call_runs_once_with_recorded_args_before_the_new_input()
// resume: tool b invoked exactly once with the recorded args; the first model request of the
// resume ends with [.., ToolCall b, ToolResult b, User "next"]; events start
// RunStarted, ToolOutputItem(b), TurnStarted { turn: 0 }; RunCompleted.

#[tokio::test]
async fn denied_call_gets_the_denial_result_and_the_tool_does_not_run()
// ToolResolution::Denied { reason: "nope" } → ToolOutputItem text "permission denied: nope",
// PermissionDenied { tool: "b", reason: "nope" }, tool b invoked 0 times.

#[tokio::test]
async fn approved_call_with_post_tool_use_hook_and_redaction()
// PostToolUse ReplaceOutput adds the extra secret → the result text has "***".

#[tokio::test]
async fn input_guardrail_tripwire_closes_the_deferral()
// tripping input guardrail on resume → ToolOutputItem(b, "tool call did not run: the run ended
// before the resume completed") then GuardrailTriggered/RunFailed; tool b not invoked; model not invoked.

#[tokio::test]
async fn renamed_root_denies_and_continues()
// resume agent named "other" → ToolOutputItem(b, "permission denied: agent no longer available: root"),
// then the loop calls the model (one script), RunCompleted.

#[tokio::test]
async fn resolutions_without_a_plan_fail()
// agent.run(ctx, AgentInput::new().with_resolution("x", Approved)) →
// RunFailed { error: "resolutions require a deferral-aware runner" }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --test deferred_resume`
Expected: FAIL (the plan is ignored; tool b is not invoked).

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/resume_exec.rs crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/tests/deferred_resume.rs
git commit -m "feat(core): SMA-803 run resolved deferred calls after a re-check on resume"
```

---

### Task 8b: Resume routes to the recorded handoff agent

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (resume entry from Task 8a; reuse the `Handoff` arm's depth check and re-yield)
- Modify: `crates/paigasus-helikon-core/tests/deferred_resume.rs`

**Interfaces:**
- Consumes: Task 7 `expected_agent`, `next_agent`, `descend`, `with_all_denied`, `history`; Task 8a entry and `not_run_results`.
- Produces: no new API.

**Routing rules** (spec §4.7), evaluated by each `LlmAgent` that receives a plan:
1. `agent_name != plan.expected_agent()` (only possible at level 0) → `plan = plan.with_all_denied(&format!("agent no longer available: {}", plan.expected_agent()))`, run it here (Task 8a path), continue the loop.
2. `next_agent() == None` → Task 8a path.
3. `next_agent() == Some(next)` and a handoff target with `agent().name() == next` exists → `child = ctx.handoff_child()`; depth check as in the `Handoff` arm (failure exit yields `not_run_results` first); do NOT fire `OnHandoff` and do NOT emit `HandoffItem`; yield `AgentUpdated { agent: next }`; run the target with `AgentInput { messages: input.messages, resume_plan: Some(plan.descend()), .. }`; re-yield with the same arms as the `Handoff` arm (drop `RunStarted`, sum `RunCompleted` and `RunDeferred` usage with a zero `parent_usage`); fire `OnSubagentStop { agent: next }` at the end; return.
4. `next_agent() == Some(next)` with no such target → `plan = plan.with_all_denied(&format!("agent no longer available: {next}"))`; this agent runs the denials with its own `plan.history()` and continues its own loop.

- [ ] **Step 1: Write the failing tests** in `tests/deferred_resume.rs`

```rust
#[tokio::test]
async fn handoff_resume_runs_the_call_on_the_child_with_the_child_conversation()
// run 1: triage → budgeting (transfer turn), budgeting calls b (deferred).
// resume (Approved): triage's model is never invoked; budgeting's first request is
//   [System(budgeting), User "hi", User <transfer note>, Assistant?, ToolCall b, ToolResult b, User "next"]
//   with no transfer ToolCall; events contain AgentUpdated { agent: "budgeting" } and no HandoffItem;
//   an OnHandoff counting hook stays at 0; an OnSubagentStop hook fires once.
// assert_tool_calls_paired(&request) for every request of the resume.

#[tokio::test]
async fn missing_handoff_target_denies_the_calls_at_the_deepest_reachable_agent()
// resume with a triage that has no budgeting handoff → ToolOutputItem(b,
// "permission denied: agent no longer available: budgeting"); triage's model is called next.
```

Add a test helper in the file:

```rust
/// Each ToolCall has a ToolResult with the same call_id later, with no UserMessage or
/// AssistantMessage between them. `skip` lists call ids exempt from the check.
fn assert_tool_calls_paired(messages: &[Item], skip: &[&str]);
```

In the missing-target test, call it with `skip = ["<transfer call id>"]`: the root history still holds the handoff transfer call without a result (SMA-810, a non-goal of this ticket).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core --test deferred_resume handoff`
Expected: FAIL (Task 8a fails closed on a continuing path).

- [ ] **Step 3: Implement the routing rules**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs crates/paigasus-helikon-core/tests/deferred_resume.rs
git commit -m "feat(core): SMA-803 route a resume to the recorded handoff agent"
```

---

### Task 9: TokioRunner resumes and guards the deferral write

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs:77-114` (`load_and_record`, `finalize`), `:125-174` (`run`), `:176-234` (`run_streamed`)
- Modify: `crates/paigasus-helikon-core/src/runner.rs:64-160` (`Runner` trait docs)
- Create: `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs`

**Interfaces:**
- Consumes: `open_deferrals`, `ResumePlan::from_events`, `AgentInput::{resolutions, with_resume_plan}`, `SessionRecorder::{with_deferral_slot, with_resume_plan}`, `RunContext::deferral_handle`, `RunError::{InvalidResume, SessionWrite}`, `StopReason`.
- Produces (private to the crate):

```rust
struct Loaded { input: AgentInput, recorder: Arc<Mutex<SessionRecorder>>, resuming: bool }
async fn load_and_record(session: &Arc<dyn Session>, agent_name: &str, input: AgentInput,
    deferrals: DeferralSlot, max_agent_depth: u32) -> Result<Loaded, RunError>;
async fn finalize(session: &Arc<dyn Session>, recorder: &Arc<Mutex<SessionRecorder>>) -> Result<(), SessionError>;
// logs the warning as today and also returns the error
```

**Runner rules** (spec §4.5, §4.6 runner steps):
1. Read `session.snapshot()` and `session.events(None)`; a read error → `Err(RunError::Other(..))` as today.
2. `resuming = !input.resolutions().is_empty() || !open_deferrals(&events).is_empty()`. If resuming: `ResumePlan::from_events(&events, &snapshot, agent.name(), &input, config.max_agent_depth)`; an error → `Err(RunError::InvalidResume(e))` before the agent starts, nothing appended. Agent input = `AgentInput` with `messages = input.messages` (new input only) and `with_resume_plan(plan)`, no resolutions. Recorder = `SessionRecorder::new(name).with_deferral_slot(slot).with_resume_plan(&plan)`, then `record_input(&new messages)`.
3. Not resuming: merged input as today; recorder with the slot.
4. `run`: after `collect`, call `finalize`. A strict run is `resuming` or `collected.stop_reason` is `Deferred`. A strict run with an append error returns `Err(RunError::SessionWrite(e))` (this wins over the agent result and the interrupt). Other runs keep best-effort.
5. `run_streamed`: on a terminal event, finalize first; a strict terminal (`resuming`, or the event is `RunDeferred`) with an append error yields `RunFailed { error: format!("session write failed: {e}") }` instead of the event. The interrupt-path finalize keeps best-effort.

`Runner` trait docs (core `runner.rs`): `run` — the terminal list becomes `RunCompleted`/`RunFailed`/`RunDeferred`; state the one-writer-per-session precondition; state that a runner that does not support resume must reject a run on a session with open deferrals (use `open_deferrals()`), or its run puts a user message after an unanswered `ToolCall`. `resume`/`resume_streamed` — send an empty input, so on a session with open deferrals they return `RunError::InvalidResume`. Change "persistence is best-effort" to name the two strict cases.

- [ ] **Step 1: Write the failing tests** in `crates/paigasus-helikon-runtime-tokio/tests/deferred.rs`

Use a `RecordingModel` (records each request), a counting `PayTool`, a `DeferPolicy` that defers `pay`, ctx `with_deferred_tool_calls(true)` and a `MemorySession`; a `FailingAppendSession` wrapper whose `append` returns `Err(SessionError::Unavailable)` and whose reads delegate.

```rust
#[tokio::test] async fn deferral_round_trip_stores_raw_args()
// PreToolUse ReplaceInput adds {"token":"sk-test-SECRET-1234"} (extra secret) →
// session log ToolDeferred.args has the raw token; RunResult.stop_reason and the
// RunDeferred/ApprovalRequested events do not; log tail is tool_called, tool_deferred.
#[tokio::test] async fn resume_approved_runs_the_tool_once_with_recorded_args()
// next model request: [.., ToolCall pay, ToolResult pay, User "next"]; tool count 1;
// open_deferrals(log) is empty after the resume.
#[tokio::test] async fn resume_denied_records_the_denial()
#[tokio::test] async fn repeated_call_id_does_not_match_an_old_deferral()
// turn ids reuse "fc_0": deferral 1 resumed; a later run defers fc_0 again (d2);
// resolving d1 → InvalidResume::UnknownDeferral; resolving d2 → Ok.
#[tokio::test] async fn invalid_resume_reasons_reach_the_caller()
// one sub-case per reason that a runner can hit end-to-end:
//   NoOpenDeferrals (resolution on a clean session), UnresolvedDeferrals (Runner::resume on an open
//   deferral), UnknownDeferral, DuplicateResolution, ToolItemsInInput, SessionModified (append a
//   user message to the log by hand), CallNotInHistory (session wrapper whose snapshot drops the
//   ToolCall), PathTooDeep (RunConfig max_agent_depth = 0 on a handoff deferral);
//   assert each returns Err(RunError::InvalidResume(<reason>)) and the log length is unchanged.
#[tokio::test] async fn renamed_root_denies_and_continues()
#[tokio::test] async fn input_guardrail_tripwire_closes_the_deferral_before_the_input()
// log after: tool_returned(pay, "tool call did not run: the run ended before the resume completed"),
// then user_message("next"); open_deferrals empty.
#[tokio::test] async fn cancelled_resume_closes_the_deferral()
// approved tool blocks on a Notify; cancel the token → Err(Cancelled); log: tool_returned(pay,
// "tool call did not complete (run cancelled/timed out)") before user_message("next").
#[tokio::test] async fn failed_append_on_deferring_run_returns_session_write()
// run → Err(RunError::SessionWrite(_)); run_streamed → last event RunFailed starting "session write failed:".
#[tokio::test] async fn failed_append_on_resume_returns_session_write()
#[tokio::test] async fn run_streamed_ends_with_run_deferred_after_finalize()
// the session holds tool_deferred at the moment the RunDeferred event is observed.
#[tokio::test] async fn resume_works_with_flag_off()
#[tokio::test] async fn chained_deferral_resumes_twice()
// resume with the flag on; the resumed turn defers pay again → second RunDeferred, one open
// deferral with a new id; a third run resolves it.
#[tokio::test] async fn mixed_resolutions_keep_call_order()
// one turn defers pay(call_1) and pay(call_2); resolve call_1 Approved and call_2 Denied →
// next request has ToolResult call_1 then ToolResult call_2 then User "next";
// resolving only call_1 → UnresolvedDeferrals.
#[tokio::test] async fn handoff_deferral_round_trip()
// triage → budgeting defers pay; resume via TokioRunner runs pay on budgeting;
// assert_tool_calls_paired on every resume request (copy the helper from core's test).
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-tokio --test deferred`
Expected: FAIL (`resume_approved_…`: the tool does not run; the run appends a user message after the open `ToolCall`).

- [ ] **Step 3: Implement the runner rules and the trait docs**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-tokio && cargo test -p paigasus-helikon-core --lib runner`
Expected: PASS (old `session_persistence`, `run_streamed`, `run_control` tests stay green).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-runtime-tokio/src/lib.rs crates/paigasus-helikon-runtime-tokio/tests/deferred.rs crates/paigasus-helikon-core/src/runner.rs
git commit -m "feat(runtime-tokio): SMA-803 resume deferred calls and fail a lost deferral write"
```

---

### Task 10: Workflow agents reject resume input

**Files:**
- Modify: `crates/paigasus-helikon-core/src/workflow.rs:138-175` (Sequential), `:291+` (Parallel), `:496+` (Loop); `crates/paigasus-helikon-core/src/graph.rs:203+`; `crates/paigasus-helikon-core/src/swarm.rs:257+` (`SwarmAgent` only, not `MemberSlot`)
- Test: `crates/paigasus-helikon-core/tests/workflow_sequential.rs`, `workflow_parallel.rs`, `workflow_loop.rs`, `graph.rs`, `swarm.rs`

**Interfaces:**
- Consumes: `AgentInput::{resolutions, resume_plan}`.
- Produces: `pub(crate) fn has_resume_input(input: &AgentInput) -> bool` in `workflow.rs` (`!resolutions().is_empty() || resume_plan().is_some()`).

Rule: in each of the five `run` streams, directly after `RunStarted`: if `has_resume_input(&input)` → `parent_failure.set(AgentError::Other(anyhow!("this agent cannot resume deferred calls")))`, yield `RunFailed { error: "this agent cannot resume deferred calls" }`, return. No child runs.

- [ ] **Step 1: Write the failing tests** — one per agent, named `<agent>_rejects_resolutions` (`sequential_agent_rejects_resolutions`, `parallel_agent_rejects_resolutions`, `loop_agent_rejects_resolutions`, `graph_agent_rejects_resolutions`, `swarm_agent_rejects_resolutions`). Input `AgentInput::from_user_text("x").with_resolution("d1", ToolResolution::Approved)`. Assert the last event is `RunFailed { error: "this agent cannot resume deferred calls" }` and the child agent (a `MockAgent` with a call counter) ran 0 times.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-core rejects_resolutions`
Expected: FAIL (the children run).

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/workflow.rs crates/paigasus-helikon-core/src/graph.rs crates/paigasus-helikon-core/src/swarm.rs crates/paigasus-helikon-core/tests/workflow_sequential.rs crates/paigasus-helikon-core/tests/workflow_parallel.rs crates/paigasus-helikon-core/tests/workflow_loop.rs crates/paigasus-helikon-core/tests/graph.rs crates/paigasus-helikon-core/tests/swarm.rs
git commit -m "feat(core): SMA-803 make workflow agents reject resume input"
```

---

### Task 11: TemporalRunner rejects resume and clears the flag

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/runner.rs:170-222` (`run_inner`), tests `:377+`

**Interfaces:**
- Consumes: `open_deferrals`, `InvalidResume::ResumeUnsupported`, `RunContext::{deferred_tool_calls_enabled, with_deferred_tool_calls}`.
- Produces (private):

```rust
/// Err(RunError::InvalidResume(InvalidResume::ResumeUnsupported)) when the input has
/// resolutions or the log has open deferrals.
fn reject_deferred_resume(events: &[SessionEvent], input: &AgentInput) -> Result<(), RunError>;
```

Rule: at the start of `run_inner`: if `ctx.deferred_tool_calls_enabled()`, `tracing::warn!(target: "paigasus::runtime_temporal::runner", "TemporalRunner does not support deferred tool calls; the flag is cleared and a Defer becomes a Deny")` and `ctx = ctx.with_deferred_tool_calls(false)`. Then read `session.events(None)` (read error → `RunError::Other`, as the snapshot read) and call `reject_deferred_resume` before `load_and_record`, so nothing is recorded. The driver keeps its wildcard arms.

- [ ] **Step 1: Write the failing tests** (unit, in `runner.rs` `mod tests`; no Temporal server)

```rust
#[test] fn rejects_resolutions()          // clean log + with_resolution → ResumeUnsupported
#[test] fn rejects_an_open_deferral()     // log with tool_called + tool_deferred, empty input → ResumeUnsupported
#[test] fn accepts_a_clean_log()          // Ok(())
#[test] fn accepts_a_closed_deferral()    // tool_deferred then tool_returned → Ok(())
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-temporal --lib rejects_`
Expected: FAIL to compile (`reject_deferred_resume` not found).

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-temporal --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-runtime-temporal/src/runner.rs
git commit -m "feat(runtime-temporal): SMA-803 reject deferred resumes and clear the deferral flag"
```

---

### Task 12: HTTP runtimes clear the deferral flag

**Files:**
- Modify: `crates/paigasus-helikon-runtime-axum/src/context.rs` (helper + unit test), `crates/paigasus-helikon-runtime-axum/src/handlers/runs.rs:220`
- Modify: `crates/paigasus-helikon-runtime-actix/src/context.rs` (helper + unit test), `crates/paigasus-helikon-runtime-actix/src/handlers/runs.rs:257`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/invoke.rs:165` (helper + unit test here), `src/ws.rs:203`, `src/agui/sse.rs:147`, `src/agui/ws.rs:267`, `src/a2a/rpc.rs:516-520`
- Test: `crates/paigasus-helikon-runtime-axum/tests/runs.rs`, `crates/paigasus-helikon-runtime-actix/tests/runs.rs`

**Interfaces:**
- Consumes: `RunContext::{deferred_tool_calls_enabled, with_deferred_tool_calls}`.
- Produces, once per crate (private to the crate):

```rust
/// Clears run features this runtime does not support. SMA-804 adds its checkpoint clear here.
pub(crate) fn restrict_unsupported_run_features<Ctx: Send + Sync + 'static>(ctx: RunContext<Ctx>) -> RunContext<Ctx>;
// flag on → one tracing::warn! (target paigasus::runtime_axum::context / paigasus::runtime_actix::context /
// paigasus::runtime_agentcore::invoke) and return ctx.with_deferred_tool_calls(false); else ctx unchanged.
```

Call it directly after each of the seven `build(..)` results: `let ctx = restrict_unsupported_run_features(ctx);` (for the `match` sites, after the `Ok(c) => c` binding). In agentcore the helper lives in `invoke.rs` (always compiled); the feature-gated modules call `crate::invoke::restrict_unsupported_run_features`.

- [ ] **Step 1: Write the failing tests**

Unit test in each helper's module: `restrict_clears_the_deferral_flag` (flag on → off) and `restrict_keeps_a_context_without_the_flag`.

Integration tests (axum `tests/runs.rs`, actix `tests/runs.rs`), named `oneshot_run_clears_the_deferral_flag`: a `ContextProvider` that returns `RunContext::ephemeral(()).with_session(session).with_cancel(cancel).with_deferred_tool_calls(true)`; a probe `Agent` whose `run` yields `MessageOutput` with text `format!("deferral={}", ctx.deferred_tool_calls_enabled())` and `RunCompleted`. Assert the one-shot response `final_output == "deferral=false"`.

agentcore: one probe test in the existing `invoke.rs` test module, `invoke_clears_the_deferral_flag`, with the same probe agent and provider.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-axum --test runs oneshot_run_clears_the_deferral_flag`
Expected: FAIL (`deferral=true`).

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-axum -p paigasus-helikon-runtime-actix -p paigasus-helikon-runtime-agentcore && cargo build -p paigasus-helikon-runtime-agentcore --no-default-features`
Expected: PASS, and the no-default-features build has no dead-code warning.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-runtime-axum/src/context.rs crates/paigasus-helikon-runtime-axum/src/handlers/runs.rs crates/paigasus-helikon-runtime-axum/tests/runs.rs crates/paigasus-helikon-runtime-actix/src/context.rs crates/paigasus-helikon-runtime-actix/src/handlers/runs.rs crates/paigasus-helikon-runtime-actix/tests/runs.rs crates/paigasus-helikon-runtime-agentcore/src/invoke.rs crates/paigasus-helikon-runtime-agentcore/src/ws.rs crates/paigasus-helikon-runtime-agentcore/src/agui/sse.rs crates/paigasus-helikon-runtime-agentcore/src/agui/ws.rs crates/paigasus-helikon-runtime-agentcore/src/a2a/rpc.rs
git commit -m "feat(runtime): SMA-803 clear the deferral flag in the http runtimes"
```

---

### Task 13: CompactingSession does not compact during an open deferral

**Files:**
- Modify: `crates/paigasus-helikon-core/src/compacting_session.rs:154-232` (`maybe_compact`, after step 3 reads `evs`)
- Test: `crates/paigasus-helikon-core/tests/compacting_session.rs`

**Interfaces:**
- Consumes: `open_deferrals`.
- Produces: no new API.

Rule: after `let evs = self.inner.events(None).await?;`, if `!open_deferrals(&evs).is_empty()`, emit `tracing::debug!(target: "paigasus::core::compaction", ..)` and return `Ok(())` without a model call. Do not update the cheap estimate, so the next append re-checks.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn does_not_compact_while_a_deferral_is_open()
// threshold low; append big user messages, then tool_called c1 + tool_deferred c1 →
// no Compacted event, the summary model was invoked 0 times.
// then append tool_returned c1 + a big user message → a Compacted event appears.
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-core --test compacting_session does_not_compact_while_a_deferral_is_open`
Expected: FAIL (a `Compacted` event appears).

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core --test compacting_session`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/compacting_session.rs crates/paigasus-helikon-core/tests/compacting_session.rs
git commit -m "feat(core): SMA-803 skip compaction while a deferral is open"
```

---

### Task 14: Docs, book, READMEs and the release note

**Files:**
- Modify: `crates/paigasus-helikon-core/src/lib.rs:1-15` (crate docs: a "Deferred tool calls" section with the rollback note)
- Modify: `crates/paigasus-helikon-core/src/session.rs` (`SessionEvent::ToolDeferred` doc: open-deferral `snapshot()` caveat, rollback note)
- Modify: `docs/book/src/concepts/permissions-guardrails-hooks.md:11-71` (new "Deferred tool calls" subsection)
- Modify: `docs/book/src/concepts/agent-loop.md:16` (`NeedsApproval` is terminal), `:50-66` (event list: `RunDeferred`; `ApprovalRequested` args are effective, redacted), driving section (`RunResult.stop_reason`)
- Modify: `docs/book/src/concepts/sessions.md:35-70` (`tool_deferred` event, projection, open deferral), `:270-297` (compaction skip), `:374+` (strict write for deferring/resuming runs, rollback note)
- Modify: `docs/book/src/concepts/runtimes.md` (TokioRunner resumes; TemporalRunner and HTTP runtimes clear the flag and reject resume)
- Modify: `docs/book/src/concepts/core-primitives.md:17` (terminal events now include `RunDeferred`)

**Interfaces:** none.

Content the book subsection must state (from the spec, one fact each):
- What defers: a policy `Defer`, and an `ApprovalHandler` `Defer` on a guard `Ask` or an `AskUser`. Every ask point calls the handler, so one call can produce more than one `decide`.
- The opt-in: `RunContext::with_deferred_tool_calls(true)`; off by default; off in `AgentAsTool` and workflow children; kept by handoff children; cleared by TemporalRunner, axum, actix and agentcore. With the flag off, a `Defer` is a deny with "approval deferred, but this run cannot defer: …".
- The end of the run: `StopReason::Deferred { calls }`, `AgentEvent::RunDeferred`, one `ApprovalRequested` per call, one `tool_deferred` session event per call; `collect_typed` returns `AgentError::Incomplete`.
- The resume: a code block (fenced ` ```rust,ignore `):

```rust,ignore
let input = AgentInput::new()
    .with_resolution(pending.deferral_id(), ToolResolution::Approved);
let result = TokioRunner.run(&agent, ctx, input, config).await?;
```

- The re-check: no `PreToolUse`; deny rules, guards, allow rules, mode and policy run again; ask points count as answered; any deny wins; a `Replace` with different args denies; policy `Replace` must be idempotent.
- All open calls resolve together; the `InvalidResume` reasons; one writer per session.
- Secrets: the session stores the raw effective args; events carry redacted args.
- The rollback note (verbatim from Global Constraints).

READMEs: `crates/paigasus-helikon-core/README.md` shows no approval flow and lists no event set, so make no edit (a conscious call). No other crate README shows the permission or event API; record this call in the PR body.

Release note: `release-plz.toml` sets no changelog `body` template, so release-plz uses its default, which renders only the commit subject. Put the rollback note in the PR body under a "Release note" heading, and add one line there: "After release-plz opens the release PR, add the rollback note to `crates/paigasus-helikon-core/CHANGELOG.md` in that PR before merge." Do not edit any CHANGELOG on this branch.

- [ ] **Step 1: Write the docs**

- [ ] **Step 2: Verify docs and the full gate set**

Run:
```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
mdbook build docs/book
npx markdownlint-cli2
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
```
Expected: all clean. (If `mdbook` or the nightly toolchain is missing locally, say so in the report; CI runs both.) If `cargo test --workspace --all-features` fails only in `paigasus-helikon-providers-bedrock` with NATIVE_ROOTS errors on macOS, that is the known checkout-path issue, not this change; confirm by running the same command from a worktree under the scratchpad.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/src/session.rs docs/book/src/concepts/permissions-guardrails-hooks.md docs/book/src/concepts/agent-loop.md docs/book/src/concepts/sessions.md docs/book/src/concepts/runtimes.md docs/book/src/concepts/core-primitives.md
git commit -m "docs(docs): SMA-803 document deferred tool calls and the rollback note"
```

PR title (for the open-PR stage, not this task): `feat(core): SMA-803 defer a tool call to a later run`.

---

## Self-review

**Spec coverage.**
- §4.1 types → Tasks 1, 2, 3, 7. §4.2 flag and copy sites → Tasks 3, 5 (AgentAsTool and workflow child), 11, 12. §4.3 pipeline → Task 4; rule 8 (unknown tool) → Task 5, because the authorization function has no tool lookup. §4.4 loop → Task 5. §4.5 recording → Tasks 6, 7, 9; `collect`/`collect_typed`/`parse_final` → Task 2; CompactingSession → Task 13. §4.6 resume → Tasks 7, 8a, 9; other runners → Tasks 10, 11; `Runner` docs → Task 9. §4.7 handoffs → Tasks 5 (path, re-yield), 7 (child history), 8b (routing). §4.8 re-check → Tasks 4, 8a. §4.9 Temporal → Task 11. §4.10 counts, docs, rollback → Tasks 2, 14.
- §5 tests: authorization (Task 4), re-check (Tasks 4, 8a), loop (Task 5), runner (Task 9), handoff (Tasks 8b, 9), propagation (Tasks 5, 12), workflow (Task 10), Temporal (Task 11), serde snapshots (Tasks 1, 2), projection and `open_deferrals` (Tasks 2, 6), CompactingSession (Task 13).
- The optional translator test (§4.7, "welcome if the dev-dependency graph allows") is not planned: it needs a provider crate as a dev-dependency of a runtime or core crate. The structural pairing check covers the requirement.

**Type consistency.** `Authorization`/`AskPoints` (Task 4) are used by Tasks 5 and 8a. `ConcurrentOutcome` is local to Task 5. `PlannedCall` and the `ResumePlan` crate API (Task 7) are used by Tasks 8a, 8b, 9 (only `from_events` and `with_resume_plan` cross the crate boundary). `DeferralSlot::record` is `pub(crate)` and is used only in core (Tasks 3, 5); runtime-tokio uses `deferral_handle()` and `with_deferral_slot()`.

**Proportion.** Code blocks are signatures and test assertions; algorithm text appears only where the spec fixes it (pipeline rules, position rule, child-history derivation, recorder order).
