# SMA-414 — Bash command-policy hardening

**Status:** approved (design)
**Date:** 2026-06-17
**Linear:** [SMA-414](https://linear.app/smaschek/issue/SMA-414/paigasus-helikon-tools-bash-command-policy-hardening-operator-aware)
**Related:** SMA-326 (PermissionPolicy / PermissionMode), SMA-328 (BashTool)

## Problem

Three hardening follow-ups surfaced by SMA-326 / SMA-328 in how Bash calls are
matched and how their output is handled:

1. **Operator-aware deny matching.** Deny matching looks only at the first
   whitespace-delimited token, so a compound command bypasses it:
   `echo ok && rm -rf /` resolves to program `echo` and slips through a deny
   rule for `rm`.
2. **Destructive-command circuit breaker.** `PermissionMode::Bypass`
   short-circuits to `Allow` before the policy runs, so there is no always-on
   floor that stops `rm -rf /` / `rm -rf ~` / writes to protected paths under
   Bypass.
3. **Output secret redaction.** Tool output flows back into the model context
   and logs verbatim; secret-shaped strings (`*_API_KEY`, `*_TOKEN`,
   `*_SECRET`, `*_PASSWORD`, `*_CREDENTIAL`) are never scrubbed.

## Acceptance criteria

- **AC-1.** A deny rule for `rm` blocks `echo ok && rm -rf .` (compound command).
- **AC-2.** `rm -rf /` is denied/prompted even with `PermissionMode::Bypass` set.
- **AC-3.** Secret-shaped strings in command output are redacted before reaching
  the model.

## Current state (what we build on)

- **Permission pipeline** — `core/src/control.rs::Interceptors::authorize`
  runs `deny rules › mode › policy › AskUser`. Deny rules run **before** mode,
  so they already override `Bypass` (test `deny_rule_beats_bypass`). `Bypass`
  returns `Allow` before the policy, so policy-driven *ask* decisions do not
  survive Bypass today.
- **`DenyRule`** — `core/src/permission.rs`. Matches by exact tool name;
  `matches(&self, tool, _args)` carries an unused `_args` param documented as
  *"reserved for richer (arg-aware) matchers in a later ticket."* This is that
  ticket.
- **`BashTool`** — `tools/src/bash.rs`. Has its own tool-local
  `deny_commands` / `allow_commands` lists, matched via
  `command.split_whitespace().next()` (first token only). Containment is
  delegated to the `ExecutionBackend`; the host backend already has an
  `env_allowlist` (SMA-328), whose values are read from `std::env::var(name)`.
- **Hook seam** — `core/src/agent.rs::run_tools_concurrent` runs
  `PreToolUse → authorize → invoke → PostToolUse`. `PostToolUse` already
  supports `ReplaceOutput`, applied to the tool's JSON before it is rendered
  into content parts for the model. The *output guardrail* seam
  (`run_output_guardrails`) only sees final **model** text, not tool output, so
  it is the wrong seam for tool-output redaction — `PostToolUse` is correct.

## Design decisions (resolved during brainstorming)

| # | Decision |
|---|----------|
| D1 | The shell tokenizer and the arg-aware matcher live in **core** (extend the existing `DenyRule._args` breadcrumb). |
| D2 | The breaker is a **pre-mode guard rule** that can `Deny` **or** `Ask` (resolved via the approval handler, default-Deny), evaluated before mode so it beats `Bypass`. |
| D3 | The built-in destructive guard set is **always-on**, with an explicit `RunContext::without_default_guards()` opt-out. |
| D4 | Redaction matches **both** key-name patterns in text **and** known secret env values; it lives in **core** as an always-on `PostToolUse` step with an opt-out. |

## Architecture

### Crate touch-map & release sequencing

| Crate | Changes |
|-------|---------|
| **paigasus-helikon-core** | new `command_match` module (tokenizer); arg-aware `DenyRule`; new `GuardRule` / `GuardAction`; `authorize()` guard step; new `redaction` module; `agent.rs` post-tool redaction step; `RunContext` config (`guard_rules`, `without_default_guards`, redaction opt-out + extra-secrets). |
| **paigasus-helikon-tools** | `BashTool` deny/allow lists become operator-aware via core's `command_match`; crate README + docs. |
| **paigasus-helikon (facade)** | re-export any new public types; bump (see below). |
| **docs/book + READMEs** | `concepts/permissions-guardrails-hooks.md`; tools `README.md`. |

Because **tools consumes new core API in the same PR**, this triggers the
CLAUDE.md "same-PR core bump + facade bump" rule: bump
`paigasus-helikon-core` (patch — additive behind `#[non_exhaustive]`), update
its `[workspace.dependencies]` pin + CHANGELOG, and bump the facade
(`paigasus-helikon`) so it republishes with current sibling reqs. release-plz
then publishes core → tools → facade in dependency order.

### Component 1 — `command_match` module (core, pure, dependency-free)

A small, self-contained tokenizer. No new third-party deps.

```rust
/// One sub-command of a compound command, after wrapper stripping.
pub struct ResolvedCommand<'a> {
    pub program: &'a str,   // effective program token
    pub args: Vec<&'a str>, // remaining tokens
}

/// Split a compound command on shell operators, quote-aware.
/// Operators: && || ; | |& & and newlines. Does not split inside '…' or "…".
pub fn split_operators(command: &str) -> Vec<&str>;

/// Strip leading env assignments (FOO=bar) and fixed wrappers
/// (timeout, nice, nohup, stdbuf, env, command) with their flag args,
/// returning the effective program + args. Returns None for an empty segment.
pub fn resolve_command(segment: &str) -> Option<ResolvedCommand<'_>>;
```

**Scope (deliberate v1 limitations, documented not hidden):**
- Models Claude Code's *pragmatic* Bash matcher, **not** a full POSIX shell
  grammar.
- Command substitution `$(…)` / backticks is **not** parsed into. A deny target
  hidden only inside a substitution is a known gap, called out in the rustdoc
  and the concept page rather than silently passing as "covered."
- Quote handling covers single/double quotes for the purpose of not
  mis-splitting operators; it is not a full quote-removal pass.

### Component 2 — arg-aware `DenyRule`

`DenyRule` keeps `Debug + Clone + PartialEq + Eq` by modeling the matcher as an
enum (no boxed closures):

```rust
enum Matcher {
    Tool(String),         // exact tool name (today's behavior)
    BashProgram(String),  // matches if ANY sub-command program == this
}

impl DenyRule {
    pub fn tool(name: impl Into<String>) -> Self;          // unchanged
    pub fn bash_command(program: impl Into<String>) -> Self; // NEW
}
```

`matches("Bash", args)` for a `BashProgram` matcher reads `args["command"]`,
runs it through `split_operators` + `resolve_command`, and returns true if any
resolved program equals the target. **Satisfies AC-1.**

### Component 3 — `GuardRule` + destructive breaker

```rust
pub struct GuardRule {
    matcher: GuardMatcher, // reuses command_match; incl. destructive predicates
    action: GuardAction,
}

#[non_exhaustive]
pub enum GuardAction {
    Deny { reason: String },
    Ask  { prompt: String },
}
```

`GuardMatcher` is an enum (Clone/Eq-friendly) covering both program-level
matches and the built-in destructive predicates, e.g.
`RmRecursiveForce { target: RootOrHome }` and
`ProtectedPathWrite`. Protected-path writes are matched against:
- Bash redirection targets (`>`, `>>`, `tee`, `dd of=`) and destructive
  programs operating on a protected prefix, and
- the `path` argument of the Write / Edit tools.

against a **curated protected-prefix list** (`/etc`, `/usr`, `/bin`, `/sbin`,
`/sys`, `/boot`, `/dev`, root `/`, home `~`). This ships as a curated list with
an extension hook — **not** a configurable policy language (v1 scope call).

**Pipeline change** — `authorize()` gains two steps before mode:

```
1.  user deny rules            → Deny            (existing; beats Bypass)
1a. BUILT-IN destructive guards → Deny | Ask     (always-on unless opted out)
1b. user guard rules           → Deny | Ask
2.  mode (Bypass => Allow, Plan, AcceptEdits)
3.  policy (canUseTool)
4.  AskUser → approval handler (default Deny)
```

Steps 1a/1b run **before** mode, so a destructive command is gated even under
`Bypass`. `Ask` resolves through the existing approval-handler path (default
Deny when no handler is installed) — the same machinery step 4 already uses.
Default action for the built-in destructive set is `Ask` (so a deliberate
operator with an approval handler can confirm; absent a handler it denies).
**Satisfies AC-2** (`rm -rf /` prompts/denies under Bypass).

**Enablement** — built-in guards are always consulted unless
`RunContext::without_default_guards()` is called. New `RunContext` state:
`guard_rules: Vec<GuardRule>` (user) + a `default_guards: bool` flag (default
true), both carried through `handoff_child` / `subagent_child` /
`to_tool_context` alongside the existing permission fields (deny rules, mode,
policy, approval handler) — so a sub-run inherits the same guard posture as its
parent.

### Component 4 — secret redaction (core, always-on `PostToolUse` step)

```rust
/// Walk a JSON value; rewrite secret-shaped substrings inside every string.
pub fn redact(value: &Value, extra_secrets: &[String]) -> Value;
```

Two matchers, applied to every string in the tool's output JSON:

1. **Key-name scan** — `KEY=val`, `KEY: val`, `export KEY=val` where `KEY`
   ends (case-insensitive) in `_API_KEY` / `_TOKEN` / `_SECRET` / `_PASSWORD`
   / `_CREDENTIAL`; the value is replaced with `***`.
2. **Value scan** — literal occurrences of known secret values are replaced
   with `***`. The known-value set is sourced **automatically** by scanning the
   **parent process environment** for variable names matching the same
   suffixes and taking their values (a superset of any subprocess's
   `env_allowlist`, so no threading from `BashTool` is needed). The application
   may extend the set via `RunContext` config (`extra_secrets`).

**Wiring** — applied in `run_tools_concurrent` as the **final** transform on
the output JSON, *after* user `PostToolUse` hooks run (so a user hook that
reshapes output cannot reintroduce an unredacted secret), gated by a default-on
flag with a `RunContext` opt-out. **Satisfies AC-3.**

Empty-value edge cases (e.g. `KEY=`), already-`***` values, and non-string JSON
nodes are pass-through. Redaction never errors; worst case it is a no-op.

### Component 5 — `BashTool` (tools)

`BashTool`'s tool-local `deny_commands` / `allow_commands` (defense-in-depth,
evaluated inside `invoke` *after* authorize) switch from
`split_whitespace().next()` to core's `command_match` resolution, so the
run-level (`DenyRule`) and tool-local layers agree on what a command's program
is. No change to the allow/deny *semantics*, only to how the program token is
resolved (now operator- and wrapper-aware).

## Testing → AC mapping

| Test | Location | AC |
|------|----------|----|
| `split_operators` / `resolve_command` units (operators, wrappers, env-assignment, quoting, substitution-limitation) | `core` `command_match` | AC-1 |
| `DenyRule::bash_command("rm")` blocks `echo ok && rm -rf .`; allows `echo ok` | `core` `permission` | AC-1 |
| Built-in destructive guard denies/asks `rm -rf /` under `Bypass`, with and without an approval handler; `without_default_guards()` disables it | `core` `control` | AC-2 |
| `redact` units: key-name (`KEY=`, `KEY:`, `export`), env-value match, nested JSON, idempotence | `core` `redaction` | AC-3 |
| `BashTool` integration: command echoing `FOO_API_KEY=secret` returns `***` in output | `tools` `tests/bash.rs` | AC-3 |
| `BashTool` deny list blocks `nice rm -rf .` / `timeout 5 rm x` (wrapper-stripped) | `tools` `tests/bash.rs` | AC-1 |

## Documentation

- `docs/book/src/concepts/permissions-guardrails-hooks.md` — guard rules
  (Deny/Ask, pre-mode, beats Bypass), the always-on destructive defaults +
  `without_default_guards()`, secret redaction defaults + opt-out, and the
  command-substitution / protected-path scope limitations.
- `crates/paigasus-helikon-tools/README.md` — operator-aware Bash deny matching
  and the secret-redaction default.
- facade `src/lib.rs` doc'd re-exports for any new public types
  (`GuardRule`, `GuardAction`).

## Out of scope (v1)

- Parsing into command substitution `$(…)` / backticks (documented limitation).
- A configurable protected-path *policy language* — v1 ships a curated prefix
  list with an extension hook.
- Redacting secrets that are neither key-labeled in output nor sourced from a
  suffix-matching env var (e.g. a bare high-entropy token with no context).
