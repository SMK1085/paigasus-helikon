# Staff review — SMA-414 Bash command-policy hardening design

**Reviews:** [`2026-06-17-sma-414-bash-command-policy-hardening-design.md`](2026-06-17-sma-414-bash-command-policy-hardening-design.md)
**Ticket:** [SMA-414](https://linear.app/smaschek/issue/SMA-414/paigasus-helikon-tools-bash-command-policy-hardening-operator-aware)
**Reviewed against:** Linear SMA-414, and the on-disk core/tools code (`control.rs`, `permission.rs`, `context.rs`, `agent.rs::run_tools_concurrent`, `bash.rs`).
**Date:** 2026-06-17

## Verdict

The architecture is sound where it counts: the seam choices are correct (verified
against the code), the pipeline insertion point is clean, and the release mechanics
correctly invoke the CLAUDE.md same-PR core+facade bump rule. The risk in this spec is
not the plumbing — it's the **matcher**. A security circuit breaker built on a pragmatic,
admittedly-incomplete shell tokenizer is simultaneously **bypassable on its headline
threat** and **false-positive-prone on the most common shell idiom**, which is the worst
combination for a control of this kind. The redaction value-scan has a similar
false-positive hazard. None of this is unfixable, but several items need to be resolved in
the design before implementation, not discovered in review.

The blockers are **#1** (`/dev/null` breakage), **#2** (`sudo`/`-c`/quote bypass of the
breaker), and **#5** (redaction corrupting legitimate output). **#3** (a behavior change
shipped as a quiet patch) is a process/communication blocker.

## Critical

### 1. The destructive breaker will deny `cmd > /dev/null` — the most common shell idiom

§Component 3 lists `/dev` in the curated protected-prefix list and matches
`ProtectedPathWrite` against redirection targets (`>`, `>>`, `tee`, `dd of=`). So
`echo x > /dev/null`, `cmd 2> /dev/null`, `cmd &> /dev/null` all resolve to "write under
`/dev`" → `Ask` → (no approval handler) → **Deny**. `/dev/null` redirection appears in a
huge fraction of real commands; denying it by default would make the guarded posture
unusable and would fire on benign agent traffic constantly.

**Fix:** carve out an explicit device-node allowlist (`/dev/null`, `/dev/zero`,
`/dev/stdout`, `/dev/stderr`, `/dev/tty`, `/dev/random`, `/dev/urandom`) before any
`/dev`-prefixed write rule — and add these as test cases.

### 2. The breaker is bypassable on `rm -rf /` itself

`resolve_command` strips a fixed wrapper set (`timeout, nice, nohup, stdbuf, env,
command`) and `RmRecursiveForce` keys on the resolved *program* token. That leaves the
canonical dangerous forms uncaught:

- **`sudo rm -rf /`** / `doas rm -rf /` — `sudo` is not in the strip set, so the program
  resolves to `sudo`, not `rm`, and the predicate never fires. This is *the* command the
  feature exists to stop.
- **`bash -c 'rm -rf /'`** / `sh -c …` / `xargs rm -rf /` / `find / -delete` — re-entry
  and exec-wrappers hide the target program entirely.
- **Quoted/escaped program token** — `\rm -rf /`, `'rm' -rf /`, `r''m -rf /`. The spec
  states the tokenizer is "not a full quote-removal pass," but doesn't connect that this
  means **quoting the program name bypasses both deny rules and the breaker**.

A breaker that misses `sudo rm -rf /` provides *false* assurance, which is worse than a
documented absence. **Fix:** strip `sudo`/`doas`; give the program token a minimal
unquote/unescape pass before comparison; and either look through `bash -c`/`sh -c` or
document these as explicit, tested bypasses alongside the `$(…)` gap (don't leave them
implicit).

### 3. A default-posture behavior change is shipped as a quiet core *patch*

Today, in `Default` mode with no policy, `authorize()` returns `Allow`
(confirmed `control.rs:141`). After this change, `rm -rf /` (and any matched
destructive/protected-path command) becomes `Ask` → `Deny` when no approval handler is
installed. That is the intended hardening, but it is a **runtime behavior break** for
existing headless/CI deployments that run without an approval handler — previously-working
commands start failing on upgrade. Mechanically release-plz will tag a `feat` as a 0.x
**patch**, but framing the whole change as "additive (patch)" undersells it, and combined
with #1 it can break live agents silently on a version that looks like a no-op bump.

**Fix:** call this out as a behavior change with a prominent CHANGELOG entry + migration
note (`without_default_guards()` / install an approval handler), and resolve #1 so the
break is limited to genuinely dangerous commands.

## Moderate

### 4. The tokenizer isn't redirection-aware, which both misses and mis-splits

Two coupled problems the spec doesn't address (it only flags `$(…)`):

- **Splitting on bare `&`** must not break `2>&1`, `&>`, `>&` — naive operator splitting
  turns `cmd 2>&1` into `cmd 2>` + `1`.
- **Redirection targets glued to the operator** (`>/etc/passwd`, `2>/etc/x`,
  `>"/etc/passwd"`) won't tokenize as a separate `>` token under whitespace splitting, so
  protected-path-write detection silently misses them.

Robust redirection detection needs a small redirection scanner, not whitespace splitting —
and Component 3 needs that scanner anyway. **Fix:** either build the redirection-aware pass
or scope v1's breaker to program-level predicates (`rm -rf /` / `~`) only and defer
protected-path-write-via-redirection with an explicit note.

### 5. Redaction value-scan has no length/entropy floor — it will corrupt output

§Component 4 matcher 2 auto-sources secret *values* from parent-env vars whose names match
the suffixes, then replaces every literal occurrence in tool output. If any such var holds
a short or common value (`1234`, `true`, `dev`, a dictionary word), **every occurrence of
that substring in every tool's output** — file contents, code, logs — becomes `***`. That
silently corrupts legitimate output and is hard to debug.

**Fix:** apply value-scanning only to values above a minimum length / entropy threshold,
cap the value-set size, and snapshot the env once rather than re-scanning per call.

### 6. Compound-command allow/deny composition is unspecified, and "no semantic change" is inaccurate

§Component 5 says switching `BashTool`'s tool-local lists to `command_match` is "no change
to the allow/deny semantics." It is a change: with operator-awareness, an allowlisted
first program no longer green-lights a trailing sub-command
(`allow=["git"]` + `git status && rm -rf .` was allowed by first-token matching, now isn't).
That's a desirable change, but the spec must **define the composition rule** — deny if
*any* sub-command matches; allow only if *all* sub-commands are allowed — and present it as
an intended behavior change, not a no-op.

### 7. `rm -rf /` form coverage is under-specified for something called a "circuit breaker"

`RmRecursiveForce { target: RootOrHome }` gets one line, but the catchable surface is
fiddly: `/*` globs, `--no-preserve-root`, `$HOME`/`${HOME}`/`~/`, flag bundling
(`-rf`/`-fr`/`-R`), and the classic `rm -rf / tmp` spacing bug. A partial matcher
advertised as a breaker invites false confidence. **Fix:** enumerate caught vs. uncaught
forms explicitly in the rustdoc and pin them in the test matrix.

### 8. Guard inheritance is manual and duplicated — easy to half-wire, silent when wrong

`context.rs` hand-copies the four permission fields in **three** places —
`handoff_child` (l.192), `subagent_child` (l.216), and `to_tool_context` (l.301) — plus the
constructor and builders. Adding `guard_rules` + `default_guards` means updating all of
them. Missing `to_tool_context` makes the breaker **inert at the actual authorize site**
(that's the context the interceptors use); missing a child path means a subagent silently
runs unguarded. The failure mode is a silently-absent security control.

**Fix:** add an explicit inheritance test asserting both new fields propagate through all
three paths, and consider a single `clone_permission_fields` helper so the copy sites can't
drift.

## Minor

### 9. The ticket's "/logs" clause isn't fully closed by the chosen seam

Redaction at `final_json` (before `tool_output_to_content_parts`, `agent.rs:617`) correctly
covers the model context **and** the trajectory/session `ToolResult` Item — good, that's
the right insertion point. But the ticket says "before it re-enters the conversation
context/**logs**," and the per-tool span today records metadata, not output bodies. If
tracing is later configured to capture tool I/O (Langfuse input/output capture), raw output
could reach the trace before redaction. **Fix:** add a test asserting the recorded
`ToolResult` Item is redacted, and note the redaction-before-trace ordering requirement.

### 10. User `PostToolUse` hooks observe pre-redaction output

By design, redaction runs *after* user hooks (so they can't reintroduce a secret) — but it
also means user hook code sees the raw secret. Acceptable for trusted hooks; document it.

### 11. The guard matcher must be tool-scoped

Steps 1a/1b run in `authorize()` for *every* tool. Ensure the bash-parsing predicates only
engage when `tool == "Bash"` (reading `args["command"]`), so a non-Bash tool that happens
to carry a `command` field can't misfire the breaker.

### 12. `DenyRule` is not `#[non_exhaustive]`

The enum-matcher refactor is non-breaking only because the `tool` field is **private**, not
because of `#[non_exhaustive]` (the spec's stated justification). Cosmetic, but worth
getting right in the rationale.

## What the spec got right

- **Seam choice is correct and verified.** Output guardrails only see final *model* text
  (`control.rs:78–87`), so `PostToolUse`/`final_json` is the right place for tool-output
  redaction — and it happens to also cover the session trajectory Item.
- **Pipeline insertion is clean.** Deny rules already run before mode (`control.rs:120`),
  so slotting guards at 1a/1b before mode makes them beat `Bypass` without touching the
  sticky-Bypass invariant in `with_permission_mode` (`context.rs:246`).
- **`Clone + Eq` rules via enums, not boxed closures** — the right call for a Debug/Eq-able
  rule type; keeps `DenyRule`/`GuardRule` comparable and testable.
- **Release mechanics are right.** Tools consumes new core API in the same PR, so the spec
  correctly invokes the CLAUDE.md same-PR core bump + workspace-pin + facade bump (the
  SMA-321/346 deadlock-and-cascade caveat). This is the part teams most often get wrong; it's
  handled.
- **`$(…)` substitution documented as a known gap** rather than passed off as covered —
  the right instinct; extend the same honesty to the bypasses in #2.

## Suggested next actions

1. Device-node allowlist before any `/dev` write rule (#1).
2. Handle `sudo`/`doas`, `-c` re-entry, and program-token unquoting — or scope the v1
   breaker to what it can actually catch and document the rest as tested bypasses (#2, #7).
3. Reframe the release as a behavior change with a loud CHANGELOG + migration note (#3).
4. Decide: real redirection-aware tokenizer, or defer protected-path-write-via-redirection
   (#4); and specify the compound-command allow/deny composition rule (#6).
5. Add a length/entropy floor to the redaction value-scan (#5).
6. Add guard-inheritance tests across `handoff_child` / `subagent_child` / `to_tool_context`
   (#8).
