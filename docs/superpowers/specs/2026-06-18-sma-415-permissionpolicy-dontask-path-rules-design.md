# SMA-415 — PermissionPolicy enhancements: DontAsk mode + filesystem path rules

**Status:** Design approved (2026-06-18)
**Crate:** `paigasus-helikon-core`
**Linear:** [SMA-415](https://linear.app/smaschek/issue/SMA-415)
**Builds on:** SMA-326 (PermissionPolicy / PermissionMode), SMA-414 (operator-aware deny matching, destructive breaker), SMA-328 (cap-std root in the tools crate)

## Problem

Two gaps in the permission layer surfaced by the standard-tools safeguards review:

1. **No locked-down headless mode.** `PermissionMode` has `Bypass` (allow-all) but no inverse: a non-interactive run where an `AskUser` prompt can never be answered currently has to choose between an unattended permissive policy or `Bypass`. This is the gap that forced `--permission-mode bypassPermissions` in the SDA worker (SMA-265).
2. **No per-path filesystem policy.** cap-std bounds the *root* (in `paigasus-helikon-tools`) but cannot express per-path allow/deny. There is no way to say "deny reading `.env`" or "scope writes to `src/**`", and nothing stops an auto-write to `.git/` under `AcceptEdits`.

## Acceptance criteria (from the ticket)

- **AC1 — DontAsk:** a tool call with no matching allow rule is denied without invoking `canUseTool`.
- **AC2 — path rules:** a `Read(.env)` deny rule blocks reads of `.env` at any depth; an `Edit` allow rule scopes writes to the configured glob.
- **AC3 — breaker:** a write to `.git/` is refused even under `AcceptEdits`.

## Key design decisions

These were settled during brainstorming (2026-06-18):

| # | Decision | Choice |
|---|----------|--------|
| 1 | Allow-rule scope | **General + path.** `AllowRule::tool("X")` *and* filesystem `read`/`edit` globs — so `DontAsk` is a usable general lockdown, not filesystem-only. |
| 2 | Allow-rule reach | **Short-circuit in all modes**, symmetric with deny rules. A matching allow rule resolves to `Allow` after deny+guard, in any mode — including pre-empting the policy in `Default` mode. |
| 3 | DontAsk stickiness | **Sticky, incumbent wins.** Once `Bypass` *or* `DontAsk` is set, `with_permission_mode` refuses to move off it. First-set terminal mode wins. |
| 4 | Glob engine | **`ignore::gitignore`** — exact gitignore semantics (bare-name-at-any-depth, `**`, anchoring), queried without a tree walk. |
| 5 | Breaker action | **Ask** (consistent with the existing absolute-path write guard). Headless → Deny; with handler → prompt; disabled by `without_default_guards()`. |

## Existing system (for reference)

The decision pipeline lives in `control.rs::authorize`:

```
deny rules › guard rules › mode › policy (canUseTool) › AskUser
```

- `PermissionMode` — `#[non_exhaustive]` enum: `Default`, `AcceptEdits`, `Plan`, `Bypass`. `Bypass` is sticky and propagates to sub-agents.
- `DenyRule` — deny-only; matches by exact tool name (`DenyRule::tool`) or Bash program (`DenyRule::bash_command`). There is **no allow-rule concept** today.
- `GuardRule` / `GuardRule::destructive_defaults()` — always-on, runs **before** mode (beats `Bypass`), may `Ask`. Already protects *absolute* system-path **writes** (`/etc`, `/usr`, …) with `Ask` semantics.
- `RunContext` carries `permission_mode`, `permission_policy`, `deny_rules`, `approval_handler`, `guard_rules`, `default_guards`. The tools (`Read`/`Write`/`Edit`, names `"Read"`/`"Write"`/`"Edit"`) all use the `path` argument.

**Constraint:** core has **no notion of a working root/cwd** — the cap-std root lives entirely in `paigasus-helikon-tools` (`sandbox.rs`). So path rules in core are **lexical glob matches on the `path` argument**, not filesystem-aware.

## New public API surface (`permission.rs`)

```rust
pub enum PermissionMode {            // existing #[non_exhaustive] enum
    Default, AcceptEdits, Plan, Bypass,
    DontAsk,                         // NEW — deny-by-default; policy never invoked
}

pub struct AllowRule { /* … */ }     // NEW — positive counterpart of DenyRule
impl AllowRule {
    pub fn tool(name: impl Into<String>) -> Self;       // mirrors DenyRule::tool
    pub fn read(pattern: impl Into<String>) -> Self;    // gitignore glob on the Read tool
    pub fn edit(pattern: impl Into<String>) -> Self;    // gitignore glob on Edit/Write
    pub fn matches(&self, tool: &str, args: &serde_json::Value) -> bool;
}

impl DenyRule {                      // EXISTING type, extended
    pub fn read(pattern: impl Into<String>) -> Self;    // NEW path-deny
    pub fn edit(pattern: impl Into<String>) -> Self;    // NEW path-deny
}
```

**Why this shape:** all *denies* stay in `DenyRule` (→ the existing `deny_rules` vec, **no new copy-site**); all *allows* go in a new `AllowRule` (→ one new `allow_rules` vec). `allow_rules` is the *only* new `RunContext` field.

## Revised pipeline (`control.rs::authorize`)

```
1. deny rules        → Deny     (tool / bash_command / read-path / edit-path)  — beats all, incl. Bypass
2. guard rules       → Ask/Deny (destructive defaults + NEW .git/.ssh/.env breaker)  — beats mode
3. allow rules       → Allow    (short-circuit, ANY mode)                        — NEW
4. mode:
     Bypass      → Allow
     Plan        → Deny non-ReadOnly
     AcceptEdits → Allow Write
     DontAsk     → Deny  ("DontAsk: no allow rule matched")   — NEW; policy NOT reached
5. policy (canUseTool)   — never invoked under DontAsk (returned at step 4)
6. AskUser → ApprovalHandler (default Deny)
```

How each AC is met:

- **AC1:** under `DontAsk`, step 4 returns `Deny` before step 5 — `canUseTool` is unreachable. The only path to `Allow` is a step-3 allow rule.
- **AC2:** `DenyRule::read(".env")` fires at step 1 (beats everything); `AllowRule::edit("src/**")` is the only way a write passes under `DontAsk` (step 3), scoping writes to the glob.
- **AC3:** the breaker is a guard (step 2), which runs before the `AcceptEdits` auto-allow (step 4) — so a `.git/` write is refused under `AcceptEdits`. It also runs before allow rules (step 3), so a user `AllowRule::edit(".git/**")` cannot override it.

## Path matching semantics

- Backed by `ignore::gitignore::GitignoreBuilder`, queried via `matched_path_or_any_parents(path, /*is_dir*/ false)` — **no filesystem walk**. So a `.git` (directory) pattern catches `.git/config`.
- Each rule compiles **its own single pattern**. Precedence among rules is the **pipeline's** (deny > allow), *not* gitignore's last-match-wins. We do not promise cross-rule `!`-negation; only single-pattern negation is meaningful.
- Storage: each path-bearing rule holds `pattern: String` + `Arc<Gitignore>` (cheap `Clone`). `PartialEq`/`Eq`/`Debug` compare the **pattern string + kind**, ignoring the compiled matcher — so `DenyRule` retains its derive-style equality usage.
- Tool/arg mapping: `read` matches tool `"Read"`; `edit` matches `"Edit"` **and** `"Write"`; both read the `path` arg — consistent with the existing `ProtectedPathWrite` guard.

**Documented limitation:** anchored patterns (`src/**`) assume the path is expressed relative to the agent's working directory (the cap-std root the tools pin); core has no root, so absolute paths from outside it won't match anchored patterns. Bare-name patterns (`.env`, `*.pem`) match at any depth regardless. Paths are normalized by trimming a leading `./` before matching.

## Protected-path breaker

New `GuardMatcher::ProtectedDotPathWrite` added to `GuardRule::destructive_defaults()`, **Ask** action:

- Matches the `Write`/`Edit` `path` arg, and Bash write-redirects (`>`, `>>`, `tee`, `dd of=`), whose path contains a `.git/`, `.ssh/`, or `.env`(incl. `.env.*`) segment.
- Headless (no handler) → Deny (so AC3 "refused under AcceptEdits" holds); with handler → prompt a human; disabled by `without_default_guards()`.
- **Writes only.** Reading `.env` is the user-configured `DenyRule::read(".env")` example, not a built-in.
- The protected segment set is a fixed constant (like `PROTECTED_PREFIXES`); a configurable/extensible set is out of scope.

## DontAsk stickiness

One-line change in `RunContext::with_permission_mode`:

```rust
if !matches!(self.permission_mode, PermissionMode::Bypass | PermissionMode::DontAsk) {
    self.permission_mode = mode;
}
```

First-set terminal mode wins. (Edge case, accepted for v1: a `Bypass` parent cannot hand a child a stricter `DontAsk` lockdown.)

## RunContext state + propagation

New field `allow_rules: Vec<AllowRule>` with consuming builder `with_allow_rules` and reader `allow_rules()`. It **must** be wired through all **four** copy sites (the SMA-414 lesson — missing one is fail-open):

1. `handoff_child`
2. `subagent_child`
3. `clone_permission_fields` → `PermissionFields` → `to_tool_context`
4. `agent_as_tool::invoke`'s `sub_ctx` rebuild

`DontAsk` (a `PermissionMode` value) already propagates through `permission_mode`, so it needs no new plumbing — but the propagation test must assert it crosses, because the sub_ctx rebuild reconstructs mode explicitly.

## Testing

- **Unit (`permission.rs`):** `AllowRule` tool/read/edit matching; `DenyRule::read`/`edit` path variants; breaker on `.git`/`.ssh`/`.env`/`.env.local`; gitignore depth (`.env` at any depth) and anchoring (`src/**` scoped); `PartialEq` on pattern string.
- **Unit (`control.rs`):** `DontAsk` denies without invoking the policy (use a policy that panics if called); allow rule short-circuits in `Default`/`Plan`/`AcceptEdits`; deny-path beats `Bypass`; breaker beats `AcceptEdits`; an allow rule does **not** override the breaker.
- **Integration:** extend `tests/subagent_propagation.rs` Test D to assert `allow_rules` **and** `DontAsk` cross into the agent-as-tool sub-run (the fail-open regression guard).

## Dependencies, release, docs

- **Dependency:** add `ignore` to `[workspace.dependencies]` (root) and core `Cargo.toml` (`dep.workspace = true`). License is MIT/Unlicense — `deny.toml` allowlist OK — but it pulls `walkdir`/`crossbeam`/`globset`/`regex-automata` into the *foundational* crate. This transitive weight on core is the cost of the exact-semantics choice (decision #4); flagged for review. Verify `cargo deny`, `cargo audit`, and the SBOM workflow stay green.
- **Release:** purely additive. `PermissionMode` is `#[non_exhaustive]`, so the new variant is non-breaking; new types are additive. Normal release-plz patch/minor bump — **no** stub-ascend ritual and **no** manual core bump (this is an already-released crate gaining additive API, not a same-PR core-API consumer).
- **Docs (same PR):**
  - `docs/book/src/concepts/permissions-guardrails-hooks.md` — `DontAsk`, `AllowRule`, path rules, the `.git`/`.ssh`/`.env` breaker, and the anchored-pattern/no-root limitation.
  - `crates/paigasus-helikon-core/README.md` — if its permission example/surface changes.
  - Facade re-export of `AllowRule` (+ a `///` doc comment, or `-D warnings` fails the docs job).

## Out of scope (YAGNI)

- Symlink allow-rule semantics — the ticket explicitly says "if later exposed."
- `AllowRule::bash_command` — `AllowRule::tool("Bash")` covers Bash wholesale for v1.
- A configurable/extensible breaker path set — fixed constant for v1.
- Teaching core about a working root/cwd — path rules stay lexical on the `path` arg.
