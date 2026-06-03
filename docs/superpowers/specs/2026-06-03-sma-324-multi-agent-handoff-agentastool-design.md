# SMA-324 — Multi-agent: Handoff + `AgentAsTool`

**Status:** Design (approved)
**Issue:** [SMA-324](https://linear.app/smaschek/issue/SMA-324/multi-agent-handoff-agentastool)
**Branch:** `feature/sma-324-multi-agent-handoff-agentastool`
**Date:** 2026-06-03
**References:** [Multi-Agent Patterns](https://www.notion.so/355830e8fbaa81ff9b43d6e466d5fc14) (Notion). Resolves the handoff-usage seam flagged in [SMA-402](https://linear.app/smaschek/issue/SMA-402) §3.5.
**Domain:** personal finance, per the 2026-06-01 SMA-323 pivot off hematopathology. The Linear SMA-324 AC and the Notion "Multi-Agent Patterns" snippet were realigned to finance (budgeting / investing specialists) and the real `OpenAiModel::chat("gpt-5-mini").build()?` builder on 2026-06-03.

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
- Bounded agent-nesting recursion spanning **both** handoff chains and `AgentAsTool`
  nesting: a unified `RunContext.agent_depth` + `ToolContext.{agent_depth, max_agent_depth}`
  + `RunConfig.max_agent_depth` (default 8) + `AgentError::MaxAgentDepthExceeded`.
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
(`"Investing specialist"` → `transfer_to_investing_specialist`). The driver builds the
`Vec<HandoffDef>` once at run start and **fails fast** (terminal `RunFailed` + a structured
`AgentError::Other` naming the offenders) on **either** collision class:

- two handoff targets mapping to the same `transfer_to_*` `tool_name`; **or**
- a `transfer_to_*` `tool_name` that collides with a **real tool / `AgentAsTool`** name on
  the same agent — otherwise that tool would both pollute the model's schema and be
  **mis-routed as a handoff** by the routing branch (§3.3), which matches purely on name.

The `target` carried in events and used for lookup is always the **real** `agent.name()`,
never the slug.

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

**Handoff routing sits ahead of the structured-output finalizing path**, and composes
cleanly with `output_type` on a constrained provider:

- *Routing precedence.* A transfer is a tool call, so it routes here and never reaches the
  no-tool-calls finalizing branch. An `LlmAgent` with **both** `output_type` and `handoffs`
  (the finance triage) finalizes its own structured output only when it does *not* hand
  off; when it hands off, the run's terminal output is the **target's**.
- *Constrained-provider note (verified against source).* The output constraint is
  applied **only on the finalizing/repair turns** (`constrained_settings` in
  `loop_state.rs`), and on the `Start` arm only when `tools.is_empty()`. The Anthropic
  provider's synthesized forced-tool path keys **entirely off `model_settings.response_format`**
  (`translate/response_format.rs::synthesize_for_response_format`), which the loop leaves
  `None` on every turn that carries tools. So on the triage's normal turns the
  `transfer_to_*` tools are present and callable on **both** OpenAI and Anthropic — the
  structured-output constraint does **not** block the transfer tools here. Not a blocking
  dependency.
- *Dynamic post-handoff output type.* Because the terminal agent may differ from the
  parent, a run's `final_output` **type is dynamic**: `collect_typed::<T>()` is sound only
  if *every* reachable terminal agent produces `T`. A triage that routes to a free-text
  specialist would make `collect_typed::<TriageDecision>()` fail to deserialize.
  **Contract:** with `handoffs`, prefer `collect()` (string) or a parse-or-fallback; reserve
  `collect_typed::<T>()` for runs where all terminal agents share `T`. Documented on the
  `output_type` + handoff surface; the finance example uses `collect()`.

### 3.4 Driver: nested delegation + transcript + usage (`agent.rs`)

The driver snapshots `let handoffs = self.handoffs.clone();` (cheap — `Vec<Handoff>` of
`Arc`s) alongside the existing `tools` snapshot, and builds the `Vec<HandoffDef>` (with the
collision check) before the stream starts.

**Transcript threading** (built wholly in `transition` from `ctx.conversation`, then stored
in `ApplyingHandoff.transcript` — the driver only forwards it). The target injects its
*own* instructions and exposes only its *own* tools, so the threaded transcript must carry
**no tool references the target doesn't define**. Rule:

- strip the parent's **leading `System`** item (avoids a double system prompt);
- strip **all `Item::ToolCall` / `Item::ToolResult`** items — both the `transfer_to_*` call
  *and* any real tool calls the parent made before transferring. Threading them would leave
  `tool_use`/`tool_result` blocks referencing tools absent from the target's request, which
  providers reject (OpenAI requires a result per call; Anthropic rejects tool blocks for
  undefined tools). Stripping removes the hazard **by construction** — no per-provider
  verification needed;
- keep `UserMessage` and text `AssistantMessage` items;
- append a synthesized `Item::UserMessage` note (`"Transferred from <parent>."`) so the
  target has routing context (and the transcript is never empty when the parent's only
  output that turn was the transfer call).

These become the target's `AgentInput { messages }`. This v1 rule is deliberately lossy on
tool history (the triage→specialist AC routes on the user's question, so it is unaffected);
richer, configurable threading is the job of the **deferred handoff input-filter** (§6).

**On `NextAction::Handoff`** the driver:

1. Reads `LoopState::ApplyingHandoff { target, transcript, usage }`.
2. Looks up the `Handoff` whose `agent.name() == target`. Missing → `RunFailed` +
   `failure.set(AgentError::Other(…))`, `return`. (Cannot normally happen — the def came
   from the same list — but handled defensively.)
3. Computes the child context `let child = ctx.handoff_child();` (§3.6). If
   `child.agent_depth() > max_agent_depth` → emit `RunFailed`,
   `failure.set(AgentError::MaxAgentDepthExceeded { depth, max })`, `return`.
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
  2. **Depth guard.** If `ctx.agent_depth() + 1 > ctx.max_agent_depth()` →
     `ToolError::Other(AgentError::MaxAgentDepthExceeded { .. }.into())`. Bounds cyclic /
     deeply nested agent-as-tool graphs (A wraps B, B wraps A) with the **same** counter the
     handoff path uses (§3.6) — without this, the fresh sub-context would reset depth to 0
     and recurse on the call stack unbounded.
  3. Build an **isolated** sub-context via the public `RunContext::new`, then
     `.with_agent_depth(ctx.agent_depth() + 1)`:
     - `user_ctx`   = `ctx.user_ctx().clone()` (shared application state — intended),
     - `session`    = `Arc::new(MemorySession::new())` (fresh; the sub-agent's turns do **not**
       pollute the parent session log),
     - `hooks`      = `HookRegistry::new()` (empty),
     - `tracer`     = `ctx.tracer().clone()` (sub-run spans nest under the parent's trace),
     - `cancel`     = `ctx.cancel().clone()` (already a child token of the run — parent
       cancellation propagates in; tool-local cancellation stays local).
     The sub-agent uses its **own** `RunConfig` (`RunContext::new` sets `run_config = None`),
     so the parent's `timeout`/`max_turns` do **not** cross this boundary — only the shared
     `agent_depth` bound does. (Documented limitation.)
  4. Run the agent and **reuse the canonical drain**: capture
     `let fh = sub_ctx.failure_handle();` *before* moving `sub_ctx` into `run`, then
     `RunResultStreaming::with_failure(agent.run(sub_ctx, input).await?, fh).collect().await`
     — one definition of "stream → result", inheriting the post-drain `FailureSlot` read.
     Map `Err(RunError::Agent(e)) → ToolError::Other(e.into())` (and the outer `run` start
     error likewise).
  5. Return `ToolOutput::new(serde_json::Value::String(result.final_output))`. The existing
     `tool_output_to_content_parts` turns a `Value::String` into one `ContentPart::Text`,
     so the round trip preserves the sub-agent's `final_output` verbatim — the acceptance
     criterion.

This is the central design point: `Tool::invoke` deliberately receives only `ToolContext`
(no session, no hooks — an explicit `tool.rs` invariant: "tools must not bypass the
runner's persistence"). `AgentAsTool` honors that invariant by **constructing** an isolated
`RunContext` from the `ToolContext` pieces rather than smuggling the parent's session/hooks
in. The sub-agent uses its **own** `RunConfig` (its `LlmAgent.config`), since
`RunContext::new` sets `run_config = None`.

### 3.6 Bounded agent nesting: one `agent_depth` across handoff *and* `AgentAsTool` (`context.rs`, `tool.rs`)

`max_turns` bounds calls *per agent run* but cannot bound **nesting depth** across runs:
each nested `run` (a handoff target, or an `AgentAsTool` sub-run) resets the per-agent turn
counter. An A↔B handoff ping-pong, or a cyclic agent-as-tool graph (A wraps B, B wraps A),
recurses unbounded. A **single** nesting counter shared by both mechanisms closes both holes.

```rust
impl<Ctx> RunContext<Ctx> {
    /// A context for a handed-off sub-run: shares session, hooks, cancel,
    /// failure slot, and run config; `agent_depth` is incremented by one.
    pub fn handoff_child(&self) -> Self { … }
    /// Nesting depth that produced this context (0 for a top-level run).
    pub fn agent_depth(&self) -> u32 { … }
    /// Stamp an explicit nesting depth (used by `AgentAsTool` on its isolated sub-context).
    pub fn with_agent_depth(mut self, depth: u32) -> Self { … }
}
```

- **`RunContext`** gains a private `agent_depth: u32` (0 in `RunContext::new`).
  `handoff_child()` returns a context that **shares** session, hooks, cancel token, failure
  slot, and run config — a handoff *continues the same logical run* (unlike `AgentAsTool`) —
  with `agent_depth + 1`. Preferred over a blanket `Clone` (which would imply contexts are
  freely duplicable). `HookRegistry` gains a cheap `Clone` (clones `Vec<Arc<dyn Hook>>`); the
  failure-slot clone **shares** the underlying `Arc<Mutex<…>>` — exactly what lets a failing
  target reach the parent's boundary.
- **`ToolContext`** gains two scalars, `agent_depth: u32` and `max_agent_depth: u32`,
  projected by `RunContext::to_tool_context()` (from `self.agent_depth` and the effective
  `run_config.max_agent_depth`, defaulting when no runner installed a config). These are
  scalars, **not** the session/hooks/run_config — the documented "tools must not bypass
  persistence" invariant is untouched. `AgentAsTool::invoke` reads them for the §3.5 guard +
  the `with_agent_depth` stamp.
- **`RunConfig`** gains `max_agent_depth: u32` (default **8**) + `with_max_agent_depth`.
- **`AgentError`** (already `#[non_exhaustive]`) gains
  `MaxAgentDepthExceeded { depth: u32, max: u32 }`. On the handoff path the driver surfaces
  it as a terminal `RunFailed` (+ `failure.set`); inside `AgentAsTool` it surfaces as
  `ToolError::Other`.

So the same `agent_depth` is incremented whether the next level is a handoff or an
agent-as-tool call, and the same `max_agent_depth` bounds both.

## 4. Public-API & module surface

`lib.rs` gains `pub mod handoff;` + `pub mod agent_as_tool;` and `pub use handoff::*;` +
`pub use agent_as_tool::*;` (same pattern as every other core module). The facade reaches
them as `paigasus_helikon::core::{Handoff, AgentAsTool, HandoffDef}` (the facade exposes
core only as `core::*`; no flatten — unchanged).

New public items: `Handoff<Ctx>`, `HandoffDef`, `AgentAsTool<Ctx>`,
`RunConfig::max_agent_depth` + `with_max_agent_depth`, `RunContext::{handoff_child,
agent_depth, with_agent_depth}`, `ToolContext::{agent_depth, max_agent_depth}`,
`AgentError::MaxAgentDepthExceeded`, `NextAction::Handoff`. Changed: `LlmAgent.handoffs` /
builder `.handoffs` bound; `LoopState::ApplyingHandoff` (+`usage`); `TransitionCtx.handoffs`;
`ToolContext::new` signature (+ the two depth scalars).

## 5. Release mechanics

This is a **breaking** change to `paigasus-helikon-core`'s public API:

- `LlmAgent.handoffs` field retype and `.handoffs<I>` bound change (§3.2);
- `LoopState::ApplyingHandoff` gains `usage` — breaks external exhaustive matches /
  construction (the variant is not `#[non_exhaustive]`, by the same deliberate choice as
  SMA-402 so `tests/transition_unit.rs` can construct it);
- `TransitionCtx` gains a field — breaks external struct construction (same test crate);
- `ToolContext::new` gains two `u32` params (`agent_depth`, `max_agent_depth`) — breaks
  external callers that construct a `ToolContext` directly.

Additive (non-breaking): the new types, `NextAction::Handoff` (enum is `#[non_exhaustive]`
→ downstream matches already carry a wildcard), `AgentError::MaxAgentDepthExceeded`
(`#[non_exhaustive]`), `RunConfig::max_agent_depth` (`#[non_exhaustive]`).

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

- **D1 — Bounded recursion (included, unified).** One `agent_depth` + `max_agent_depth`
  (default 8) bounds **both** handoff chains and `AgentAsTool` nesting (§3.6) — symmetric
  across the two nesting mechanisms. Cheap insurance against infinite transfer cycles /
  cyclic agent-as-tool graphs that `max_turns` cannot catch.
- **D5 — Handoff transcript is tool-stripped.** v1 threads user + assistant-text + a
  synthesized transfer note, dropping all tool `Item`s, so no `tool_use`/`tool_result` for a
  tool the target doesn't define ever reaches a provider. Full/configurable history threading
  is the deferred input-filter feature.
- **D2 — `OnHandoff` hook deferred (out of scope).** Consistent with the current
  stored-but-not-driven state of *all* hooks; only the observable events are emitted now.
- **D3 — `Handoff` is a minimal wrapper.** No per-edge overrides this ticket; `Handoff<Ctx>`
  is the named extension point for them later.
- **D4 — Usage accumulates across the chain.** Resolves SMA-402 §3.5's open "who pays"
  question: the parent's pre-handoff usage is summed into the forwarded `RunCompleted`.

## 7. Testing (both core gate + facade example)

Domain: **personal finance**, per the 2026-06-01 pivot (SMA-323 + the Notion Side-by-Side)
— a **triage** agent routing to a **budgeting specialist** vs an **investing specialist**
(`transfer_to_budgeting_specialist` / `transfer_to_investing_specialist`).

**Pure unit (`tests/transition_unit.rs`):**
- New branch test: `CallingModel` + `ModelResponse` containing a
  `transfer_to_budgeting_specialist` tool call (with that `HandoffDef` in `TransitionCtx`)
  → `next_state == ApplyingHandoff { target: "budgeting specialist", usage: total, .. }`,
  `next_action == Handoff`, and the threaded transcript **drops the leading `System` and all
  tool `Item`s** and ends with the synthesized `"Transferred from <parent>."` `UserMessage`.
- Precedence: a response with both a handoff call and a regular tool call → routes to
  `ApplyingHandoff` (handoff wins), regular call ignored (documented drop).
- Existing exhaustive `assert_matches!`/constructions updated for the new
  `ApplyingHandoff.usage` and `TransitionCtx.handoffs` fields.

**Core integration — 3-agent triage (`tests/handoff.rs`):** triage + budgeting + investing,
each its own `MockModel`. Triage scripts a `transfer_to_budgeting_specialist` tool call;
budgeting scripts a final answer; investing scripts nothing (**must not run**). Assert:
exactly one `RunStarted`; `HandoffItem { from: "triage", to: "budgeting specialist" }` then
`AgentUpdated { agent: "budgeting specialist" }`; `final_output` is the budgeting agent's;
`RunCompleted.usage == triage + budgeting` (chain sum); routing went to budgeting, **not**
investing.
- Slug-vs-handoff collision (two targets → same slug) → terminal `RunFailed`.
- Slug-vs-real-tool collision (a tool named `transfer_to_*`) → terminal `RunFailed`.
- Depth guard: A↔B mutual handoff → terminal `MaxAgentDepthExceeded`.

**Core integration — `AgentAsTool` round-trip (`tests/agent_as_tool.rs`):** a parent
`LlmAgent` whose tool list contains an `AgentAsTool`-wrapped sub-agent; parent scripts a
call to the wrapped tool, then a final message. Assert the sub-agent's `final_output`
arrives as the `ToolResult` content the parent sees, the parent resumes and completes, and
the parent session contains no sub-agent turns (isolation). Add a sub-agent failure case →
parent observes a `ToolError`. Add an agent-as-tool **depth** case (A wraps B, B wraps A) →
the inner `invoke` returns `MaxAgentDepthExceeded`.

**Facade example (`crates/paigasus-helikon/examples/multi_agent_triage.rs`,
`required-features = ["openai"]`):** a finance triage `LlmAgent` with
`.handoffs([Handoff::to(budgeting), Handoff::to(investing)])` over `openai`, built with the
**real** API `OpenAiModel::chat("gpt-5-mini").build()?` (not the Notion snippet's fictional
`openai::gpt_5_mini()`) and consumed with `collect()` (string — per the §3.3 post-handoff output contract).
Compile-gated in CI; runnable with an API key.

Plus the standard gates: `cargo fmt --all`, `clippy --all-features --all-targets -D
warnings`, `test --workspace --all-features`, `RUSTDOCFLAGS=-D warnings cargo doc`, and the
doc-coverage threshold — every new public item carries a `///`.

## 8. Risks & mitigations

- **Double `RunStarted` / lost usage on the forwarded sub-stream.** Mitigated by explicitly
  suppressing the sub-run `RunStarted` and rewriting its `RunCompleted.usage` to the chain
  sum (§3.4), asserted in `tests/handoff.rs`.
- **Tool blocks for tools the target doesn't define break the next provider request.**
  Mitigated by **stripping all tool `Item`s** from the threaded transcript (§3.4) —
  removes the hazard by construction (no per-provider verification needed); asserted by the
  transcript unit test. Trade-off: v1 handoff loses parent tool-history; the deferred
  input-filter is the configurable fix.
- **Unbounded nesting — transfer cycles *or* cyclic agent-as-tool graphs**. Mitigated
  by the unified `agent_depth`/`max_agent_depth` guard spanning both mechanisms (§3.6),
  asserted by the A↔B handoff test and the A-wraps-B-wraps-A agent-as-tool test.
- **`AgentAsTool` leaking sub-agent chatter into the parent session.** Mitigated by the
  fresh `MemorySession` (§3.5), asserted by the isolation check.
- **`output_type` + `handoffs` blocking transfer tools on Anthropic**. Verified a
  non-issue: the constraint is finalizing-turn-only and the provider keys off
  `response_format` (§3.3) — documented as a provider note; no code mitigation needed.
- **Caller `collect_typed::<T>()` on a handed-off run deserialize-fails**. Mitigated
  by the documented dynamic-output contract (§3.3): prefer `collect()` with handoffs.
- **Breaking API shipped at the wrong level.** Mitigated by the `feat(core)!:` title →
  minor bump via release-plz (§5); near-zero real blast radius.
- **Slug collision silently mis-routing** (incl. handoff-vs-real-tool). Mitigated by the
  fail-fast collision check at run start (§3.1), asserted in `tests/handoff.rs`.
