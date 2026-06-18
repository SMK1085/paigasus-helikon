# Permissions, Guardrails & Hooks

Three governance layers ship in [`paigasus-helikon-core`](https://docs.rs/paigasus-helikon-core), each addressing a different concern:

- **Permissions** authorize tool calls (*may this tool run with these args?*).
- **Guardrails** validate input and output content (*is this text safe?*).
- **Hooks** intercept lifecycle events (*observe, deny, or rewrite around each step*).

They are orthogonal. A tool call passes the permission pipeline; a hook can still veto or rewrite it; guardrails gate the surrounding text. All three are typed traits — no stringly-typed policy DSL.

## Permissions

A tool call is authorized by the pipeline `deny rules › guard rules › allow rules › mode › policy › AskUser`, evaluated in that order. The pieces:

- `PermissionMode` — a `#[non_exhaustive]` enum: `Default` (defer to policy; permissive when no policy), `AcceptEdits` (auto-approve tools whose `ToolEffect` is `Write`), `Plan` (deny any tool whose `ToolEffect` is not `ReadOnly`), `Bypass` (allow all — deny rules still apply), and `DontAsk` (deny-by-default headless lockdown — the policy is never invoked; only an allow rule can permit a call). Mode is **tighten-only**: `RunContext::with_permission_mode` refuses to loosen `Bypass` (it may only tighten to `DontAsk`), and `DontAsk` is terminal. `Bypass` and `DontAsk` both propagate to sub-agents.
- `DenyRule` — a first-class rule evaluated **before** mode, so it overrides even `Bypass`. Matches by exact tool name (`DenyRule::tool("Bash")`), by Bash sub-command program (`DenyRule::bash_command("rm")`), or by filesystem path glob (`DenyRule::read(".env")` / `DenyRule::edit("dist/**")` — see **Allow rules & filesystem path rules** below). See also **Guard rules** below for the higher-level Bash-command matcher.
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

### Guard rules & the destructive-command breaker

Guard rules sit *above* the permission pipeline. A `GuardRule` has an action — `Deny` or `Ask` — and is evaluated **before** permission mode. Even `PermissionMode::Bypass` does not skip them.

An always-on built-in set, `GuardRule::destructive_defaults()`, is installed on every `RunContext` automatically. It blocks two classes of command:

- **Recursive removes at catastrophic paths.** `rm -rf /` and `rm -rf ~` (the home directory) are blocked by default.
- **Writes to protected system paths.** Commands that write to `/etc`, `/usr`, `/bin`, `/sbin`, `/sys`, `/boot`, `/dev`, or `/` are blocked. A device-node allowlist exempts the common redirect target `> /dev/null` (and other `/dev/null`, `/dev/stderr`, `/dev/stdout` forms) so ordinary output suppression is never blocked.

The default action for a matched rule is `Ask`, which **resolves to Deny when no `ApprovalHandler` is installed** — the common headless/CI configuration.

> **Behavior change.** In `Default` mode with no policy and no approval handler — the typical unattended setup — a command matching a destructive guard now resolves to **Deny** rather than running silently. To restore interactive behavior, install an `ApprovalHandler` (the runner will prompt before blocking). To disable the guards entirely, call `RunContext::without_default_guards()`:
>
> ```rust
> let ctx = RunContext::new(/* … */)
>     .without_default_guards();
> ```

### Operator-aware deny matching

`DenyRule::bash_command("rm")` matches when **any sub-command** of a compound command resolves to that program. The matcher:

1. Splits the command string on `&&`, `||`, `;`, `|`, `|&`, `&`, and newlines.
2. Strips fixed wrappers — `timeout`, `nice`, `nohup`, `stdbuf`, `env`, `command`, `sudo`, `doas` — and their flags.
3. Unquotes the program token.
4. Follows `bash -c '…'` / `sh -c '…'` re-entry to a bounded depth.

As a result, `echo ok && rm -rf .`, `sudo rm -rf /`, and `bash -c 'rm -rf /'` are all caught by a single `DenyRule::bash_command("rm")`.

`BashTool`'s own `deny_commands`/`allow_commands` lists use the same matcher, with a defined composition rule:

- **deny list** — the command is denied if **any** sub-command's program is denied.
- **allow list** — the command is permitted only if **every** sub-command's program is in the list.

### Allow rules & filesystem path rules

`AllowRule` is the positive counterpart of `DenyRule`. A matching allow rule
resolves the call to `Allow` **after** the deny and guard steps but **before**
mode — in *every* mode — and `canUseTool` is not consulted for it. It is a
**global, all-modes, per-tool pre-approval**, so prefer the narrow forms:

- `AllowRule::tool("WebSearch")` — allow a tool by name.
- `AllowRule::bash_command("git")` — allow a Bash call only when *every*
  sub-command's program is `git` (fail-closed on a mixed compound command).
- `AllowRule::read("src/**")` / `AllowRule::edit("src/**")` — allow `Read` /
  `Edit`+`Write` whose `path` matches a gitignore-style glob.

`DenyRule` gains the same path forms: `DenyRule::read(".env")` blocks reads of
`.env` at any depth; `DenyRule::edit("dist/**")` blocks writes under `dist`.
Under `DontAsk`, allow rules are the *only* way a call is permitted:

```rust
let ctx = RunContext::new(/* … */)
    .with_allow_rules(vec![
        AllowRule::tool("WebSearch"),
        AllowRule::edit("src/**"),
    ])
    .with_permission_mode(PermissionMode::DontAsk);
```

**Path rules are advisory, not a sandbox.** Core has no filesystem root, so a
path rule is a lexical match on the `path` argument (`..` is collapsed and
matching is case-insensitive, but the real boundary is the cap-std root in
`paigasus-helikon-tools`). Pattern syntax: a pattern without a `/` matches at
any depth (`.env`, `*.pem`); a pattern with a `/` is anchored to the root
(`src/**`). Because an allow rule short-circuits the mode, a matched
`AllowRule::edit("src/**")` permits a write even under `Plan`'s read-only gate —
intended, since an allow rule is a pre-approval.

### The `.git`/`.ssh`/`.env` write breaker

A third built-in guard joins `destructive_defaults()`: a write whose target has
a `.git` or `.ssh` path component, or a final component `.env`/`.env.*`, is
`Ask` (→ Deny when headless). Component-exact and case-insensitive, so
`name.git/`, `.gitignore`, and `environment.env` are unaffected while `.SSH/`
and `.ENV` still trip. It runs before mode and before allow rules, so a `.git/`
write is refused even under `AcceptEdits` and even with a matching
`AllowRule::edit(".git/**")`. Disabled by `without_default_guards()`.

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

### Secret redaction

On by default, tool output is scrubbed of secret-shaped strings before it re-enters the model context and the session trajectory. Redaction runs as the **final** transform on `PostToolUse` output — after any user `PostToolUse` hook.

Two matchers run in sequence:

1. **Key-name patterns.** Lines matching `KEY=value`, `KEY: value`, or `export KEY=value` where `KEY` ends (case-insensitively) in `_API_KEY`, `_TOKEN`, `_SECRET`, `_PASSWORD`, or `_CREDENTIAL` have the value portion replaced with `***`.
2. **Known-secret value scan.** Literal occurrences of known secret values — the parent process's secret-named env vars, plus any strings registered via `RunContext::with_extra_secrets(…)` — are replaced with `***`. A length floor (≥ 8 characters, common English words excluded) prevents over-matching from corrupting ordinary output.

To add application-specific secrets to the scan:

```rust
let ctx = RunContext::new(/* … */)
    .with_extra_secrets(vec!["my-api-key-value".to_string()]);
```

To disable redaction entirely:

```rust
let ctx = RunContext::new(/* … */)
    .without_output_redaction();
```

### Scope & limitations

The v1 Bash guard and deny-matching are pragmatic, not based on a full POSIX shell parser. Known limitations:

- **Command substitution is not parsed.** Tokens inside `$(…)` or backtick expressions are not inspected.
- **`find -exec`/`find -delete`, `xargs`, and `eval`** are not followed into their arguments; only the top-level program name is matched.
- **Variable-indirect command strings** (`eval "$VAR"`, `$CMD arg`) are not resolved — the matcher sees literal pre-expansion tokens.
- **Shell-expanded globs and variables** in the command string are not expanded before matching.
- **`bash -c` re-entry** is followed to a bounded depth only; deeply nested shells are not fully traced.
- **`rm -rf <protected-prefix>`** — only `/` (root) and `~` (home directory) are guarded against recursive removal. Subtrees such as `/etc` or `/usr` are protected only against writes, not `rm -rf`.
- **Relative redirect targets** such as `> ../../etc/passwd` are not canonicalized; only absolute protected paths are matched on the write-guard.
- **Redaction limitations:** only the first `KEY=`/`KEY:` occurrence per line is processed; value-scan matching is case-sensitive; key-name matching requires underscore form (`X_API_KEY`, not `X-API-KEY`) and does not scan JSON object keys.

## How they compose

For a single tool call, the layers run in this order:

1. The `PreToolUse` hook fires — it may deny or `ReplaceInput` the args.
2. The permission pipeline authorizes the (possibly rewritten) call: `deny rules › guard rules › allow rules › mode › policy › AskUser`.
3. The tool runs; the `PostToolUse` hook fires — it may `ReplaceOutput`.
4. Secret redaction runs as the final transform on the output before it enters the model context.

Input guardrails gate user text before the loop begins; output guardrails gate the final model text before it is returned. See [The Agent Loop](./agent-loop.md) for where each seam sits in the run, and [Multi-Agent Patterns](./multi-agent-patterns.md) for how `Bypass` mode and the shared registry propagate across handoffs.
