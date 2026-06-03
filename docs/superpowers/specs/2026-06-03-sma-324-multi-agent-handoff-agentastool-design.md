# SMA-324 — Multi-agent: Handoff + `AgentAsTool`

**Status:** Design (approved)
**Issue:** [SMA-324](https://linear.app/smaschek/issue/SMA-324/multi-agent-handoff-agentastool)
**Branch:** `feature/sma-324-multi-agent-handoff-agentastool`
**Date:** 2026-06-03
**References:** [Multi-Agent Patterns](https://www.notion.so/355830e8fbaa81ff9b43d6e466d5fc14) (Notion). Resolves the handoff-usage seam flagged in [SMA-402](https://linear.app/smaschek/issue/SMA-402) §3.5.

## 1. Summary

First multi-agent primitives. Two distinct shapes that both compose through the
**existing `Agent<Ctx>` trait** — no new trait surface:

- **Handoff** *transfers the conversation*. When an `LlmAgent`'s `handoffs` list is
  non-empty, the loop injects synthetic `transfer_to_<name>` tools into the schema the
  model sees. When the model calls one, the run **switches the active agent**: the driver
  runs the target's `Agent::run` with the threaded transcript and forwards its events. The
  parent does not regain control unless the target hands back.
- **`AgentAsTool`** *calls a sub-agent and continues in the parent*. An adapter that wraps
  any `Agent<Ctx>` as a `Tool<Ctx>`; the parent calls it like any tool, gets the
  sub-agent's `final_output` back as `ToolOutput`, and keeps reasoning.

Most of the scaffolding already exists and is wired here for the first time:

- `LlmAgent.handoffs` exists ("stored but not driven" — `agent.rs`).
- `LoopState::ApplyingHandoff { target, transcript }` exists and currently returns
  `AgentError::NotImplemented { feature: "handoff" }` (`loop_state.rs`).
- `AgentEvent::HandoffItem { from, to }`, `AgentEvent::AgentUpdated { agent }`, and
  `HookEvent::OnHandoff { from, to }` already exist.
- The synthetic-tool plumbing (`ToolDef`, the model request's `tools`) already exists.

The design is therefore mostly **wiring + one new wrapper type + one adapter**, plus a
small bounded-recursion guard.

## 2. Scope

### In scope

- New `Handoff<Ctx>` type (`src/handoff.rs`) with a `to(agent)` constructor — minimal
  wrapper around `Arc<dyn Agent<Ctx>>` (no per-edge overrides this ticket; §6).
- New `AgentAsTool<Ctx>` adapter (`src/agent_as_tool.rs`) implementing `Tool<Ctx>`.
- Drive the handoff path: inject `transfer_to_<slug>` tool defs; route a matching tool
  call to `ApplyingHandoff` in `transition`; run the target via nested delegation in the
  driver, forwarding its events.
- `LlmAgent.handoffs` field retyped `Vec<Arc<dyn Agent<Ctx>>>` → `Vec<Handoff<Ctx>>`;
  builder updated (`.handoff`, `.shared_handoff`, `.handoffs`).
- Bounded handoff recursion: `RunContext.handoff_depth` + `RunConfig.max_handoff_depth`
  (default 8) + `AgentError::HandoffDepthExceeded`.
- Resolve the SMA-402 usage seam: cumulative usage **accumulates across the handoff
  chain** — `ApplyingHandoff` gains a `usage` field; the forwarded `RunCompleted` carries
  parent-pre-handoff + sub-run usage (§3.4).
- Tests: pure `transition` unit tests (handoff routing, slug collision), two end-to-end
  core integration tests (3-agent triage, `AgentAsTool` round-trip), and a runnable facade
  example.
- Docs: `///` on every new public item (`missing_docs` is `-D warnings`).

### Out of scope (YAGNI)

- **`OnHandoff` hook is not fired** (§6, D2). No hook is dispatched anywhere in the loop
  today (`PreToolUse`/`PostToolUse`/… are all stored-but-not-driven); firing one only for
  handoff would be a one-off path. The observable `HandoffItem`/`AgentUpdated` **events**
  are emitted; hook dispatch (and honoring `HookDecision::Deny` on a transfer) lands when
  hooks are wired generally.
- **No `Handoff` overrides** — no custom `tool_name`/`tool_description`, no transcript
  input-filter callback, no typed handoff-input. Deferred to a follow-up.
- No `SequentialAgent`/`ParallelAgent`/`LoopAgent`/`SwarmAgent`/`GraphAgent` (separate
  tickets) — though nested delegation is deliberately built so they slot in unchanged.
- No structured-output passthrough from `AgentAsTool` (the sub-agent's `final_output` text
  becomes a `ToolOutput` string; parsing back to a typed value is the parent's concern).

## 3. Design

### 3.1 `Handoff<Ctx>` — the minimal wrapper (`src/handoff.rs`)

```rust
/// A candidate agent an `LlmAgent` may transfer the conversation to.
pub struct Handoff<Ctx> {
    agent: std::sync::Arc<dyn crate::Agent<Ctx>>,
}

impl<Ctx: Send + Sync + 'static> Handoff<Ctx> {
    /// Transfer target from an owned agent (wrapped in `Arc`).
    pub fn to(agent: impl crate::Agent<Ctx> + 'static) -> Self { … }
    /// Transfer target from a pre-wrapped trait object.
    pub fn shared(agent: std::sync::Arc<dyn crate::Agent<Ctx>>) -> Self { … }
    /// The target agent.
    pub fn agent(&self) -> &std::sync::Arc<dyn crate::Agent<Ctx>> { &self.agent }
}

impl<Ctx> Clone for Handoff<Ctx> { … } // clones the Arc only
```

The wrapper is intentionally thin: with no per-edge overrides this ticket, `Handoff<Ctx>`
is morally `Arc<dyn Agent<Ctx>>`, but it is the **named home** for future per-edge config
(tool-name override, input filter) and matches the Notion snippet's
`.handoffs([Handoff::to(a), Handoff::to(b)])`.

**`HandoffDef`** — the pure-data projection the state machine consumes (the driver
precomputes one per handoff before the loop):

```rust
/// Pure-data description of one injected transfer tool. Built by the driver.
#[derive(Debug, Clone)]
pub struct HandoffDef {
    pub tool_name: String,    // "transfer_to_<slug>"
    pub target: String,       // the real agent.name() — used in events/lookup
    pub description: String,   // the target agent.description()
}
```

**Slug derivation & collision:** `tool_name = format!("transfer_to_{}", slug(agent.name()))`
where `slug` lowercases and replaces every run of non-`[a-z0-9_]` with a single `_`
(`"AML cytogenetics"` → `transfer_to_aml_cytogenetics`). The driver builds the
`Vec<HandoffDef>` once at run start and **fails fast** if two slugs collide (two targets
mapping to the same `tool_name`) — surfaced as a terminal `RunFailed` + a structured
`AgentError::Other` with a message naming the colliding agents. The `target` carried in
events and used for lookup is always the **real** `agent.name()`, never the slug.

### 3.2 `LlmAgent` field + builder change (`agent.rs`, `agent_builder.rs`)

`LlmAgent.handoffs` is retyped `Vec<Arc<dyn Agent<Ctx>>>` → `Vec<Handoff<Ctx>>`. The
builder mirrors the change (and `LlmAgentBuilder.handoffs` likewise), threading the new
type through every typestate transition's struct copy:

- `.handoff(impl Agent<Ctx> + 'static)` — unchanged signature; now wraps via `Handoff::to`.
- `.shared_handoff(Arc<dyn Agent<Ctx>>)` — unchanged signature; now wraps via `Handoff::shared`.
- `.handoffs<I>(I)` — bound changes from `Item = Arc<dyn Agent<Ctx>>` to
  `Item = Handoff<Ctx>`, matching the Notion snippet. This is the only **source-breaking**
  builder-call change (§5); existing tests assert `handoffs.is_empty()` and still compile.

### 3.3 State machine: routing a transfer call (`loop_state.rs`)

**`TransitionCtx` gains `handoffs: &'a [HandoffDef]`.** Every `CallModel` arm that builds a
`ModelRequest` now sends `tools = [real tool defs ++ handoff tool defs]` so the model can
see and call the transfer tools. The handoff defs are converted to `ToolDef`s inline
(`{ name: tool_name, description, schema: empty-object }`); the transfer tools take **no
arguments** (`{"type":"object","properties":{},"additionalProperties":false}`) — the
conversation *is* the payload.

A **new routing branch is inserted before the existing tool-call branch.** On
`(CallingModel { turn, usage: prior }, ModelResponse { items, usage, .. })`, if any
`Item::ToolCall` whose `name` matches a `HandoffDef.tool_name` is present:

1. Take the **first** matching handoff call; **other tool calls that turn are ignored**
   (handoff wins — documented).
2. Emit `MessageOutput` for any assistant message and `ToolCallItem` for the transfer call.
3. Build the **threaded transcript** (§3.4).
4. Transition to `LoopState::ApplyingHandoff { target, transcript, usage: total }` (where
   `total = prior + usage`) and return `NextAction::Handoff`.

`ApplyingHandoff` gains a `usage: TokenUsage` field (it currently has only
`{ target, transcript }`). `NextAction` (already `#[non_exhaustive]`) gains a unit-ish
`Handoff` variant; the driver reads the payload from the `ApplyingHandoff` **state**,
mirroring how the existing `Terminate` arm reads `LoopState::Failed`.

The previous `(ApplyingHandoff, _) => not_implemented("handoff")` arm is **removed**;
`ApplyingHandoff` is now produced (with a payload) and consumed by the driver, never
re-entered by `transition`. `Compacting` / `NeedsApproval` keep their `not_implemented`
arms.

Handoff routing sits **ahead of** the structured-output finalizing path: a transfer is a
tool call, so it never reaches the no-tool-calls finalizing branch. Consequently an
`LlmAgent` configured with **both** `output_type` and `handoffs` (the Notion triage
example) finalizes its own `TriageDecision` only when it does *not* hand off; when it does
hand off, the run's terminal output is the **target's**. This is correct and intended.

### 3.4 Driver: nested delegation + transcript + usage (`agent.rs`)

The driver snapshots `let handoffs = self.handoffs.clone();` (cheap — `Vec<Handoff>` of
`Arc`s) alongside the existing `tools` snapshot, and builds the `Vec<HandoffDef>` (with the
collision check) before the stream starts.

**Transcript threading** (built wholly in `transition` from `ctx.conversation`, then
stored in `ApplyingHandoff.transcript` — the driver only forwards it): the target agent
injects its *own* instructions, so the threaded messages are the parent's accumulated
`conversation` with:

- the parent's **leading `System` item stripped** (avoids a double system prompt), and
- a **synthesized `ToolResult`** for the transfer `call_id` appended (content a short text
  ack, e.g. `"Transferred to <target>."`) so the dangling transfer `ToolCall` is satisfied
  — OpenAI rejects a tool call with no matching result.

These become the target's `AgentInput { messages }`.

**On `NextAction::Handoff`** the driver:

1. Reads `LoopState::ApplyingHandoff { target, transcript, usage }`.
2. Looks up the `Handoff` whose `agent.name() == target`. Missing → `RunFailed` +
   `failure.set(AgentError::Other(…))`, `return`. (Cannot normally happen — the def came
   from the same list — but handled defensively.)
3. Computes the child context `let child = ctx.handoff_child();` (§3.6). If
   `child.handoff_depth() > max_handoff_depth` → emit `RunFailed`,
   `failure.set(AgentError::HandoffDepthExceeded { depth, max })`, `return`.
4. Emits `AgentEvent::HandoffItem { from: agent_name, to: target }` then
   `AgentEvent::AgentUpdated { agent: target }`.
5. Runs `handoff.agent().run(child, AgentInput { messages: transcript }).await` — on
   `Err`, emit `RunFailed` + `failure.set(err)` and `return` (the yield-and-return pattern
   the driver already uses for `model.invoke` errors; `?` is unavailable inside the event
   stream). On `Ok(mut sub)`, forward events:
   - **suppresses the sub-run's `RunStarted`** (one `RunStarted` per logical run; the switch
     is already signalled by `AgentUpdated`),
   - **rewrites the sub-run's `RunCompleted { usage: sub }`** to
     `RunCompleted { usage: parent_usage + sub }` so the run total spans the whole chain
     (this is the SMA-402 "who pays" resolution: **accumulate across the chain**),
   - forwards every other event (including `TurnStarted`, deltas, `MessageOutput`,
     `RunFailed`) verbatim.
6. `return`s — the run is over; the sub-run's terminal event is the run's terminal event.

**Failure propagation across the boundary works for free:** `handoff_child()` shares the
same `FailureSlot` `Arc<Mutex<…>>` (§3.6), so a target that fails records its structured
`AgentError` into the slot the parent's `Runner`/`RunResultStreaming::collect` reads after
draining. No new plumbing needed.

The sub-run's `final_output` (last `MessageOutput`) is forwarded, so
`RunResultStreaming::collect`'s "last assistant message wins" rule already yields the
target's output as the run's `final_output`.

### 3.5 `AgentAsTool<Ctx>` (`src/agent_as_tool.rs`)

```rust
/// Adapter exposing any `Agent<Ctx>` as a `Tool<Ctx>`.
pub struct AgentAsTool<Ctx> {
    agent: std::sync::Arc<dyn crate::Agent<Ctx>>,
    name: String,
    description: String,
    schema: serde_json::Value,
}

impl<Ctx: Send + Sync + 'static> AgentAsTool<Ctx> {
    /// Wrap an agent; tool name + description default to the agent's.
    pub fn new(agent: impl crate::Agent<Ctx> + 'static) -> Self { … }
    pub fn shared(agent: std::sync::Arc<dyn crate::Agent<Ctx>>) -> Self { … }
    pub fn with_name(mut self, n: impl Into<String>) -> Self { … }
    pub fn with_description(mut self, d: impl Into<String>) -> Self { … }
}
```

- **Schema** (fixed, single string input):
  `{"type":"object","properties":{"input":{"type":"string","description":"…"}},"required":["input"]}`.
- **`Tool::invoke(&self, ctx: &ToolContext<Ctx>, args)`**:
  1. Parse `args["input"]` as the user text → `AgentInput::from_user_text(text)`. A missing
     / non-string `input` → `ToolError::InvalidArgs { schema_errors }` (the one recoverable
     `ToolError` per ADR-10 — the parent model gets one repair shot).
  2. Build an **isolated** sub-context via the public `RunContext::new`:
     - `user_ctx`   = `ctx.user_ctx().clone()` (shared application state — intended),
     - `session`    = `Arc::new(MemorySession::new())` (fresh; the sub-agent's turns do **not**
       pollute the parent session log),
     - `hooks`      = `HookRegistry::new()` (empty),
     - `tracer`     = `ctx.tracer().clone()` (sub-run spans nest under the parent's trace),
     - `cancel`     = `ctx.cancel().clone()` (already a child token of the run — parent
       cancellation propagates in; tool-local cancellation stays local).
     `handoff_depth` starts at 0 (a fresh `RunContext::new`).
  3. Run the agent to completion, draining its event stream and tracking the last
     `MessageOutput` text exactly as `RunResultStreaming::collect` does. A `RunFailed`
     (read from the sub-context's `FailureSlot`, else the event string) →
     `ToolError::Other(anyhow::Error::from(agent_error))`.
  4. Return `ToolOutput::new(serde_json::Value::String(final_output))`. The existing
     `tool_output_to_content_parts` turns a `Value::String` into one `ContentPart::Text`,
     so the round trip preserves the sub-agent's `final_output` verbatim — the acceptance
     criterion.

This is the central design point: `Tool::invoke` deliberately receives only `ToolContext`
(no session, no hooks — an explicit `tool.rs` invariant: "tools must not bypass the
runner's persistence"). `AgentAsTool` honors that invariant by **constructing** an isolated
`RunContext` from the `ToolContext` pieces rather than smuggling the parent's session/hooks
in. The sub-agent uses its **own** `RunConfig` (its `LlmAgent.config`), since
`RunContext::new` sets `run_config = None`.

### 3.6 `RunContext::handoff_child()` + depth (`context.rs`)

Handoff needs to hand the target a `RunContext`, but a handoff *continues the same logical
run* — so the child shares the parent's session, hooks, cancel token, failure slot, and
run config (it does **not** isolate, unlike `AgentAsTool`). Rather than a blanket `Clone`
(which would imply contexts are freely duplicable), add a single intentional method that
also carries the depth bump:

```rust
impl<Ctx> RunContext<Ctx> {
    /// A context for a handed-off sub-run: shares session, hooks, cancel,
    /// failure slot, and run config; `handoff_depth` is incremented by one.
    pub fn handoff_child(&self) -> Self { … }
    /// Number of handoffs that produced this context (0 for a top-level run).
    pub fn handoff_depth(&self) -> u32 { … }
}
```

Add a private `handoff_depth: u32` field (default 0 in `RunContext::new`). `HookRegistry`
gains a `Clone` impl (clones the `Vec<Arc<dyn Hook>>` — Arc clones, cheap) so the child can
copy the registry. `CancellationToken`, `TracerHandle`, `FailureSlot`, and the `Arc`s are
already cheaply cloneable; the failure-slot clone **shares** the underlying
`Arc<Mutex<…>>`, which is exactly what lets a failing target reach the parent's boundary.

`RunConfig` gains `max_handoff_depth: u32` (default **8**) + a `with_max_handoff_depth`
builder. `AgentError` (already `#[non_exhaustive]`) gains:

```rust
#[error("handoff depth ({depth}) exceeded max ({max})")]
HandoffDepthExceeded { depth: u32, max: u32 },
```

The guard prevents an A↔B transfer ping-pong from looping forever: each nested `run` resets
the per-agent turn counter, so `max_turns` alone cannot bound a cross-run cycle; the depth
counter does.

## 4. Public-API & module surface

`lib.rs` gains `pub mod handoff;` + `pub mod agent_as_tool;` and `pub use handoff::*;` +
`pub use agent_as_tool::*;` (same pattern as every other core module). The facade reaches
them as `paigasus_helikon::core::{Handoff, AgentAsTool, HandoffDef}` (the facade exposes
core only as `core::*`; no flatten — unchanged).

New public items: `Handoff<Ctx>`, `HandoffDef`, `AgentAsTool<Ctx>`,
`RunConfig::max_handoff_depth` + `with_max_handoff_depth`, `RunContext::handoff_child` +
`handoff_depth`, `AgentError::HandoffDepthExceeded`, `NextAction::Handoff`. Changed:
`LlmAgent.handoffs` / builder `.handoffs` bound; `LoopState::ApplyingHandoff` (+`usage`);
`TransitionCtx.handoffs`.

## 5. Release mechanics

This is a **breaking** change to `paigasus-helikon-core`'s public API:

- `LlmAgent.handoffs` field retype and `.handoffs<I>` bound change (§3.2);
- `LoopState::ApplyingHandoff` gains `usage` — breaks external exhaustive matches /
  construction (the variant is not `#[non_exhaustive]`, by the same deliberate choice as
  SMA-402 so `tests/transition_unit.rs` can construct it);
- `TransitionCtx` gains a field — breaks external struct construction (same test crate).

Additive (non-breaking): the new types, `NextAction::Handoff` (enum is `#[non_exhaustive]`
→ downstream matches already carry a wildcard), `AgentError::HandoffDepthExceeded`
(`#[non_exhaustive]`), `RunConfig::max_handoff_depth` (`#[non_exhaustive]`).

On a `0.x` crate a breaking change maps to a **minor** bump: `core 0.3.0 → 0.4.0`. Signal
it with a breaking-marked Conventional Commit — **`feat(core)!: SMA-324 …`** — so
`release-plz` proposes the minor bump and its `dependencies_update` cascade bumps the
facade (`0.2.4 → 0.2.5`) through the **normal** flow. Do **not** hand-bump versions: `core`
is an already-released crate (the manual core-/facade-bump ritual in `CLAUDE.md` applies
only to an ascending `0.0.0` stub using same-PR core API — not this case). During
implementation, confirm the release PR proposes `core 0.4.0` + the facade cascade, and
verify the actual current `core` version before assuming `0.3.0` is still the base. The PR
title must satisfy `pr-title.yml` (lowercase subject after `SMA-324 `; the `!` is allowed)
and `convco check` (`feat` + scope `core` are on the allowlist).

Realistic blast radius is near-zero: the only `transition`/`LoopState` consumers are
in-crate + the external `tests/transition_unit.rs`; the durable-runner crates that would
re-drive `transition` are empty `0.0.0` stubs.

## 6. Resolved judgment calls

- **D1 — Bounded recursion (included).** `max_handoff_depth` default 8 + the depth guard
  (§3.6). Cheap insurance against infinite transfer cycles that `max_turns` cannot catch.
- **D2 — `OnHandoff` hook deferred (out of scope).** Consistent with the current
  stored-but-not-driven state of *all* hooks; only the observable events are emitted now.
- **D3 — `Handoff` is a minimal wrapper.** No per-edge overrides this ticket; `Handoff<Ctx>`
  is the named extension point for them later.
- **D4 — Usage accumulates across the chain.** Resolves SMA-402 §3.5's open "who pays"
  question: the parent's pre-handoff usage is summed into the forwarded `RunCompleted`.

## 7. Testing (both core gate + facade example)

**Pure unit (`tests/transition_unit.rs`):**
- New branch test: `CallingModel` + `ModelResponse` containing a `transfer_to_mrd` tool
  call (with `handoffs = [mrd def]` in `TransitionCtx`) → `next_state ==
  ApplyingHandoff { target: "mrd", usage: total, .. }`, `next_action == Handoff`, and the
  threaded transcript drops the leading `System` and ends with the synthesized
  `ToolResult`.
- Precedence: a response with both a handoff call and a regular tool call → routes to
  `ApplyingHandoff` (handoff wins), regular call ignored.
- Existing exhaustive `assert_matches!`/constructions updated for the new
  `ApplyingHandoff.usage` field and `TransitionCtx.handoffs` field.

**Core integration — 3-agent triage (`tests/handoff.rs`):** triage + MRD + AML, each its
own `MockModel`. Triage scripts a `transfer_to_mrd_*` tool call; MRD scripts a final
answer; AML scripts nothing (must not run). Assert: exactly one `RunStarted`;
`HandoffItem { from: "triage", to: "mrd" }` then `AgentUpdated { agent: "mrd" }`;
`final_output` is MRD's; `RunCompleted.usage` == triage + MRD usage (chain sum); routing
went to MRD, **not** AML. Add a slug-collision case (two targets sluggable to the same
name) → terminal `RunFailed`. Add a depth-guard case (A↔B mutual handoff) → terminal
`HandoffDepthExceeded`.

**Core integration — `AgentAsTool` round-trip (`tests/agent_as_tool.rs`):** a parent
`LlmAgent` whose tool list contains an `AgentAsTool`-wrapped sub-agent; parent scripts a
call to the wrapped tool, then a final message. Assert the sub-agent's `final_output`
arrives as the `ToolResult` content the parent sees, the parent resumes and completes, and
the parent's `RunContext` session contains no sub-agent turns (isolation). Add a sub-agent
failure case → parent observes a tool error (recoverable repair path not required to
succeed; assert the error surfaces).

**Facade example (`crates/paigasus-helikon/examples/multi_agent_triage.rs`,
`required-features = ["openai"]`):** mirrors the Notion snippet — a triage `LlmAgent` with
`.handoffs([Handoff::to(mrd), Handoff::to(aml)])` over `openai`, runnable with an API key.
Compile-gated in CI; documents the user-facing surface.

Plus the standard gates: `cargo fmt --all`, `clippy --all-features --all-targets -D
warnings`, `test --workspace --all-features`, `RUSTDOCFLAGS=-D warnings cargo doc`, and the
doc-coverage threshold — every new public item carries a `///`.

## 8. Risks & mitigations

- **Double `RunStarted` / lost usage on the forwarded sub-stream.** Mitigated by explicitly
  suppressing the sub-run `RunStarted` and rewriting its `RunCompleted.usage` to the chain
  sum (§3.4), asserted in `tests/handoff.rs`.
- **Dangling transfer tool call breaks the next provider request.** Mitigated by
  synthesizing the `ToolResult` ack before threading (§3.4); covered by the transcript
  unit assertion.
- **Infinite transfer cycle.** Mitigated by the depth guard (§3.6), asserted by the A↔B
  test.
- **`AgentAsTool` leaking sub-agent chatter into the parent session.** Mitigated by the
  fresh `MemorySession` (§3.5), asserted by the isolation check.
- **Breaking API shipped at the wrong level.** Mitigated by the `feat(core)!:` title →
  minor bump via release-plz (§5); near-zero real blast radius.
- **Slug collision silently mis-routing.** Mitigated by the fail-fast collision check at
  run start (§3.1), asserted in `tests/handoff.rs`.
