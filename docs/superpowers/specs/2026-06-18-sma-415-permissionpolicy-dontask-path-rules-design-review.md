# Staff review — SMA-415 PermissionPolicy: DontAsk + filesystem path rules

**Reviews:** [`2026-06-18-sma-415-permissionpolicy-dontask-path-rules-design.md`](2026-06-18-sma-415-permissionpolicy-dontask-path-rules-design.md)
**Ticket:** [SMA-415](https://linear.app/smaschek/issue/SMA-415)
**Reviewed against:** Linear SMA-415, and the on-disk core code (`permission.rs`, `control.rs`, `context.rs`, `tool.rs::PermissionFields`, `agent_as_tool.rs`).
**Date:** 2026-06-18

## Verdict

The pipeline design is correct and the propagation story is the most careful of the three
specs in this series — it enumerates all four context copy sites and explicitly applies the
SMA-414 "missing one is fail-open" lesson (verified: `agent_as_tool.rs` already crosses
mode/deny/guard/policy/handler at lines 129–144, and `allow_rules` is the one field it
still needs). `DontAsk` as a `#[non_exhaustive]` variant is clean and non-breaking.

The risks are concentrated in three places: a **foundational-crate dependency choice**
(`ignore` on core), the **path-matcher's fitness as a security control** (case-sensitivity,
`..`, segment semantics), and one **policy-defeating footgun** in how allow rules
short-circuit. The blockers I'd resolve before implementation are **#1** (the `ignore`
dependency), **#2** (case/normalization bypasses), and **#4** (allow-rule short-circuit
scope).

## Critical

### 1. `ignore` on `core` is the wrong tool, in the wrong crate

Decision #4 picks `ignore::gitignore` for exact gitignore semantics and the spec itself
notes it drags `walkdir`, `crossbeam`, `globset`, `regex-automata` into core. That's a lot
of surface — including threading primitives (`crossbeam`) and a directory walker
(`walkdir`) — added to the **most-depended-on crate in the workspace**, to do what is
fundamentally *match one path string against one glob*. `ignore` is ripgrep's directory
traversal engine; using it purely via `matched_path_or_any_parents` with no walk is a
sledgehammer.

`globset` (already a transitive dep of `ignore`) provides the glob matching directly, and
the three gitignore behaviors actually needed here (bare-name-at-any-depth, `**`, leading
anchor) are a small amount of logic on top of it. Pulling the full `ignore` stack into core
is the kind of decision that's painful to reverse once consumers and the SBOM bake it in.
The spec flags it "for review" but lists it as settled — I'd push back: use `globset`
directly, or feature-gate the path-rule API so non-permission users of core don't pay the
weight. **This is the highest-leverage thing to change now, because it's the hardest to
change later.**

### 2. The path matcher is case-sensitive and `..`-blind, so it fails as a secret/scope control

The matcher is lexical on the raw `path` arg (core has no root — correctly noted). But the
spec only normalizes a leading `./`, which leaves two real bypasses for what AC2 frames as a
containment guarantee:

- **Case sensitivity.** gitignore matching is case-sensitive by default, so
  `DenyRule::read(".env")` does **not** match `.Env` or `.ENV` — which resolve to the *same
  file* on macOS and Windows (case-insensitive filesystems). A secret-protection deny rule
  that a trivial case change defeats is a weak control. Set `GitignoreBuilder` case-insensitive
  (at minimum on case-insensitive targets).
- **`..` traversal.** `AllowRule::edit("src/**")` under `DontAsk` is sold as "scope writes
  to the glob," but `src/../.git/config` lexically matches the `src/` prefix and would be
  allowed, while escaping `src`. No `..` collapse happens. The cap-std root in tools is the
  *real* boundary; the path allow-rule is a convenience filter. **Reframe AC2's
  scoping as advisory, not a security boundary,** and collapse `..` before matching so the
  filter at least does what it appears to.

### 3. The `.git`/`.ssh`/`.env` breaker "segment" semantics are under-defined

§Protected-path breaker matches paths "whose path contains a `.git/`, `.ssh/`, or `.env`
segment." "Segment" needs a precise definition or it's either over-broad or bypassable:

- **Bare git repos are conventionally named `name.git/`.** If the match is a substring
  `.git/`, every write under any `*.git/` repo (a very common layout) trips the breaker —
  false positives that would deny legitimate writes.
- **Trailing-file cases.** `.env.local` should trip; `environment.env` and `.gitignore`
  should **not** (no `.git`/`.env` path component). A substring test gets these wrong.

**Fix:** define "segment" as an exact path *component* (`split('/')`, compare each), match
`.env` and `.env.*` as a final component, and add tests for `name.git/config`,
`environment.env`, `.env.local`, and `.gitignore` to pin the boundaries.

## Moderate

### 4. Allow rules short-circuit the policy in *all* modes — a policy-defeating footgun

Decision #2 / pipeline step 3: a matching allow rule resolves to `Allow` in any mode,
*before* the policy (step 5). Deny short-circuit is fail-closed and safe; allow
short-circuit is **fail-open relative to `canUseTool`**. Concretely: to make Bash usable
under `DontAsk` you must add `AllowRule::tool("Bash")` (there's no `AllowRule::bash_command`
— see #7), and that same rule then silently disables the Bash policy in `Default`,
`AcceptEdits`, and `Plan` too. A user who adds an allow rule for a headless sub-run can
neuter their interactive policy's per-arg Bash checks without realizing it. The guard/deny
steps still fire, so truly destructive commands are caught — but the policy's nuanced checks
are bypassed.

**Fix:** either scope allow-rule short-circuit to `DontAsk` (where it's actually needed), or
document loudly that an allow rule is a global, all-modes, per-tool policy override — and
add `AllowRule::bash_command` so `DontAsk` Bash isn't all-or-nothing.

### 5. Stickiness blocks *tightening*, which is the security-wrong direction

Decision #3 ("incumbent wins") means a `Bypass` parent can't hand a child a stricter
`DontAsk`. The spec accepts this for v1, but it's backwards: you always want to permit
moving toward *more* restrictive. The original invariant (in `permission.rs`) existed to
stop a child *escaping* `Bypass` — i.e. to block *loosening*. Folding `DontAsk` into the
same `matches!` guard throws away the ability to tighten, and the headline use case (lock
down an untrusted subagent spawned from a permissive parent) becomes unreachable.

**Fix:** model modes as a restrictiveness lattice and allow only-tighten transitions (or at
minimum permit `Bypass → DontAsk`). "First terminal mode wins" is the wrong mental model for
security modes.

### 6. Hard dependency on SMA-414 with no Linear blocking link

SMA-415 reuses `GuardRule::destructive_defaults()`, the guard pipeline step, `PermissionFields`,
`without_default_guards()`, and the four-site propagation — all SMA-414 surface (present in
the working tree, but SMA-414 is still "In Progress" and has open blockers from its own
review). Linear shows **no `blockedBy` SMA-415 → SMA-414**, so the two can merge out of
order, and if SMA-414's breaker action or pipeline shifts in review, SMA-415 is rebasing on
moving ground (it even *extends* `destructive_defaults()` with `ProtectedDotPathWrite`).

**Fix:** add the blocking relation and sequence the merges; or, if intentionally
co-developed, say so in the spec and pin the shared surface.

## Minor

### 7. Allow-rule granularity is asymmetric with deny

`DenyRule` has `bash_command` (per-program) but `AllowRule` does not (out of scope), so
`DontAsk` + Bash is all-or-nothing — likely the exact thing the SDA worker (SMA-265) that
motivated this will need. Flagged so it isn't a surprise fast-follow. (Pairs with #4.)

### 8. `PartialEq` on pattern-string-plus-kind can surprise

Equality compares the pattern string + kind, ignoring the compiled matcher, and `./`
normalization happens at match time, not in storage. So `DenyRule::read(".env")` and
`DenyRule::read("./.env")` compile to the same matcher but compare unequal, which can defeat
dedup or rule-set equality checks. Normalize the stored pattern, or document it.

### 9. Confirm the facade re-export ships in the same release

The release section says "no manual core bump," which is right for *core*, but `AllowRule`
must be re-exported from the facade in the same release or it's unreachable through
`paigasus-helikon` until a later PR (the SMA-346 facade-drift pattern). release-plz's
cascade should bump the facade automatically since core bumps via the normal flow — just
make the same-release facade re-export explicit in the release section, not only in docs.

## What the spec got right

- **Four-copy-site propagation is correctly and completely enumerated** — `handoff_child`,
  `subagent_child`, `PermissionFields → to_tool_context`, and the `agent_as_tool` sub_ctx
  rebuild (verified at `agent_as_tool.rs:121–144`, which already crosses mode/deny/guard/
  policy/handler; `allow_rules` is the one missing field). Mandating the Test D extension to
  assert `DontAsk` *and* `allow_rules` cross is exactly the right regression guard, and the
  note that the sub_ctx rebuild reconstructs mode explicitly is a sharp observation.
- **Minimal new surface:** all denies stay in `DenyRule` (no new copy-site), one new
  `allow_rules` field. Good instinct.
- **Pipeline ordering proves the ACs:** the breaker (step 2) runs before both the
  `AcceptEdits` auto-allow and the allow rules, so a `.git/` write is refused even with a
  user `AllowRule::edit(".git/**")`. Sound.
- **AC1 via a policy that panics if called** is a clean, unambiguous proof that `canUseTool`
  is unreachable under `DontAsk`.
- **`matched_path_or_any_parents(path, false)`** is the correct way to make a `.git` pattern
  catch `.git/config` without a filesystem walk.

## Suggested next actions

1. Replace `ignore` with `globset` on core, or justify the transitive weight on the
   foundational crate (#1).
2. Enable case-insensitive matching and `..` normalization; reframe allow-path scoping as
   advisory, not a boundary (#2).
3. Define the breaker "segment" as an exact path component and test the bare-repo /
   `.env.local` / `environment.env` boundaries (#3).
4. Decide the allow-rule short-circuit scope and add `AllowRule::bash_command` (#4, #7).
5. Make permission modes a tighten-only lattice (#5).
6. Add the `blockedBy` SMA-415 → SMA-414 relation and sequence the merges (#6).
