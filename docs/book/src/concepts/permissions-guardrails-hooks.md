# Permissions, Guardrails & Hooks

Three governance layers ship in [`paigasus-helikon-core`](https://docs.rs/paigasus-helikon-core), each addressing a different concern:

- **Permissions** authorize tool calls (*may this tool run with these args?*).
- **Guardrails** validate input and output content (*is this text safe?*).
- **Hooks** intercept lifecycle events (*observe, deny, or rewrite around each step*).

They are orthogonal. A tool call passes the permission pipeline; a hook can still veto or rewrite it; guardrails gate the surrounding text. All three are typed traits — no stringly-typed policy DSL.

## Permissions

A tool call is authorized by the pipeline `deny rules › mode › policy › AskUser`, evaluated in that order. The pieces:

- `PermissionMode` — a `#[non_exhaustive]` enum: `Default` (defer to policy; permissive when no policy), `AcceptEdits` (auto-approve tools whose `ToolEffect` is `Write`), `Plan` (deny any tool whose `ToolEffect` is not `ReadOnly`), `Bypass` (allow all — deny rules still apply). `Bypass` is **sticky**: `RunContext::with_permission_mode` refuses to downgrade it, and it propagates to sub-agents.
- `DenyRule` — a first-class rule evaluated **before** mode, so it overrides even `Bypass`. v1 matches by exact tool name: `DenyRule::tool("Bash")`.
- `PermissionPolicy<Ctx>` — the `canUseTool` trait. Its async `check` returns a `PermissionDecision`: `Allow`, `Deny { reason }`, `AskUser { prompt }`, or `Replace { args }` (sanitize the call's arguments before execution).
- `ApprovalHandler` — resolves an `AskUser` decision out of band. Its `decide` returns an `ApprovalOutcome` (`Allow` or `Deny { reason }`) — it cannot recursively ask. With **no** approval handler installed, `AskUser` resolves to deny.

Permissions attach to the `RunContext`:

```rust
use std::sync::Arc;
use async_trait::async_trait;
use paigasus_helikon::core::{
    ApprovalHandler, ApprovalOutcome, CancellationToken, DenyRule, HookRegistry,
    MemorySession, PermissionDecision, PermissionMode, PermissionPolicy, RunContext,
    TracerHandle,
};

// A policy that asks before any tool touching the network.
struct AskOnNetwork;

#[async_trait]
impl PermissionPolicy<()> for AskOnNetwork {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        tool: &str,
        _args: &serde_json::Value,
    ) -> PermissionDecision {
        if tool == "WebFetch" {
            PermissionDecision::AskUser { prompt: format!("Allow {tool}?") }
        } else {
            PermissionDecision::Allow
        }
    }
}

// An approval handler that auto-approves (a real one would prompt a human).
struct AutoApprove;

#[async_trait]
impl ApprovalHandler for AutoApprove {
    async fn decide(
        &self,
        _tool: &str,
        _prompt: &str,
        _args: &serde_json::Value,
    ) -> ApprovalOutcome {
        ApprovalOutcome::Allow
    }
}

let ctx: RunContext<()> = RunContext::new(
    Arc::new(()),
    Arc::new(MemorySession::new()),
    HookRegistry::<()>::new(),
    TracerHandle::default(),
    CancellationToken::new(),
)
.with_permission_mode(PermissionMode::AcceptEdits)
.with_deny_rules(vec![DenyRule::tool("Bash")])
.with_permission_policy(Arc::new(AskOnNetwork))
.with_approval_handler(Arc::new(AutoApprove));
```

`with_permission_mode`, `with_deny_rules`, `with_permission_policy`, and `with_approval_handler` are consuming builder methods on `RunContext`; the corresponding readers are `permission_mode`, `deny_rules`, `permission_policy`, and `approval_handler`. A tool's `ToolEffect` (`ReadOnly`, `Write`, or `SideEffect`) is what `AcceptEdits` and `Plan` mode test against — see [Tools](./tools.md).

## Guardrails

A `Guardrail<Ctx>` validates content flowing into or out of the agent. Its single async method `check` receives a `GuardrailInput<'_>` — either `UserText(&str)` (text entering the agent) or `ModelOutput(&str)` (text leaving it) — and returns `Result<GuardrailVerdict, GuardrailError>`:

- `GuardrailVerdict::Pass` — all clear; the run continues.
- `GuardrailVerdict::Tripwire { kind, info }` — the run halts. `kind` is a `GuardrailKind` (`InputPolicy`, `OutputPolicy`, or `Other { reason }`); `info` is free-form JSON. A tripwire is a *successful* verdict, not an error.
- `GuardrailError` — a failure of the guardrail itself. The runner treats a guardrail error as a tripwire of kind `GuardrailKind::Other`.

Guardrails attach to the **agent**, separately for input and output:

```rust
use async_trait::async_trait;
use paigasus_helikon::core::{
    Guardrail, GuardrailError, GuardrailInput, GuardrailKind, GuardrailVerdict,
    LlmAgent, RunContext,
};

struct BlockSecrets;

#[async_trait]
impl Guardrail<()> for BlockSecrets {
    async fn check(
        &self,
        _ctx: &RunContext<()>,
        input: GuardrailInput<'_>,
    ) -> Result<GuardrailVerdict, GuardrailError> {
        let text = match input {
            GuardrailInput::UserText(t) | GuardrailInput::ModelOutput(t) => t,
        };
        if text.contains("BEGIN PRIVATE KEY") {
            Ok(GuardrailVerdict::Tripwire {
                kind: GuardrailKind::InputPolicy,
                info: serde_json::json!({ "matched": "private key" }),
            })
        } else {
            Ok(GuardrailVerdict::Pass)
        }
    }
}

let agent = LlmAgent::builder::<()>()
    .name("assistant")
    // .model(...).instructions(...)
    .input_guardrail(BlockSecrets)
    .output_guardrail(BlockSecrets)
    .build();
```

The builder also exposes `shared_input_guardrail` / `shared_output_guardrail` (taking a pre-wrapped `Arc<dyn Guardrail<Ctx>>`) and `input_guardrails` / `output_guardrails` (replacing the whole list from an iterator).

## Hooks

A `Hook<Ctx>` observes lifecycle events and can steer the run. Its async `on_event` receives a `&HookEvent` and returns a `HookDecision`. Hooks are *observation and side effects* — distinct from permissions (authorization) and guardrails (content).

`HookEvent` is a `#[non_exhaustive]` enum covering the run lifecycle: `OnRunStart`, `OnTurnStart { turn }`, `PreToolUse { tool, args }`, `PostToolUse { tool, output }`, `OnHandoff { from, to }`, `OnRunComplete`, and `OnSubagentStop { agent }`. (`OnRunComplete` is best-effort — a cancelled run may abort a still-running completion hook.)

`HookDecision` is also `#[non_exhaustive]`:

- `Allow` — proceed unchanged.
- `Deny { reason }` — block the event; the reason is surfaced to the agent.
- `ReplaceInput { value }` — rewrite the value the runner is about to use (e.g. sanitize `PreToolUse` args).
- `ReplaceOutput { value }` — rewrite the value the runner just observed (e.g. redact `PostToolUse` output).
- `InjectSystemMessage { text }` — inject a system message into the next model call.

When several hooks fire for one event, the runner folds the decisions: the first `Deny` short-circuits, `ReplaceInput`/`ReplaceOutput` is last-writer-wins, and `InjectSystemMessage` accumulates in fire order.

Hooks attach in two places. Per-agent hooks go on the builder via `hook` (or `shared_hook` / `hooks`); run-level hooks go in the `HookRegistry<Ctx>` carried by the `RunContext`. Agent-level hooks fire before run-level ones.

```rust
use async_trait::async_trait;
use std::sync::Arc;
use paigasus_helikon::core::{
    Hook, HookDecision, HookEvent, HookRegistry, LlmAgent, RunContext,
};

struct AuditLog;

#[async_trait]
impl Hook<()> for AuditLog {
    async fn on_event(&self, _ctx: &RunContext<()>, event: &HookEvent) -> HookDecision {
        if let HookEvent::PreToolUse { tool, .. } = event {
            eprintln!("about to call tool: {tool}");
        }
        HookDecision::Allow
    }
}

// Per-agent:
let agent = LlmAgent::builder::<()>()
    .name("assistant")
    // .model(...).instructions(...)
    .hook(AuditLog)
    .build();

// Or run-level, via the registry on the RunContext:
let mut registry = HookRegistry::<()>::new();
registry.push(Arc::new(AuditLog));
```

`HookRegistry` is the run-level container: `new`, `push`, `iter`, and `is_empty`. It is the third positional argument to [`RunContext::new`](./core-primitives.md) and is shared (cloned) across handed-off and sub-agent contexts.

## How they compose

For a single tool call, the layers run in this order:

1. The `PreToolUse` hook fires — it may deny or `ReplaceInput` the args.
2. The permission pipeline authorizes the (possibly rewritten) call: `deny rules › mode › policy › AskUser`.
3. The tool runs; the `PostToolUse` hook fires — it may `ReplaceOutput`.

Input guardrails gate user text before the loop begins; output guardrails gate the final model text before it is returned. See [The Agent Loop](./agent-loop.md) for where each seam sits in the run, and [Multi-Agent Patterns](./multi-agent-patterns.md) for how `Bypass` mode and the shared registry propagate across handoffs.
