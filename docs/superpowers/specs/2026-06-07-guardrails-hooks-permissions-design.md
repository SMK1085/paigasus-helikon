# SMA-326 — Guardrails, Hooks & PermissionPolicy

**Status:** Design approved (brainstorming) — 2026-06-07; **revised 2026-06-07** after a staff-level design review (dispositions folded in; see §3 decisions 6–9)
**Crate:** `paigasus-helikon-core` (+ `paigasus-helikon-macros`, see §4.2/§4.7)
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

All three layers ship in **one spec, one PR** (their shared integration point is the driver). The PR also touches the published `paigasus-helikon-macros` crate (§4.7) and `core/src/workflow.rs` (§6, `OnSubagentStop`). Seam-wiring is sequenced so each AC goes green independently (§8).

## 2. Starting point — what already exists

Earlier tickets pre-defined the trait scaffolding, all explicitly *"stored but not driven."* This ticket **drives** them. (All claims below verified against `core` source at `0.4.0`, post-SMA-325.)

| Already present | File | This ticket |
| --- | --- | --- |
| `Guardrail<Ctx>`, `GuardrailInput {UserText, ModelOutput}`, `GuardrailVerdict {Pass, Tripwire}`, `GuardrailKind`, `GuardrailError` | `guardrail.rs` | drive them (blocking gate) |
| `Agent.input_guardrails` / `output_guardrails` (Vec) + builder setters | `agent.rs`, `agent_builder.rs` | consume them (Arc-snapshot, §4.6) |
| `Hook<Ctx>`, `HookEvent`, `HookDecision {Allow, Deny, ReplaceInput, ReplaceOutput, InjectSystemMessage}` | `hook.rs` | drive them; add `OnSubagentStop` |
| `Agent.hooks` (agent-level) + `HookRegistry<Ctx>` on `RunContext` (run-level) | `agent.rs`, `context.rs` | fire **both**, agent-level first |
| `AgentEvent::GuardrailTriggered`, `AgentEvent::ApprovalRequested {call_id, tool, args}` | `agent.rs` | emit them |
| `AgentError::Guardrail {kind}` | `agent.rs` | produce it |
| `LoopState::NeedsApproval {pending}` (`not_implemented("approval")`) | `loop_state.rs` | **left as-is** — durable-runner seam (§7) |
| `FinalOutput::as_text()` | `loop_state.rs` | used by the output-guardrail gate (§5.3) |
| `PermissionPolicy` / `PermissionMode` / `PermissionDecision` | — | **net-new** (`permission.rs`) |

### The central constraint

`transition()` (`loop_state.rs`) is a **pure, synchronous** state machine — no async, no IO. Guardrails, hooks, and permission checks are all `async`, so they **cannot** live inside `transition()`. They are woven into the **driver** (`Agent::run`'s `async_stream`), the same seam every existing side effect (model call, tool execution, handoff) already uses. **Net change to `loop_state.rs`: none.**

## 3. Design decisions

Brainstorming decisions (1–5) plus review dispositions (6–9).

| # | Decision | Choice | Rationale |
| --- | --- | --- | --- |
| 1 | Guardrail timing | **Blocking gate** | Input guardrails are blocking **by necessity of AC1**: a gating input check cannot let a model call precede it (see §5.3). Optimistic/parallel execution — which the ticket + Notion mention — is mutually exclusive with AC1 for *input* guardrails, so it is deferred as a future, non-gating latency optimization. The `Guardrail` trait is unchanged ⇒ additive later. |
| 2 | Permission config location | **Both mode + policy on `RunContext`** | `RunContext` is the only thing cloned into sub-runs, so `Bypass` propagation + non-override is enforceable in one place. Matches Claude Agent SDK. |
| 3 | Tool effect classification | **`Tool::effect() -> ToolEffect`** | Additive default method (non-breaking); makes Plan's guarantee a **core** invariant; serves all four modes. |
| 4 | `AskUser` handling | **`ApprovalRequested` event + optional `ApprovalHandler`, default Deny** | Driveable now, clean default, no durable suspend/resume. |
| 5 | Code organization | **Extract `permission.rs` + `control.rs` (`Interceptors`)** | The per-call hook+permission interleaving deserves to be a unit-tested unit, not buried in a `stream!` macro. |
| 6 | `#[tool]` macro effect (H1) | **Extend the macro now** with `effect = read_only \| write \| side_effect` | Without it, every macro-authored tool is `SideEffect`, so `Plan` denies all macro tools and `AcceptEdits` approves none — two of four modes inert on the primary tool path. A `feat(macros)` minor bump rides along (§4.7). |
| 7 | `OnSubagentStop` coverage (M3) | **Fire on every sub-run** | Uniform contract: handoff + agent-as-tool + **workflow** (Sequential/Parallel/Loop). Adds `workflow.rs` to scope so per-subagent hook accounting is consistent. |
| 8 | `Bypass` non-override (M2) | **Enforced, not conventional** | `with_permission_mode` refuses to downgrade `Bypass` (monotonic); child clones already preserve it. "Cannot be overridden" becomes a property a security reviewer can rely on, not an absence-of-API. |
| 9 | PR structure (L1) | **One PR** behind shared `control.rs`, seams sequenced for independent AC verification | Larger review surface, accepted; `control.rs` extraction is the mitigation. |

## 4. Public API surface

Everything is **additive**. `RunContext::new`'s signature and the published `0.x` API stay intact. The common case (no config, no guardrails, no hooks) behaves exactly as today: `Default` mode + no policy ⇒ Allow all.

### 4.1 New module `permission.rs`

```rust
/// How permission mode governs tool calls. `Bypass` propagates to subagents
/// and cannot be overridden — a typed enum, not a string (enforced in §4.3).
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
    async fn check(&self, ctx: &RunContext<Ctx>, tool: &str, args: &serde_json::Value)
        -> PermissionDecision;
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
    async fn decide(&self, tool: &str, prompt: &str, args: &serde_json::Value) -> ApprovalOutcome;
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

The `#[tool]` macro is extended to set this (§4.7) — otherwise macro-authored tools could never be `ReadOnly`/`Write`.

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
    /// Set the mode. **Monotonic on `Bypass`:** once the mode is `Bypass`, this
    /// is a no-op — `Bypass` cannot be downgraded (the safety invariant).
    pub fn with_permission_mode(mut self, mode: PermissionMode) -> Self {
        if self.permission_mode != PermissionMode::Bypass { self.permission_mode = mode; }
        self
    }
    pub fn with_permission_policy(self, p: Arc<dyn PermissionPolicy<Ctx>>) -> Self;
    pub fn with_deny_rules(self, rules: Vec<DenyRule>) -> Self;
    pub fn with_approval_handler(self, h: Arc<dyn ApprovalHandler>) -> Self;
    pub fn permission_mode(&self) -> PermissionMode;
    // ... borrows for policy / deny_rules / approval_handler ...
}
```

**Propagation & the `Bypass` invariant (M2).** `handoff_child()` and `subagent_child()` clone all four fields verbatim, so a child always inherits the parent's mode. Combined with the monotonic setter above, `Bypass` is sticky on **all** paths — including a user-authored custom `Agent` that builds a child context and calls `with_permission_mode`, which can no longer lower it. "Cannot be overridden" is thus enforced where the mode is set, not assumed from the absence of an API.

### 4.4 `ToolContext<Ctx>` carries permission config (the agent-as-tool fix) (`tool.rs`)

`agent_as_tool` runs inside `Tool::invoke`, which only receives a `ToolContext` — today carrying **no** permission config. Handoff and workflow sub-runs propagate via `RunContext` children, but an agent-as-tool sub-run would **silently drop the mode**, violating "`Bypass` propagates to subagents." Fix: project the permission config into `ToolContext` so `agent_as_tool` rebuilds its sub-`RunContext` with it.

**Exposure is narrowed (L3):** only `permission_mode()` is a public accessor (a tool may legitimately branch on the run's mode). The `policy` / `deny_rules` / `approval_handler` carriers are **`pub(crate)`**, read solely by `agent_as_tool`'s rebuild path — tools cannot read the run's policy or deny rules.

```rust
// RunContext::to_tool_context() projects mode (public) + policy/deny_rules/handler (pub(crate)).
// agent_as_tool's sub_ctx gains: .with_permission_mode(tool_ctx.permission_mode())
//   plus the pub(crate) policy/deny_rules/handler carry-over.
```

`agent_as_tool` keeps its **session + hooks isolation** (fresh `MemorySession`, empty `HookRegistry`); only permission config rides through — the security invariant trumps isolation.

### 4.5 `HookEvent` gains `OnSubagentStop` (`hook.rs`)

```rust
#[non_exhaustive]
pub enum HookEvent {
    // ... existing: OnRunStart, OnTurnStart, PreToolUse, PostToolUse, OnHandoff, OnRunComplete ...
    /// Fired when a subagent sub-run completes — handoff target, agent-as-tool,
    /// OR a workflow sub-agent (Sequential/Parallel/Loop). See §6 for the firing sites.
    OnSubagentStop { agent: String },
}
```

### 4.6 New events & errors; guardrail/hook snapshotting (`agent.rs`)

```rust
// AgentEvent (additive):
PermissionDenied { tool: String, reason: String },   // observability; model gets denial via ToolResult

// AgentError (additive):
#[error("hook denied {event}: {reason}")]
HookDenied { event: String, reason: String },         // dedicated public error for a lifecycle Deny
```

`AgentEvent::ApprovalRequested` already exists and is reused as-is (`{call_id, tool, args}`); the `AskUser` prompt is routed to the `ApprovalHandler` directly.

**Stream snapshot (H2).** `Agent::run` returns `BoxStream<'static, AgentEvent>` and snapshots everything the stream needs into owned values **before** the `async_stream` (verified `agent.rs:583` — `model`/`tools`/`model_settings`/`output_type`/`handoffs`/`config` are cloned, but **not** guardrails/hooks). Add alongside them:

```rust
let input_guardrails  = self.input_guardrails.clone();   // Vec<Arc<dyn Guardrail<Ctx>>>
let output_guardrails = self.output_guardrails.clone();
let agent_hooks       = self.hooks.clone();              // Vec<Arc<dyn Hook<Ctx>>>
```

`Interceptors` borrows these **stream-local Arc-snapshots** (and the run-level `HookRegistry`, which rides in the moved `ctx`), never `&self` — otherwise it cannot live in the `'static` stream.

### 4.7 `#[tool]` macro gains `effect=` (`paigasus-helikon-macros`)

- **`attr.rs`** — add `effect: Option<ToolEffectArg>` to `ToolAttrArgs`; parse `effect = read_only | write | side_effect`; add `effect` to the "expected one of …" error list (currently `description`, `name`, `crate`).
- **`expand.rs`** — when `effect` is set, emit a `fn effect(&self) -> #core::ToolEffect { #core::ToolEffect::… }` override into the generated `impl #core::Tool` block (lines 69–92 today); when unset, emit nothing (trait default `SideEffect` applies).
- Cross-crate release: a `feat(macros)` **minor** bump rides with the core change. The macro references `#core::ToolEffect`, so it depends on the core API added here — the workspace dep pin must point at the core version that ships `ToolEffect` (apply the same-PR core-bump caveat from `CLAUDE.md` if the verify build needs it).

## 5. Control-layer semantics (`control.rs`)

`Interceptors<'a, Ctx>` borrows the **stream-local Arc-snapshots** of the agent's guardrails/hooks (§4.6) and the context's policy/mode/deny_rules/handler (run-level hooks via the moved `ctx`'s `HookRegistry`), exposing the four async seam methods the driver calls. This is the **unit-tested** unit.

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

A `Deny`/`Replace` outcome **never aborts the run** — a denied call yields a synthetic `ToolResult` carrying the reason (plus a `PermissionDenied` event); the model sees it and continues. This keeps `transition()` untouched.

### 5.2 Hook firing & conflict resolution (`fire`)

Hooks run **agent-level first, then `RunContext` registry** (run-global). Outcomes fold into one `ResolvedHookDecision`:

- **`Deny` short-circuits** — first `Deny` wins; remaining hooks skipped.
- **`ReplaceInput` / `ReplaceOutput`** — applied in order; **last writer wins** for the final value.
- **`InjectSystemMessage`** — **accumulate** all; appended to the conversation (as `Item::System`) before the next model call.
- **`Allow`** — no-op.

| Event | Honors | Deny means |
| --- | --- | --- |
| `PreToolUse` | Deny, ReplaceInput | denial `ToolResult`; skip permission + invoke |
| `PostToolUse` | ReplaceOutput | convert the result into a denial message to the model |
| `OnRunStart` / `OnTurnStart` / `OnHandoff` | Deny, InjectSystemMessage | abort run (`AgentError::HookDenied`) |
| `OnSubagentStop` / `OnRunComplete` | (observational) | ignored |

`Replace*` are ignored on pure lifecycle events (nothing to replace).

### 5.3 Guardrails (blocking gate)

- **Input** — after `OnRunStart`, before the loop, on `GuardrailInput::UserText(seed)`. **`seed` is defined as the concatenated text of all `Item::UserMessage` `ContentPart::Text` parts in the run's seed `AgentInput.messages`** (L4). This is well-defined for every entry path: a top-level run (the user's input), a handoff target (the user-message text in the threaded transcript, including the synthetic transfer note), and a workflow sub-agent (the parent's original `AgentInput`). Tripwire ⇒ `GuardrailTriggered` + `RunFailed`, `failure.set(AgentError::Guardrail{kind})`, return — **zero model calls** (AC1).
- **Output** — when the driver detects `next_state == Done`, run on `out.as_text()` **before** yielding the bundled `RunCompleted`. Tripwire ⇒ suppress `RunCompleted`, emit `GuardrailTriggered` + `RunFailed`.
- Agent-level: each nested agent runs its **own** input guardrails at the start of its run.

*Why blocking, not optimistic (M1):* AC1 requires the input check to abort **before any model call**. Optimistic execution runs the model **concurrently** with the guardrail and cancels on a tripwire — which necessarily starts a model call. The two are mutually exclusive for *input* guardrails, so blocking is the only AC1-compliant design. Optimistic execution remains a future, non-gating (e.g. output-side) latency optimization; the trait is unchanged, so adding it later is non-breaking.

### 5.4 Per-tool-call interleaving (inside `run_tools_concurrent`)

```
PreToolUse hook → [Deny ⇒ denial result] → effective args
  → authorize(args) → [Deny ⇒ denial result] [Replace ⇒ sanitized args]
  → tool.invoke(args)
  → PostToolUse hook → effective output
  → ToolResult
```

Each call's pipeline runs as a unit under the existing `parallel_tool_call_limit`. `run_tools_concurrent` gains a `&Interceptors<'_, Ctx>` borrow.

## 6. Driver integration & data flow (`agent.rs`, `workflow.rs`, `agent_as_tool.rs`)

All driver changes land in `Agent::run`'s `async_stream`. Locations are approximate line regions in today's file.

| Seam | Location | Added behavior |
| --- | --- | --- |
| Snapshot guardrails/hooks | with the existing snapshot block (~583) | clone into stream-local owned `Vec<Arc<…>>` (§4.6) |
| `OnRunStart` | after `yield RunStarted` (~654) | `fire(OnRunStart)`; Deny aborts; queue injections |
| Input guardrails | before `loop {` (~676) | blocking gate on seed user text; trip ⇒ fail, return |
| `OnTurnStart` | `TurnStarted` already matched for tracing (~691) | `fire(OnTurnStart{turn})`; injections queued |
| Inject system msgs | before `CallModel` builds its request (~729) | drain queued injections into `conversation` as `Item::System` |
| Tool gating | `NextAction::ExecuteTools` arm (~849) | `run_tools_concurrent(&interceptors, …)` runs §5.4 |
| Output guardrails | detect `next_state == Done` before yielding its events (~688) | gate `RunCompleted`; trip ⇒ rewrite to failure |
| `OnHandoff` | `NextAction::Handoff` arm, before running target (~920) | `fire(OnHandoff{from,to})`; Deny aborts handoff |
| `OnSubagentStop` (handoff) | after handoff sub-stream drains (~951) | `fire(OnSubagentStop{agent})` (observational) |
| `OnRunComplete` | `NextAction::Terminate` arm (~870) | `fire(OnRunComplete)` (observational) before return |

**`OnSubagentStop` outside the driver (M3):**
- **`agent_as_tool.rs`** — fire `OnSubagentStop{agent}` after its sub-run's stream is collected.
- **`workflow.rs`** — fire `OnSubagentStop{agent}` after each `agent.run(child, …)` completes at all three sites: Sequential (~166), Parallel (~326), Loop (~499). Fired against the workflow agent's `RunContext` (the shared `HookRegistry` rides through `subagent_child`).

**The one subtlety — output guardrails vs. the bundled `RunCompleted`.** `transition()` packs `MessageOutput` + `RunCompleted` into one events vec for the `Done` arm. So before the normal yield loop, the driver checks `if let LoopState::Done(out) = &next_state`: it runs output guardrails on `out.as_text()` first; on a tripwire it rewrites the outcome's terminal events to `GuardrailTriggered + RunFailed` and `failure.set(...)`. `transition()` itself stays pure and unchanged.

**Propagation map (the `Bypass` invariant, made universal):**

```
top-level RunContext{ mode, policy, deny_rules, handler }
   ├─ handoff_child()   → clones all four (mode Bypass-sticky)        ✓ RunContext path
   ├─ subagent_child()  → clones all four (Seq/Parallel/Loop)         ✓ RunContext path
   └─ agent_as_tool     → ToolContext carries them (mode pub,
                          rest pub(crate)) → sub RunContext rebuilt    ✓ the fix (§4.4)
```

## 7. What is explicitly NOT in scope

- **Durable suspend/resume for approval.** `LoopState::NeedsApproval` is left undriven, the documented seam for `paigasus-helikon-runtime-temporal` / `-agentcore`. v1 resolves `AskUser` inline via `ApprovalHandler` (default Deny).
- **Optimistic (parallel) guardrail execution.** v1 is a blocking gate (§5.3, M1); the `Guardrail` trait is unchanged so this is a future, non-breaking addition for non-gating cases.
- **Persisted permission state / allowlists.** "Unfamiliar tool" tracking (Default mode) is delegated to a user-supplied policy, not built into core.
- **Rich `DenyRule` matchers** (glob, predicate, arg-aware). v1 is exact tool-name; `#[non_exhaustive]` keeps it extensible.

## 8. Testing strategy (TDD)

**Unit tests on `control.rs` / `permission.rs`** — high-value, no model needed:

- Permission pipeline truth table: deny-rule beats `Bypass`; `Plan` denies `SideEffect`/`Write`, allows `ReadOnly`; `AcceptEdits` allows `Write`; `Default`+no-policy allows; `AskUser`+no-handler denies; `Replace` sanitizes args.
- Hook conflict resolution: first-`Deny`-wins short-circuit; last-`ReplaceInput`-wins; multiple `InjectSystemMessage` accumulate; agent-before-registry order.
- `ToolEffect` default is `SideEffect`.
- **`Bypass` monotonicity (M2):** `with_permission_mode(Plan)` on a `Bypass` context is a no-op.

**Driver / integration tests** (mock model + mock tools, existing style):

- **AC1** — failing input guardrail ⇒ run aborts with **zero** model invocations (assert the mock model's call count is 0).
- **AC2** — `PreToolUse` returning replace-input ⇒ the tool receives replaced args (recording tool).
- **AC3** — `Plan` mode ⇒ every non-`ReadOnly` tool call denied (model sees denial `ToolResult`s).
- Output-guardrail tripwire suppresses `RunCompleted`, yields `RunFailed`.
- `Bypass` propagation across **all three** sub-run paths — including agent-as-tool (regression for the `ToolContext` fix).
- `AskUser` + installed handler returning `Allow`/`Deny` resolves correctly; `ApprovalRequested` emitted.
- **`OnSubagentStop` (M3)** fires for a handoff target, an agent-as-tool sub-run, **and** a workflow sub-agent.

**Macro tests (`paigasus-helikon-macros`, H1):**

- `#[tool(effect = read_only)]` generates an `effect()` returning `ReadOnly`; the tool is **allowed under `Plan`** (end-to-end with a mock model).
- `effect = write` / `side_effect` parse; an unknown value and a bare `#[tool]` (default `SideEffect`) behave as specified.

**Gates:** every new `pub` item gets a `///` doc with a doctest at entry points (`RUSTDOCFLAGS=-D warnings` + 80% doc-coverage). New code follows the workspace `#[non_exhaustive]` + `missing_docs` conventions.

## 9. File-by-file change summary

| File / crate | Change |
| --- | --- |
| `core/src/permission.rs` | **new** — `PermissionMode`, `PermissionDecision`, `PermissionPolicy`, `DenyRule`, `ApprovalHandler`, `ApprovalOutcome` |
| `core/src/control.rs` | **new** — `Interceptors<'a, Ctx>` + `ResolvedHookDecision`; the four seam methods (borrow stream-local snapshots) |
| `core/src/tool.rs` | `ToolEffect` enum; `Tool::effect()` default method; `ToolContext` carries permission config (mode pub, rest pub(crate)) + accessors |
| `core/src/context.rs` | `RunContext` permission fields + `with_*` setters (monotonic `Bypass`); propagate in `handoff_child`/`subagent_child`; project in `to_tool_context` |
| `core/src/hook.rs` | add `HookEvent::OnSubagentStop` |
| `core/src/agent.rs` | snapshot guardrails/hooks (H2); drive all seams in `Agent::run`; `run_tools_concurrent` takes `&Interceptors`; add `AgentEvent::PermissionDenied`, `AgentError::HookDenied` |
| `core/src/agent_as_tool.rs` | rebuild sub-`RunContext` with inherited permission config; fire `OnSubagentStop` |
| `core/src/workflow.rs` | fire `OnSubagentStop` after each sub-agent run (Sequential/Parallel/Loop) |
| `core/src/lib.rs` | `pub mod permission; pub mod control;` + re-exports |
| `core/src/loop_state.rs` | **no change** (pure state machine stays pure) |
| `macros/src/attr.rs` | parse `effect = read_only \| write \| side_effect`; extend the unknown-key error list |
| `macros/src/expand.rs` | emit `fn effect()` override into the generated `impl Tool` when `effect` is set |

## 10. Release

Additive surface (new modules, `#[non_exhaustive]` enum variant/field additions, private `RunContext`/`ToolContext` fields with unchanged `new` arity, a defaulted trait method) ⇒ **`feat(core)` minor bump** and a **`feat(macros)` minor bump** for the `effect=` support.

- **Target:** core `0.4.0 → 0.5.0` (or the next minor if SMA-325's bump is still pending); macros to its next minor. **Verify the live versions at implementation time** (per `CLAUDE.md` — read each crate's `Cargo.toml`; don't trust hardcoded numbers).
- **Cross-crate caveat:** the macro emits `#core::ToolEffect`, so `macros` depends on the core API added in this PR. If `cargo publish --verify` for `macros` builds against the registry core, apply the same-PR core-bump + `[workspace.dependencies]` pin + facade-bump recipe from `CLAUDE.md` ("ascending crate uses same-PR core API"). Confirm whether the facade (`paigasus-helikon`) needs a republish so its dep reqs track the new core/macros versions.
