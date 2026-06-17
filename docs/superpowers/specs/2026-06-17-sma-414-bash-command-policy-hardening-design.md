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
  so they already override `Bypass` (test `deny_rule_beats_bypass`,
  `control.rs:120`). `Bypass` returns `Allow` before the policy
  (`control.rs:128`); with no policy installed, `Default` mode also returns
  `Allow` (`control.rs:141`). So no destructive floor exists today.
- **`DenyRule`** — `core/src/permission.rs`. Matches by exact tool name via a
  **private** `tool: String` field; `matches(&self, tool, _args)` carries an
  unused `_args` param documented as *"reserved for richer (arg-aware) matchers
  in a later ticket."* This is that ticket.
- **`BashTool`** — `tools/src/bash.rs`. Has its own tool-local
  `deny_commands` / `allow_commands` lists, matched via
  `command.split_whitespace().next()` (first token only). Containment is
  delegated to the `ExecutionBackend`; the host backend already has an
  `env_allowlist` (SMA-328), whose values are read from `std::env::var(name)`.
- **Hook seam** — `core/src/agent.rs::run_tools_concurrent` runs
  `PreToolUse → authorize → invoke → PostToolUse`. The interceptors borrow the
  **`RunContext`** (`agent.rs:771`); `authorize` reads its `deny_rules()` /
  `permission_mode()` / `permission_policy()` / `approval_handler()` directly.
  `PostToolUse` supports `ReplaceOutput`, applied to `final_json`
  (`agent.rs:617`) *before* `tool_output_to_content_parts` renders it for the
  model — that same `final_json` becomes the session-trajectory `ToolResult`
  Item. The *output guardrail* seam (`run_output_guardrails`,
  `control.rs:78–87`) only sees final **model** text, so it is the wrong seam
  for tool-output redaction — `PostToolUse`/`final_json` is correct.

## Design decisions

| # | Decision |
|---|----------|
| D1 | The shell tokenizer and the arg-aware matcher live in **core** (extend the existing `DenyRule._args` breadcrumb). |
| D2 | The breaker is a **pre-mode guard rule** that can `Deny` **or** `Ask` (resolved via the approval handler, default-Deny), evaluated before mode so it beats `Bypass`. |
| D3 | The built-in destructive guard set is **always-on**, with an explicit `RunContext::without_default_guards()` opt-out. |
| D4 | Redaction matches **both** key-name patterns in text **and** known secret env values; it lives in **core** as an always-on `PostToolUse` step with an opt-out. |
| D5 | The breaker defeats wrapper/re-entry bypasses: strip `sudo`/`doas`, unquote the program token, and recurse into `bash -c`/`sh -c` (depth-bounded). `find -delete`/`xargs`/`eval "$VAR"` are documented + tested known bypasses. |
| D6 | Protected-path-write detection uses a **full redirection-aware scanner** (first-class in v1), not whitespace splitting. |

## Architecture

### Crate touch-map & release sequencing

| Crate | Changes |
|-------|---------|
| **paigasus-helikon-core** | new `command_match` module (redirection-aware tokenizer); arg-aware `DenyRule`; new `GuardRule` / `GuardAction`; `authorize()` guard step; new `redaction` module; `agent.rs` post-tool redaction step; `RunContext` config (`guard_rules`, `without_default_guards`, redaction opt-out + extra-secrets). |
| **paigasus-helikon-tools** | `BashTool` deny/allow lists become operator-aware via core's `command_match`, with a defined compound composition rule; crate README + docs. |
| **paigasus-helikon (facade)** | re-export new public types (`GuardRule`, `GuardAction`); bump. |
| **docs/book + READMEs** | `concepts/permissions-guardrails-hooks.md`; tools `README.md`. |

Because **tools consumes new core API in the same PR**, this triggers the
CLAUDE.md "same-PR core bump + facade bump" rule: bump
`paigasus-helikon-core` (patch — the additions are new items + new variants on
already-`#[non_exhaustive]` enums; `DenyRule`'s internal refactor is non-breaking
because its `tool` field is **private**), update its `[workspace.dependencies]`
pin + CHANGELOG, and bump the facade so it republishes with current sibling
reqs. release-plz then publishes core → tools → facade in dependency order.

### Component 1 — `command_match` module (core, pure, dependency-free)

The security-critical heart of the feature. A redirection- and quote-aware
tokenizer, built strictly test-first. No new third-party deps.

```rust
/// One sub-command of a compound command, after wrapper stripping.
pub struct ResolvedCommand {
    pub program: String,            // effective program, unquoted/unescaped
    pub args: Vec<String>,          // remaining argument tokens (unquoted)
    pub redirects: Vec<Redirect>,   // parsed redirections (target paths)
}

pub struct Redirect { pub op: RedirectOp, pub target: String } // > >> 2> &> >& handled by guards

/// Split a compound command on shell control operators, quote- AND
/// redirection-aware. Control operators: && || ; | |& & newlines.
/// MUST NOT split the `&` inside fd-dup redirections (2>&1, >&2, &>file).
pub fn split_operators(command: &str) -> Vec<&str>;

/// Resolve one segment to its effective command: strip leading env
/// assignments (FOO=bar) and fixed wrappers (timeout, nice, nohup, stdbuf,
/// env, command, sudo, doas) with their flag args; unquote/unescape the
/// program token; parse redirections (incl. glued targets like `>/etc/x`
/// and quoted targets `>"/etc/x"`). Returns None for an empty segment.
pub fn resolve_command(segment: &str) -> Option<ResolvedCommand>;

/// Expand `bash -c '<str>'` / `sh -c` / `zsh -c` one level: if `program` is a
/// known shell with a `-c` argument, return the inner command string for
/// re-parsing. Callers bound recursion (MAX_REENTRY_DEPTH = 3).
pub fn shell_c_payload(cmd: &ResolvedCommand) -> Option<&str>;
```

**Coverage (deliberate, enumerated — caught vs. documented bypass):**

- **Caught:** control-operator splitting; wrapper strip incl. `sudo`/`doas`;
  program-token unquote (`\rm`, `'rm'`, `r''m` → `rm`); redirection targets
  (spaced, glued, quoted, fd-prefixed `2>`); one-level-per-step recursion into
  `bash -c '…'` / `sh -c '…'` up to `MAX_REENTRY_DEPTH`.
- **Documented + tested bypasses (v1 does not parse these):** `find / -delete`
  and other program-internal destructive semantics; `xargs rm -rf`; `eval`/
  variable-indirect command strings (`eval "$VAR"`); command substitution
  `$(…)` / backticks; shell *expansion* of globs and variables (`/*`,
  `$HOME`) — the matcher sees the literal pre-expansion token, never the
  shell's expanded form. These are listed in the rustdoc and the concept page,
  with explicit tests asserting they pass through, so the gap is visible rather
  than implied.

### Component 2 — arg-aware `DenyRule`

`DenyRule` keeps `Debug + Clone + PartialEq + Eq` by modeling the matcher as an
enum (no boxed closures). The refactor is API-non-breaking because the existing
field is private.

```rust
enum Matcher {
    Tool(String),         // exact tool name (today's behavior)
    BashProgram(String),  // matches if ANY sub-command program == this
}

impl DenyRule {
    pub fn tool(name: impl Into<String>) -> Self;            // unchanged
    pub fn bash_command(program: impl Into<String>) -> Self; // NEW
}
```

`BashProgram` matching is **tool-scoped**: it engages only when `tool == "Bash"`
and reads `args["command"]`, so a non-Bash tool that happens to carry a
`command` field cannot trip it. It runs the string through `split_operators` +
`resolve_command` (+ one-level `bash -c` recursion) and returns true if any
resolved program equals the target. **Satisfies AC-1.**

### Component 3 — `GuardRule` + destructive breaker

```rust
pub struct GuardRule {
    matcher: GuardMatcher,
    action: GuardAction,
}

#[non_exhaustive]
pub enum GuardAction {
    Deny { reason: String },
    Ask  { prompt: String },
}
```

`GuardMatcher` is an enum (Clone/Eq-friendly), **tool-scoped** like `DenyRule`,
covering the built-in destructive predicates:

- `RmRecursive { target: RootOrHome }` — keys on resolved program `rm` with a
  recursive **and** force flag (bundled or split: `-rf`, `-fr`, `-R … -f`,
  `--recursive --force`, incl. `--no-preserve-root`) and a **literal** target
  token of `/`, `/*`, `~`, `~/`, `${HOME}`-literal. (Variable/glob *expansion*
  is the shell's job and is an enumerated uncaught form — see Component 1.)
  Also flags the classic spacing bug `rm -rf / tmp`.
- `ProtectedPathWrite` — a write whose target resolves under a curated
  protected prefix. Sources of "write": Bash redirection targets (`>`, `>>`,
  `2>`, `&>`, parsed by the Component-1 redirection scanner), `tee`/`dd of=`
  programs, and the **`path` argument of the Write / Edit tools** (structured,
  no shell parsing).

**Protected prefixes:** `/etc`, `/usr`, `/bin`, `/sbin`, `/sys`, `/boot`,
`/dev`, root `/`, home `~`. **Device-node allowlist (checked first, before any
`/dev` rule):** `/dev/null`, `/dev/zero`, `/dev/full`, `/dev/stdout`,
`/dev/stderr`, `/dev/tty`, `/dev/random`, `/dev/urandom` — so the ubiquitous
`cmd > /dev/null` / `2> /dev/null` / `&> /dev/null` idiom is **not** denied.
This ships as a curated list with an extension hook — **not** a configurable
policy language (v1 scope call).

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
Deny when no handler) — the same machinery step 4 uses. The built-in
destructive set defaults to `Ask` (a deliberate operator with an approval
handler can confirm; absent a handler it denies). **Satisfies AC-2.**

**Enablement** — built-in guards are always consulted unless
`RunContext::without_default_guards()` is called. New `RunContext` state:
`guard_rules: Vec<GuardRule>` (user) + a `default_guards: bool` flag (default
true). Both are carried through `handoff_child` / `subagent_child` /
`to_tool_context` alongside the existing permission fields. **Data flow:**
`authorize` reads them from the `RunContext` (the interceptors' borrow), so the
top-level breaker is live as soon as the fields exist on `RunContext`; the
`to_tool_context` projection (a `pub(crate)` carrier) is what keeps **nested
`agent_as_tool` sub-runs** guarded when they rebuild a child `RunContext`.
Missing any copy site silently un-guards that path, so the copy sites
(constructor, `handoff_child`, `subagent_child`, `to_tool_context`, builders)
are consolidated behind a single
`clone_permission_fields(&self) -> PermissionFields` helper, and an explicit
inheritance test asserts both new fields propagate through all three child
paths.

### Component 4 — secret redaction (core, always-on `PostToolUse` step)

```rust
/// Walk a JSON value; rewrite secret-shaped substrings inside every string.
pub fn redact(value: &Value, secrets: &SecretSet) -> Value;

/// Snapshotted once per run (not per tool call).
pub struct SecretSet { values: Vec<String> /* length/entropy-filtered */ }
```

Two matchers, applied to every string in the tool's output JSON:

1. **Key-name scan** — `KEY=val`, `KEY: val`, `export KEY=val` where `KEY`
   ends (case-insensitive) in `_API_KEY` / `_TOKEN` / `_SECRET` / `_PASSWORD`
   / `_CREDENTIAL`; the value is replaced with `***`.
2. **Value scan** — literal occurrences of known secret *values* are replaced
   with `***`. The known-value set is sourced **automatically** by scanning the
   **parent process environment** once at run start for variable names matching
   the same suffixes. To avoid corrupting legitimate output, a value is
   included **only** if it clears a **minimum length (≥ 8 chars) and a
   minimum-entropy / not-a-common-word** filter, and the set is **size-capped**.
   Short/common secrets (`true`, `dev`, `1234`) are dropped from the value scan
   (they remain covered by the key-name scan when key-labeled). The application
   may extend the set via `RunContext` config (`extra_secrets`), subject to the
   same length floor.

**Wiring** — applied in `run_tools_concurrent` as the **final** transform on
`final_json`, *after* user `PostToolUse` hooks (so a user hook reshaping output
cannot reintroduce an unredacted secret), gated by a default-on flag with a
`RunContext` opt-out. Because `final_json` is also the session-trajectory
`ToolResult` Item, redaction covers the model context **and** the persisted
trajectory. **Satisfies AC-3.**

Notes:
- **Hook visibility:** by design user `PostToolUse` hooks observe
  *pre*-redaction output (redaction is the last gate). Documented; acceptable
  for trusted hook code.
- **Trace ordering:** the per-tool span records metadata, not output bodies,
  today — so nothing leaks now. The design records a **redaction-before-trace
  ordering requirement**: any future tracing that captures tool I/O must read
  the redacted `final_json`. A test asserts the recorded `ToolResult` Item is
  redacted.
- Empty values (`KEY=`), already-`***` values, and non-string JSON nodes are
  pass-through. Redaction never errors; worst case it is a no-op.

### Component 5 — `BashTool` (tools)

`BashTool`'s tool-local `deny_commands` / `allow_commands` (defense-in-depth,
evaluated inside `invoke` *after* authorize) switch from
`split_whitespace().next()` to core's `command_match` resolution.

**Compound composition rule (an intended behavior change, not a no-op):**
- **deny** if **any** resolved sub-command's program is in `deny_commands`;
- with an allowlist, **allow only if every** resolved sub-command's program is
  in `allow_commands`.

This is stricter than today's first-token matching: `allow=["git"]` +
`git status && rm -rf .` was previously allowed (first token `git`) and is now
refused (the `rm` sub-command is not allowlisted). The change is called out in
the `BashTool` rustdoc and the tools README.

## Behavior changes & migration

This release adds an **always-on destructive floor**. In `Default` mode with no
policy and no approval handler — the common headless/CI setup — a command the
built-in guards match (`rm -rf /`, `rm -rf ~`, protected-path writes) changes
from **Allow** to **Ask → Deny**. This is intended hardening, but it is a
**runtime behavior change** on a version that release-plz will tag as a 0.x
*patch*. Mitigations:

- A prominent `CHANGELOG` entry under core (and the facade) labeled
  **"Behavior change: destructive-command floor is now on by default."**
- Migration note: install an `ApprovalHandler` to convert the floor from Deny to
  interactive Ask, or call `RunContext::without_default_guards()` to restore the
  prior unguarded behavior.
- The device-node allowlist (Component 3) keeps the break limited to genuinely
  dangerous commands, so benign traffic (`> /dev/null`) is unaffected.

## Testing → AC mapping

| Test | Location | AC |
|------|----------|----|
| `split_operators`: control ops; **does not** mis-split `2>&1` / `&>` / `>&`; glued/quoted redirect targets tokenize | `core` `command_match` | AC-1 |
| `resolve_command`: env-assignment + wrapper strip incl. `sudo`/`doas`; program unquote (`\rm`, `'rm'`); `bash -c`/`sh -c` payload extraction + depth bound | `core` `command_match` | AC-2 |
| `DenyRule::bash_command("rm")` blocks `echo ok && rm -rf .`; allows `echo ok`; tool-scoped (non-Bash `command` arg ignored) | `core` `permission` | AC-1 |
| Breaker denies/asks `rm -rf /`, `rm -rf ~`, `sudo rm -rf /`, `bash -c 'rm -rf /'` under `Bypass`, with & without handler; `without_default_guards()` disables | `core` `control` | AC-2 |
| Breaker **allows** `echo x > /dev/null`, `cmd 2> /dev/null`; **denies** `echo x > /etc/passwd`, `echo x >/etc/passwd`, `tee "/etc/passwd"` | `core` `control` | AC-2 |
| `rm` form matrix: caught (`-rf`/`-fr`/`-R -f`/`--recursive --force`, `/`, `/*`, `~`, `rm -rf / tmp`); uncaught documented (`find -delete`, `xargs rm`, `eval`, `$HOME`-expanded) | `core` `control` | AC-2 |
| `redact`: key-name (`KEY=`/`KEY:`/`export`), env-value match, **length/entropy floor** (short value not redacted), nested JSON, idempotence, env snapshot-once | `core` `redaction` | AC-3 |
| Recorded session `ToolResult` Item is redacted | `core` `agent`/integration | AC-3 |
| Guard-field inheritance through `handoff_child` / `subagent_child` / `to_tool_context` | `core` `context` | AC-2 |
| `BashTool` integration: `FOO_API_KEY=secret` echo → `***`; deny list blocks `nice rm -rf .` / `timeout 5 rm x`; allowlist composition (`git status && rm -rf .` refused) | `tools` `tests/bash.rs` | AC-1, AC-3 |

## Documentation

- `docs/book/src/concepts/permissions-guardrails-hooks.md` — guard rules
  (Deny/Ask, pre-mode, beats Bypass), the always-on destructive defaults +
  `without_default_guards()`, secret redaction defaults + opt-out, the
  device-node allowlist, the compound allow/deny composition rule, and the
  enumerated scope limitations (re-entry depth, `find`/`xargs`/`eval`, `$(…)`,
  shell expansion).
- `crates/paigasus-helikon-tools/README.md` — operator-aware Bash deny matching,
  composition rule, secret-redaction default.
- facade `src/lib.rs` doc'd re-exports for `GuardRule`, `GuardAction`.

## Out of scope (v1)

- Parsing into command substitution `$(…)` / backticks (documented limitation).
- `find -delete`, `xargs`, `eval "$VAR"` and other variable-indirect command
  strings (documented + tested bypasses).
- Shell-*expanded* glob/variable destructive targets (the matcher sees literal
  pre-expansion tokens).
- A configurable protected-path *policy language* — v1 ships a curated prefix
  list with an extension hook.
- Redacting bare high-entropy tokens that are neither key-labeled in output nor
  sourced from a suffix-matching env var.
