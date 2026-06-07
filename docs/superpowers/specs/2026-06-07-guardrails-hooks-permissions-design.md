# SMA-326 — Guardrails, Hooks & PermissionPolicy

**Status:** Design approved (brainstorming) — 2026-06-07
**Crate:** `paigasus-helikon-core`
**Linear:** SMA-326 (milestone *Composition & Extensibility*, `area:core`, `stage:2`, High)
**Notion:** [Permissions, Guardrails & Hooks](https://www.notion.so/355830e8fbaa8132a1f9ef0ac5759f82)

## 1. Goal

Three **independent** control layers, all woven into the existing agent-loop driver:

- **Permissions** — *authorization*. Gate each tool call: `deny rules › permission mode › canUseTool` policy.
- **Guardrails** — *content*. Validate input/output text; a tripwire halts the run.
- **Hooks** — *observation & side effects*. Intercept lifecycle events; a typed decision can deny, replace, or inject.

Keeping them separate means a permission policy can be data-driven, guardrails stay simple, and hooks plumb into tracing — without conflating responsibilities.

### Acceptance criteria (from the ticket)

1. A failing **input guardrail** aborts the run **before any model call**.
2. A `PreToolUse` hook returning **replace-input** modifies the tool invocation.
3. **`Plan` mode rejects all side-effecting tool calls.**

### Scope decision

All three layers ship in **one spec, one PR**. They share a single integration point (the driver), so splitting them would re-touch the same code three times.

## 2. Starting point — what already exists

Earlier tickets pre-defined the trait scaffolding, all explicitly *"stored but not driven."* This ticket **drives** them.

| Already present | File | This ticket |
| --- | --- | --- |
| `Guardrail<Ctx>`, `GuardrailInput {UserText, ModelOutput}`, `GuardrailVerdict {Pass, Tripwire}`, `GuardrailKind`, `GuardrailError` | `guardrail.rs` | drive them (blocking gate) |
| `Agent.input_guardrails` / `output_guardrails` (Vec) + builder setters | `agent.rs`, `agent_builder.rs` | consume them |
| `Hook<Ctx>`, `HookEvent`, `HookDecision {Allow, Deny, ReplaceInput, ReplaceOutput, InjectSystemMessage}` | `hook.rs` | drive them; add `OnSubagentStop` |
| `Agent.hooks` (agent-level) + `HookRegistry<Ctx>` on `RunContext` (run-level) | `agent.rs`, `context.rs` | fire **both**, agent-level first |
| `AgentEvent::GuardrailTriggered`, `AgentEvent::ApprovalRequested {call_id, tool, args}` | `agent.rs` | emit them |
| `AgentError::Guardrail {kind}` | `agent.rs` | produce it |
| `LoopState::NeedsApproval {pending}` ("not driveable in SMA-314") | `loop_state.rs` | **left as-is** — durable-runner seam (see §7) |
| `PermissionPolicy` / `PermissionMode` / `PermissionDecision` | — | **net-new** (`permission.rs`) |

### The central constraint

`transition()` (`loop_state.rs`) is a **pure, synchronous** state machine — no async, no IO. Guardrails, hooks, and permission checks are all `async`, so they **cannot** live inside `transition()`. They are woven into the **driver** (`Agent::run`'s `async_stream`), the same seam every existing side effect (model call, tool execution, handoff) already uses. **Net change to `loop_state.rs`: none.**

## 3. Design decisions (locked during brainstorming)

| # | Decision | Choice | Rationale |
| --- | --- | --- | --- |
| 1 | Guardrail timing | **Blocking gate** | Satisfies AC1 literally; no cancellation machinery. Trait unchanged ⇒ optimistic execution can be added later without an API break. |
| 2 | Permission config location | **Both mode + policy on `RunContext`** | `RunContext` is the only thing cloned into sub-runs, so `Bypass` propagation + non-override is enforceable in one place. Matches Claude Agent SDK (permissions are a session concern). |
| 3 | Tool effect classification | **`Tool::effect() -> ToolEffect`** | Additive default method (non-breaking); makes Plan's guarantee a **core** invariant; serves all four modes. |
| 4 | `AskUser` handling | **`ApprovalRequested` event + optional `ApprovalHandler`, default Deny** | Driveable now, clean default, no durable suspend/resume. Interactive runners install a real handler later. |
| 5 | Code organization | **Extract `permission.rs` + `control.rs` (`Interceptors`)** | The per-call hook+permission interleaving is the trickiest logic — it deserves to be a unit-tested unit, not buried in a `stream!` macro. |

## 4. Public API surface

Everything is **additive**. `RunContext::new`'s signature and the published `0.x` API stay intact. The common case (no config, no guardrails, no hooks) behaves exactly as today: `Default` mode + no policy ⇒ Allow all.

### 4.1 New module `permission.rs`

```rust
/// How permission mode governs tool calls. `Bypass` propagates to subagents
/// and cannot be overridden — a typed enum, not a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum PermissionMode {
    #[default]
    Default,      // ask for unfamiliar tools (via policy); permissive when no policy
    AcceptEdits,  // auto-approve Write-effect tools
    Plan,         // read-only — deny any tool whose effect != ReadOnly
    Bypass,       // dangerous; allow all (deny rules still apply); propagates, sticky
}

/// The outcome of a permission check.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PermissionDecision {
    Allow,
    Deny { reason: String },
    AskUser { prompt: String },
    Replace { args: serde_json::Value },   // sanitize args before execution
}

#[async_trait::async_trait]
pub trait PermissionPolicy<Ctx>: Send + Sync
where Ctx: Send + Sync + 'static {
    async fn check(
        &self,
        ctx: &RunContext<Ctx>,
        tool: &str,
        args: &serde_json::Value,
    ) -> PermissionDecision;
}

/// A first-class deny rule, evaluated BEFORE mode — so it overrides even `Bypass`.
/// v1: exact tool-name match. `#[non_exhaustive]`; richer matchers (glob/predicate)
/// are an additive follow-up.
#[derive(Debug, Clone)]
pub struct DenyRule { /* tool: String */ }
impl DenyRule {
    pub fn tool(name: impl Into<String>) -> Self;
    pub fn matches(&self, tool: &str, args: &serde_json::Value) -> bool;
}

/// Resolves `AskUser` when the driver can't decide inline. Non-generic — needs no Ctx.
#[async_trait::async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn decide(&self, tool: &str, prompt: &str, args: &serde_json::Value)
        -> ApprovalOutcome;
}

/// A narrowed decision — an approval handler cannot recursively `AskUser`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ApprovalOutcome { Allow, Deny { reason: String } }
```

### 4.2 `Tool<Ctx>` gains an effect classification (`tool.rs`)

```rust
/// A tool's side-effect profile. Drives `Plan`/`AcceptEdits` mode decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ToolEffect {
    ReadOnly,           // no side effects; safe in Plan mode
    Write,              // mutates local/filesystem state; auto-approved by AcceptEdits
    #[default]
    SideEffect,         // anything else (network, external); safe-by-default
}

pub trait Tool<Ctx> {
    // ... existing methods ...
    /// This tool's side-effect profile. Default `SideEffect` (safe-by-default):
    /// an undeclared tool is treated as side-effecting, so `Plan` mode blocks it.
    fn effect(&self) -> ToolEffect { ToolEffect::SideEffect }
}
```

### 4.3 `RunContext<Ctx>` gains permission config (`context.rs`)

Four new fields, all defaulted; `new()` is unchanged. Consuming `with_*` setters mirror the existing `with_run_config` pattern.

```rust
pub struct RunContext<Ctx> {
    // ... existing fields ...
    permission_mode: PermissionMode,                          // default: Default
    permission_policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,// default: None
    deny_rules: Vec<DenyRule>,                                // default: empty
    approval_handler: Option<Arc<dyn ApprovalHandler>>,       // default: None
}

impl<Ctx> RunContext<Ctx> {
    pub fn with_permission_mode(self, mode: PermissionMode) -> Self;
    pub fn with_permission_policy(self, p: Arc<dyn PermissionPolicy<Ctx>>) -> Self;
    pub fn with_deny_rules(self, rules: Vec<DenyRule>) -> Self;
    pub fn with_approval_handler(self, h: Arc<dyn ApprovalHandler>) -> Self;
    pub fn permission_mode(&self) -> PermissionMode;
    // ... borrows for policy / deny_rules / approval_handler ...
}
```

**Propagation** — `handoff_child()` and `subagent_child()` clone all four. `permission_mode` is cloned verbatim; because there is **no per-agent mode-override API**, `Bypass` is automatically sticky — a child cannot lower it. (The invariant holds by construction: the only mechanism that sets a child's mode is the clone.)

### 4.4 `ToolContext<Ctx>` carries permission config (the agent-as-tool fix) (`tool.rs`)

`agent_as_tool` runs inside `Tool::invoke`, which only receives a `ToolContext` — today carrying **no** permission config. Handoff and workflow sub-runs propagate via `RunContext` children, but an agent-as-tool sub-run would **silently drop the mode**, violating "`Bypass` propagates to subagents." Fix: project the permission config into `ToolContext` so `agent_as_tool` rebuilds its sub-`RunContext` with it.

```rust
// RunContext::to_tool_context() additionally projects mode + policy + deny_rules + handler.
// agent_as_tool's sub_ctx (currently RunContext::new(...).with_agent_depth(d+1)) gains:
//   .with_permission_mode(tool_ctx.permission_mode())
//   .with_permission_policy(...) .with_deny_rules(...) .with_approval_handler(...)
```

`agent_as_tool` keeps its **session + hooks isolation** (fresh `MemorySession`, empty `HookRegistry`); only permission config rides through — the security invariant trumps isolation.

### 4.5 `HookEvent` gains `OnSubagentStop` (`hook.rs`)

```rust
#[non_exhaustive]
pub enum HookEvent {
    // ... existing: OnRunStart, OnTurnStart, PreToolUse, PostToolUse, OnHandoff, OnRunComplete ...
    /// Fired when a subagent sub-run (handoff target or agent-as-tool) completes.
    OnSubagentStop { agent: String },
}
```

### 4.6 New events & errors (`agent.rs`)

```rust
// AgentEvent (additive):
PermissionDenied { tool: String, reason: String },   // observability; model gets denial via ToolResult

// AgentError (additive):
#[error("hook denied {event}: {reason}")]
HookDenied { event: String, reason: String },         // dedicated public error for a lifecycle Deny
```

`AgentEvent::ApprovalRequested` already exists and is reused as-is (`{call_id, tool, args}`); the `AskUser` prompt is routed to the `ApprovalHandler` directly rather than added to the event.

## 5. Control-layer semantics (`control.rs`)

`Interceptors<'a, Ctx>` borrows the agent's `&[Guardrail]` / `&[Hook]` and the context's policy/mode/deny_rules/handler, exposing the four async seam methods the driver calls. This is the **unit-tested** unit.

### 5.1 Permission pipeline (`authorize`) — deny rules › mode › policy

Evaluated per tool call, on the **effective** args (after any PreToolUse replace):

```
1. deny_rules match?            → Deny           (absolute — overrides even Bypass)
2. mode:
     Bypass                     → Allow          (skip policy; deny rules already applied)
     Plan   & effect != ReadOnly→ Deny
     AcceptEdits & effect==Write→ Allow           (skip policy)
     otherwise                  → fall through
3. policy.check(ctx, tool, args)?
     None                       → Allow           (permissive when unconfigured)
     Some → Allow | Deny{reason} | Replace{args} | AskUser{prompt}
4. AskUser → emit ApprovalRequested; approval_handler.decide(...)?
     None                       → Deny            (safe default)
     Some                       → Allow | Deny
```

A `Deny`/`Replace` outcome **never aborts the run** — a denied call yields a synthetic `ToolResult` carrying the reason (plus a `PermissionDenied` event); the model sees it and continues. This is what keeps `transition()` untouched.

### 5.2 Hook firing & conflict resolution (`fire`)

Hooks run **agent-level first, then `RunContext` registry** (run-global). Outcomes fold into one `ResolvedHookDecision`:

- **`Deny` short-circuits** — first `Deny` wins; remaining hooks skipped.
- **`ReplaceInput` / `ReplaceOutput`** — applied in order; **last writer wins** for the final value.
- **`InjectSystemMessage`** — **accumulate** all; appended to the conversation (as `Item::System`) before the next model call.
- **`Allow`** — no-op.

Per event type:

| Event | Honors | Deny means |
| --- | --- | --- |
| `PreToolUse` | Deny, ReplaceInput | denial `ToolResult`; skip permission + invoke |
| `PostToolUse` | ReplaceOutput | convert the result into a denial message to the model |
| `OnRunStart` / `OnTurnStart` / `OnHandoff` | Deny, InjectSystemMessage | abort run (`AgentError::HookDenied`) |
| `OnSubagentStop` / `OnRunComplete` | (observational) | ignored |

`Replace*` are ignored on pure lifecycle events (nothing to replace).

### 5.3 Guardrails (blocking gate)

- **Input** — after `OnRunStart`, before the loop, on `GuardrailInput::UserText(seed)`. Tripwire ⇒ `GuardrailTriggered` + `RunFailed`, `failure.set(AgentError::Guardrail{kind})`, return — **zero model calls** (AC1).
- **Output** — when the driver detects `next_state == Done`, run on the final text **before** yielding the bundled `RunCompleted`. Tripwire ⇒ suppress `RunCompleted`, emit `GuardrailTriggered` + `RunFailed`.
- Agent-level: each nested agent runs its **own** input guardrails at the start of its run.

### 5.4 Per-tool-call interleaving (inside `run_tools_concurrent`)

```
PreToolUse hook → [Deny ⇒ denial result] → effective args
  → authorize(args) → [Deny ⇒ denial result] [Replace ⇒ sanitized args]
  → tool.invoke(args)
  → PostToolUse hook → effective output
  → ToolResult
```

Each call's pipeline runs as a unit under the existing `parallel_tool_call_limit`. `run_tools_concurrent` gains a `&Interceptors<'_, Ctx>` borrow.

## 6. Driver integration & data flow (`agent.rs`)

All changes land in `Agent::run`'s `async_stream`. Locations are approximate line regions in today's file.

| Seam | Location | Added behavior |
| --- | --- | --- |
| `OnRunStart` | after `yield RunStarted` (~654) | `fire(OnRunStart)`; Deny aborts; queue injections |
| Input guardrails | before `loop {` (~676) | blocking gate on seed user text; trip ⇒ fail, return |
| `OnTurnStart` | `TurnStarted` already matched for tracing (~691) | `fire(OnTurnStart{turn})`; injections queued |
| Inject system msgs | before `CallModel` builds its request (~729) | drain queued injections into `conversation` as `Item::System` |
| Tool gating | `NextAction::ExecuteTools` arm (~849) | `run_tools_concurrent(&interceptors, ...)` runs §5.4 |
| Output guardrails | detect `next_state == Done` before yielding its events (~688) | gate `RunCompleted`; trip ⇒ rewrite to failure |
| `OnHandoff` | `NextAction::Handoff` arm, before running target (~920) | `fire(OnHandoff{from,to})`; Deny aborts handoff |
| `OnSubagentStop` | after handoff sub-stream drains (~951); in `agent_as_tool` after its sub-run | `fire(OnSubagentStop{agent})` (observational) |
| `OnRunComplete` | `NextAction::Terminate` arm (~870) | `fire(OnRunComplete)` (observational) before return |

**The one subtlety — output guardrails vs. the bundled `RunCompleted`.** `transition()` packs `MessageOutput` + `RunCompleted` into one events vec for the `Done` arm. So before the normal yield loop, the driver checks `if let LoopState::Done(out) = &next_state`: it runs output guardrails on `out.as_text()` first; on a tripwire it rewrites the outcome's terminal events to `GuardrailTriggered + RunFailed` and `failure.set(...)`. `transition()` itself stays pure and unchanged.

**Propagation map (the `Bypass` invariant, made universal):**

```
top-level RunContext{ mode, policy, deny_rules, handler }
   ├─ handoff_child()   → clones all four (mode Bypass-sticky)        ✓ RunContext path
   ├─ subagent_child()  → clones all four (Seq/Parallel/Loop)         ✓ RunContext path
   └─ agent_as_tool     → ToolContext now carries them →
                          sub RunContext rebuilt with with_* setters  ✓ the fix (§4.4)
```

## 7. What is explicitly NOT in scope

- **Durable suspend/resume for approval.** `LoopState::NeedsApproval` is left undriven, the documented seam for `paigasus-helikon-runtime-temporal` / `-agentcore` to implement true human-in-the-loop in later tickets. v1 resolves `AskUser` inline via `ApprovalHandler` (default Deny).
- **Optimistic (parallel) guardrail execution.** v1 is a blocking gate; the `Guardrail` trait is unchanged so this is a future, non-breaking addition.
- **Persisted permission state / allowlists.** "Unfamiliar tool" tracking (Default mode) is delegated to a user-supplied policy, not built into core.
- **Rich `DenyRule` matchers** (glob, predicate, arg-aware). v1 is exact tool-name; `#[non_exhaustive]` keeps it extensible.

## 8. Testing strategy (TDD)

**Unit tests on `control.rs` / `permission.rs`** — high-value, no model needed:

- Permission pipeline truth table: deny-rule beats `Bypass`; `Plan` denies `SideEffect`/`Write`, allows `ReadOnly`; `AcceptEdits` allows `Write`; `Default`+no-policy allows; `AskUser`+no-handler denies; `Replace` sanitizes args.
- Hook conflict resolution: first-`Deny`-wins short-circuit; last-`ReplaceInput`-wins; multiple `InjectSystemMessage` accumulate; agent-before-registry order.
- `ToolEffect` default is `SideEffect`.

**Driver integration tests** (mock model + mock tools, existing test style):

- **AC1** — failing input guardrail ⇒ run aborts with **zero** model invocations (assert the mock model's call count is 0).
- **AC2** — `PreToolUse` returning replace-input ⇒ the tool receives replaced args (recording tool).
- **AC3** — `Plan` mode ⇒ every non-`ReadOnly` tool call denied (model sees denial `ToolResult`s).
- Output-guardrail tripwire suppresses `RunCompleted`, yields `RunFailed`.
- `Bypass` propagation across **all three** sub-run paths — including agent-as-tool (regression test for the `ToolContext` fix).
- `AskUser` + installed handler returning `Allow`/`Deny` resolves correctly; `ApprovalRequested` emitted.

**Gates:** every new `pub` item gets a `///` doc with a doctest at entry points (the crate runs `RUSTDOCFLAGS=-D warnings` + 80% doc-coverage). New code follows the workspace `#[non_exhaustive]` + `missing_docs` conventions.

## 9. File-by-file change summary

| File | Change |
| --- | --- |
| `permission.rs` | **new** — `PermissionMode`, `PermissionDecision`, `PermissionPolicy`, `DenyRule`, `ApprovalHandler`, `ApprovalOutcome` |
| `control.rs` | **new** — `Interceptors<'a, Ctx>` + `ResolvedHookDecision`; the four seam methods |
| `tool.rs` | `ToolEffect` enum; `Tool::effect()` default method; `ToolContext` carries permission config + accessors |
| `context.rs` | `RunContext` permission fields + `with_*` setters; propagate in `handoff_child`/`subagent_child`; project in `to_tool_context` |
| `hook.rs` | add `HookEvent::OnSubagentStop` |
| `agent.rs` | drive all seams in `Agent::run`; `run_tools_concurrent` takes `&Interceptors`; add `AgentEvent::PermissionDenied`, `AgentError::HookDenied` |
| `agent_as_tool.rs` | rebuild sub-`RunContext` with inherited permission config; fire `OnSubagentStop` |
| `lib.rs` | `pub mod permission; pub mod control;` + re-exports |
| `loop_state.rs` | **no change** (pure state machine stays pure) |
