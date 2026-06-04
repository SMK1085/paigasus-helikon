# SMA-325 — Workflow Agents: `SequentialAgent`, `ParallelAgent`, `LoopAgent`

**Status:** Design approved (brainstorming) — 2026-06-04
**Crate:** `paigasus-helikon-core`
**Linear:** SMA-325 (milestone *Composition & Extensibility*, `area:core`, `stage:2`)
**ADR:** ADR-11 *Single Agent trait subsumes LLM-driven and workflow agents*

## 1. Goal

Three deterministic orchestrators that implement the **same** `Agent<Ctx>` trait as
`LlmAgent`, so they compose uniformly (a workflow agent can be a sub-agent, a handoff
target, or wrapped by `AgentAsTool`):

- `SequentialAgent<Ctx>` — runs sub-agents in order, threading shared run state.
- `ParallelAgent<Ctx>` — runs sub-agents concurrently, merging results into disjoint state keys.
- `LoopAgent<Ctx>` — repeats sub-agents until a tool escalates or `max_iterations` is hit.

### Acceptance criteria (from the ticket)

1. Pipeline `Sequential([Parallel([fetchA, fetchB]), summarize])` runs in correct order.
2. A loop with `escalate` exits; without it, hitting `max_iterations` emits `RunFailed`.

## 2. How this reshapes the ticket's wording

The ticket predates the current `core` surface. Three phrases don't map to today's code; each
was resolved during brainstorming:

| Ticket says | Reality in `core` | Decision |
| --- | --- | --- |
| `escalate` returned from `ToolContext::actions` | `ToolContext` has no `actions` channel | **Build it** — faithful `EventActions`/`ActionsHandle` side-channel, mirroring the existing `FailureSlot` pattern. |
| `ParallelAgent` writes a "unique session-state key" | No key-value session state exists; `Session` is an append-only event log → `ConversationSnapshot` | **Add `SessionState`** — an in-memory, run-scoped KV scratchpad. **Not persisted** (persistence is a clean follow-up ticket). |
| `tokio::spawn`s sub-agents | `core` has no tokio runtime (only `tokio-util`, `futures-util`, `async-stream`) | **Use `futures-util`** (`select_all`), matching `run_tools_concurrent`. |

Two further design decisions taken during brainstorming:

- **Output threading is via `SessionState`**, not message rewriting. Each sub-agent receives the
  parent's *original* `AgentInput`; sub-agents coordinate through shared `state`. Downstream
  agents read upstream results in a dynamic `Instructions<Ctx>` closure (which already receives
  `&RunContext<Ctx>`).
- **`ParallelAgent` emits events live-interleaved** (`select_all`), not deterministically
  buffered. Lower latency / live multi-agent UX; intra-parallel event order is **not** byte-stable
  across runs, so parallel snapshot assertions normalize order. Cross-stage order (Parallel
  finishes before the next Sequential step) **is** deterministic.

## 3. New foundations

### 3.1 `SessionState` — run-scoped in-memory KV (`src/state.rs`)

A cloneable handle over `Arc<Mutex<HashMap<String, serde_json::Value>>>`. One logical store per
run; cloning shares it. Concurrency-safe; `ParallelAgent` branches write **disjoint** keys, so the
brief per-write lock never contends meaningfully.

```rust
pub struct SessionState(Arc<Mutex<HashMap<String, serde_json::Value>>>);

impl SessionState {
    pub fn new() -> Self;                                              // empty; also Default
    pub fn get(&self, key: &str) -> Option<serde_json::Value>;        // clones value out
    pub fn set(&self, key: impl Into<String>, value: impl Into<serde_json::Value>);
    pub fn contains_key(&self, key: &str) -> bool;
    pub fn keys(&self) -> Vec<String>;
}
```

Lock poisoning recovers in place (`unwrap_or_else(|e| e.into_inner())`), matching `FailureSlot`.
**Not persisted** to the `Session` log this ticket — purely a per-run scratchpad.

### 3.2 `EventActions` / `ActionsHandle` — control side-channel (`src/state.rs`)

The faithful port of ADK's `EventActions`. For this ticket it carries one signal, `escalate`, but
the struct is `#[non_exhaustive]` so it can grow (`skip_summarization`, `transfer_to_agent`, …).

```rust
#[non_exhaustive]
#[derive(Debug, Default, Clone)]
pub struct EventActions {
    pub escalate: bool,
}

/// Cloneable handle a tool uses to signal the enclosing driver.
pub struct ActionsHandle(Arc<Mutex<EventActions>>);

impl ActionsHandle {
    pub fn new() -> Self;                  // also Default
    pub fn escalate(&self);                // sets escalate = true
    pub fn is_escalated(&self) -> bool;
    pub fn snapshot(&self) -> EventActions; // clone-out for inspection
}
```

A tool calls `ctx.actions().escalate()`. `LoopAgent` reads `is_escalated()` after the relevant
child run drains — the same write-inside / read-after-drain discipline as `FailureSlot`.

### 3.3 Context wiring (`src/context.rs`, `src/tool.rs`)

Mirrors the `FailureSlot` precedent so nothing existing breaks:

- `RunContext<Ctx>` gains private `state: SessionState` and `actions: ActionsHandle`, with
  `state()` and `actions()` accessors. **`RunContext::new`'s 5-arg signature is unchanged** — both
  default to empty.
- `RunContext::to_tool_context()` projects `state.clone()` and `actions.clone()` into the
  `ToolContext`, which gains matching `state()` / `actions()` accessors. **`ToolContext::new` arity
  is unchanged**; the two new fields are set via additive consuming-builder methods
  `with_state(..)` / `with_actions(..)` that `to_tool_context` calls.
- New derivation method used by all three workflow agents:
  `RunContext::subagent_child(&self) -> Self` — **shares** `state` (the threading channel),
  `session`, `cancel`, `tracer`, `user_ctx`, `run_config`; gets a **fresh** `FailureSlot` and a
  **fresh** `ActionsHandle`; `agent_depth + 1`.
- `handoff_child()` additionally shares `state` and `actions` (a handoff continues the same logical
  run). `AgentAsTool` keeps its isolation contract — it constructs a fresh `RunContext::new`, hence
  fresh empty `state`/`actions`. No change to `AgentAsTool`.

All additive. **No new `AgentEvent` variants** ⇒ zero serde-fixture churn.

## 4. The three workflow agents (`src/workflow.rs`)

All `impl Agent<Ctx>`; all follow the **handoff-driver merge convention** already established in
`agent.rs` (the `NextAction::Handoff` arm):

- Emit the agent's own outer `RunStarted { agent: self.name }`.
- Before running each child, emit `AgentEvent::AgentUpdated { agent: <child name> }`.
- While draining a child stream: **swallow** the child's `RunStarted`; **intercept**
  `RunCompleted { usage }` to fold into a running total (do **not** re-emit it); **pass through**
  every other event (`TokenDelta`, `MessageOutput`, `ToolCallItem`, …).
- On a child `RunFailed { error }`: set the parent `FailureSlot` to the structured `AgentError`
  and stop (fail-fast), then emit one outer `RunFailed`.
- On success, emit the agent's own outer `RunCompleted { usage: <summed total> }`.
- Each child runs under `ctx.subagent_child()` and receives the parent's **original
  `AgentInput`** (cloned). Depth is checked against `RunConfig::max_agent_depth` before each child
  (as `AgentAsTool` does); exceeding it fails with `AgentError::MaxAgentDepthExceeded`.
- Each `run` is wrapped in an `invoke_agent`-style tracing span
  (`gen_ai.agent.name = self.name`), matching `LlmAgent`.

The stream is built with `async_stream::stream!`, snapshotting `self`'s sub-agents
(`Vec<Arc<dyn Agent<Ctx>>>` — cheap `Arc` clones) and moving `ctx` in, so the returned
`BoxStream<'static, AgentEvent>` outlives `&self` exactly like `LlmAgent::run`.

### 4.1 `SequentialAgent<Ctx>`

Runs sub-agents in registration order. Each sub-agent sees whatever prior sub-agents wrote to the
shared `state`. Fail-fast: the first child `RunFailed` aborts the remaining steps. Final
`RunCompleted` carries usage summed across all steps.

### 4.2 `ParallelAgent<Ctx>`

Branches are `(key: String, agent: Arc<dyn Agent<Ctx>>)`; the key defaults to the agent's name.
All branches:

- run under sibling `subagent_child()` contexts that **share the one `state` `Arc`** (writes go to
  disjoint keys → safe);
- receive the same original `AgentInput`;
- are merged with **`futures::stream::select_all`** and forwarded **live-interleaved**.

As each branch completes, `ParallelAgent` writes that branch's final-output text (the concatenated
text of its last `MessageOutput` assistant message) to `state[key]`. This realizes the ticket's
"each writes to a unique session-state key; results merged." Error semantics are **collect-all**:
siblings are allowed to finish; if any branch failed, one aggregate `RunFailed` is emitted and the
parent `FailureSlot` is set to the first branch error. (Fail-fast sibling cancellation via a child
`CancellationToken` is a noted follow-up, not in scope.)

> Concurrency note: `select_all` drives branches cooperatively on one task — sufficient for
> IO-bound model calls (each `model.invoke` await yields). True OS-thread parallelism would need
> `tokio::spawn`, which `core` cannot depend on; this is documented on the type.

### 4.3 `LoopAgent<Ctx>`

Holds sub-agents (run in order each iteration) and `max_iterations: u32`.

```text
for iteration in 0..max_iterations {
    for agent in &self.agents {
        let child = ctx.subagent_child();        // fresh ActionsHandle + FailureSlot, shared state
        emit AgentUpdated { agent: agent.name() }
        drive child stream (merge convention above; fold usage; fail-fast on RunFailed)
        if child.actions().is_escalated() {
            emit RunCompleted { usage: total };  // success exit
            return;
        }
    }
}
// max_iterations exhausted without escalate:
set FailureSlot = AgentError::MaxIterationsExceeded { max }
emit RunFailed
```

Escalate reaches the loop through the existing tool path: a tool inside a looped `LlmAgent` calls
`ctx.actions().escalate()`; `ctx` is the `ToolContext` projected from the child `RunContext` the
sub-agent runs under; after that sub-agent's stream drains, `LoopAgent` reads
`child.actions().is_escalated()`. The full chain works **without touching the `LlmAgent`
transition state machine**.

> Out of scope: making `escalate` *also* short-circuit an `LlmAgent`'s own turn loop mid-run
> (that would require the `transition` function to inspect `actions`). Here escalate is observed by
> `LoopAgent` after the sub-agent completes its run — sufficient for the acceptance criteria.

### 4.4 Acceptance criterion 1 walk-through

`Sequential([Parallel([fetchA, fetchB]), summarize])`:

1. `SequentialAgent` runs `ParallelAgent` first and **awaits its stream to completion** before
   starting `summarize` — so cross-stage order is deterministic.
2. `ParallelAgent` runs `fetchA`/`fetchB` concurrently; on completion it writes `state["fetchA"]`
   and `state["fetchB"]`.
3. `summarize`'s dynamic `Instructions` closure reads both keys to build its prompt.

Only the *interleave of events within the parallel block* is nondeterministic; the pipeline order
is not.

## 5. Construction API

Lightweight consume-self builders that accept `impl Agent<Ctx> + 'static` and `Arc`-wrap
internally:

```rust
let fetch = ParallelAgent::new("fetch", "Fetch A and B")
    .branch("a", fetch_a)              // explicit state key
    .branch("b", fetch_b);             // key defaults to agent name if .add(..) is used instead

let pipeline = SequentialAgent::new("pipeline", "Fetch then summarize")
    .then(fetch)
    .then(summarize);

let refine = LoopAgent::new("refine", "Draft then critique until good", 5)
    .then(drafter)
    .then(critic);                     // critic's tool escalates when the draft passes
```

- `SequentialAgent::new(name, description) -> Self`; `.then(impl Agent<Ctx> + 'static) -> Self`.
- `ParallelAgent::new(name, description) -> Self`; `.branch(key, agent) -> Self`;
  `.add(agent) -> Self` (key = `agent.name()`).
- `LoopAgent::new(name, description, max_iterations) -> Self`; `.then(agent) -> Self`.
- All three also accept a pre-wrapped `Arc<dyn Agent<Ctx>>` via `*_shared` variants for trait-object reuse.

## 6. Errors

- **Add** `AgentError::MaxIterationsExceeded { max: u32 }` — additive to the `#[non_exhaustive]`
  enum; `#[error("max iterations ({max}) exceeded")]`.
- **Reuse** `AgentError::MaxAgentDepthExceeded` for nesting depth.
- Fail-fast child errors propagate through the parent `FailureSlot`, so
  `RunResultStreaming::collect()` / `collect_typed()` surface the structured `AgentError` rather
  than the opaque `RunFailed` string.

## 7. Testing

Tools/helpers:

- A `MockAgent<Ctx>` test helper (in `tests/common/mod.rs` or a shared test module): emits a
  scripted event sequence and can optionally write `state` / call `actions().escalate()` via a
  configured closure. Lets workflow tests run without a real model.
- At least one **full-chain** integration test uses a real escalating `Tool` inside an `LlmAgent`
  driven by the existing scripted mock `Model`, to prove escalate travels
  tool → `ToolContext` → `LoopAgent`.

New integration test files:

- `tests/workflow_sequential.rs` — order; state threading; usage summation; fail-fast.
- `tests/workflow_parallel.rs` — both branches' events present (**order-normalized** assertions);
  both state keys written; summed usage; one-branch-fails aggregate error.
- `tests/workflow_loop.rs` — escalate stops after exactly N iterations (`RunCompleted`); no
  escalate ⇒ `RunFailed` with `MaxIterationsExceeded` surfaced via `collect()`.
- The acceptance pipeline `Sequential([Parallel([fetchA, fetchB]), summarize])` as an end-to-end
  test asserting cross-stage order and that `summarize` observed both state keys.
- Depth-bound test: nested workflow agents exceeding `max_agent_depth` ⇒ `RunFailed`.

Plus unit tests for `SessionState` (get/set/keys/disjoint concurrent writes) and `ActionsHandle`
(escalate/is_escalated/clone-shares-slot), styled after the existing `failure_slot_tests`.

## 8. Module layout & exports

- New `src/state.rs` — `SessionState`, `EventActions`, `ActionsHandle`. `lib.rs`:
  `pub mod state;` + `pub use state::*;`.
- New `src/workflow.rs` — `SequentialAgent`, `ParallelAgent`, `LoopAgent`. `lib.rs`:
  `pub mod workflow;` + `pub use workflow::*;`.
- `context.rs` / `tool.rs` edits as in §3.3; `agent.rs` adds the `AgentError` variant.
- Facade: new `core` types surface through the existing `pub use paigasus_helikon_core as core`
  (reachable as `paigasus_helikon::core::SequentialAgent`). The facade re-exports `core` under the
  `core` namespace (it does **not** flatten at the crate root), so **no facade edit is required**.
- Every new public item carries `///` docs to satisfy workspace `missing_docs` (`-D warnings` in
  the docs job) and the 80% doc-coverage gate. The CLI crate remains excluded from coverage.

## 9. Release

- `paigasus-helikon-core` `0.4.0` → `0.4.1` — additive surface ⇒ **patch** bump on a `0.x` crate.
  release-plz performs the bump and **cascades** the facade pin/version (no same-PR manual core
  bump, because no *other* crate in this PR consumes the new API).
- CHANGELOG: `Added — *(core)* SMA-325 add workflow agents (SequentialAgent / ParallelAgent /
  LoopAgent) + run-scoped SessionState and escalate actions`.
- Branch: `feature/sma-325-workflow-agents-sequentialagent-parallelagent-loopagent`. Design + plan
  docs land on this branch (not pre-merged to `main`).
- PR title (gates on the squashed commit): `feat(core): SMA-325 add workflow agents …` — full
  Conventional Commits prefix, lowercase subject after the `SMA-325 ` token.

## 10. Explicitly out of scope (future tickets)

- Persisting `SessionState` through the `Session` event log (durable-runner replay of state).
- `output_key` on `LlmAgent` (auto-write final output to a state key) — `ParallelAgent` owns
  key-writing this ticket, so `LlmAgent` stays untouched.
- `escalate` short-circuiting an `LlmAgent`'s own turn loop mid-run.
- Fail-fast sibling **cancellation** in `ParallelAgent` (collect-all this ticket).
- `SwarmAgent` / `GraphAgent` (named in the `agent.rs` module doc; separate tickets).
