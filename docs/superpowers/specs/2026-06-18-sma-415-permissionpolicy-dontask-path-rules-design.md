# SMA-415 — PermissionPolicy enhancements: DontAsk mode + filesystem path rules

**Status:** Design approved (2026-06-18), revised after staff review (2026-06-18)
**Crate:** `paigasus-helikon-core`
**Linear:** [SMA-415](https://linear.app/smaschek/issue/SMA-415)
**Builds on:** SMA-326 (PermissionPolicy / PermissionMode), **SMA-414 (merged — `1fe7743`, PR #101, Done)** for `GuardRule::destructive_defaults()`, the guard pipeline step, `PermissionFields`, `without_default_guards()`, and the four-site propagation; SMA-328 (cap-std root in the tools crate).

## Problem

Two gaps in the permission layer surfaced by the standard-tools safeguards review:

1. **No locked-down headless mode.** `PermissionMode` has `Bypass` (allow-all) but no inverse: a non-interactive run where an `AskUser` prompt can never be answered currently has to choose between an unattended permissive policy or `Bypass`. This is the gap that forced `--permission-mode bypassPermissions` in the SDA worker (SMA-265).
2. **No per-path filesystem policy.** cap-std bounds the *root* (in `paigasus-helikon-tools`) but cannot express per-path allow/deny. There is no way to say "deny reading `.env`" or "scope writes to `src/**`", and nothing stops an auto-write to `.git/` under `AcceptEdits`.

## Acceptance criteria (from the ticket)

- **AC1 — DontAsk:** a tool call with no matching allow rule is denied without invoking `canUseTool`.
- **AC2 — path rules:** a `Read(.env)` deny rule blocks reads of `.env` at any depth; an `Edit` allow rule scopes writes to the configured glob. **(Scoping is *advisory*, not a security boundary — see "Path matching semantics".)**
- **AC3 — breaker:** a write to `.git/` is refused even under `AcceptEdits`.

## Key design decisions

Settled during brainstorming, then refined by staff review (both 2026-06-18):

| # | Decision | Choice |
|---|----------|--------|
| 1 | Allow-rule scope | **General + path.** `AllowRule::tool("X")`, `AllowRule::bash_command("git")`, and filesystem `read`/`edit` globs — so `DontAsk` is a usable general lockdown, not filesystem-only and not all-or-nothing for Bash. |
| 2 | Allow-rule reach | **Short-circuit in all modes**, symmetric with deny rules — a matching allow rule resolves to `Allow` after deny+guard, in any mode, **pre-empting the policy** (faithful to Claude Code's "allow = pre-approved = never ask"). This is a *global per-tool/per-command policy override*, documented loudly; granularity (`bash_command`) keeps it from being all-or-nothing. |
| 3 | Mode stickiness | **Tighten-only.** Loosening is forbidden; `Bypass → DontAsk` (tightening) is permitted; `DontAsk` is terminal. Enables locking down a child from a permissive parent. |
| 4 | Glob engine | **`globset`** (not `ignore`) + a small in-crate normalization layer for the three gitignore behaviors we need (bare-name-at-any-depth, `**`, leading anchor). Keeps the foundational crate light and the choice reversible. |
| 5 | Breaker action | **Ask** (consistent with the existing absolute-path write guard). Headless → Deny; with handler → prompt; disabled by `without_default_guards()`. |
| 6 | Path-match robustness | **Case-insensitive**, with `..`/`.` lexically collapsed before matching. A `.env` deny must not be defeated by `.ENV` (same file on macOS/Windows). |

### Staff-review dispositions (2026-06-18)

| Finding | Disposition |
|---------|-------------|
| #1 `ignore` on core | **Accepted** → switch to `globset` + thin layer (decision #4). |
| #2 case / `..` bypasses | **Accepted** → case-insensitive + `..` collapse + AC2 reframed advisory (decision #6). |
| #3 breaker "segment" under-defined | **Accepted** → exact path-*component* match + boundary tests. |
| #4 allow short-circuit footgun | **Accepted (mitigated)** → keep all-modes reach, add `bash_command` granularity, document the global-override semantics (decisions #1, #2). |
| #5 stickiness blocks tightening | **Accepted** → tighten-only lattice for the terminal pair (decision #3). |
| #6 SMA-414 blocking link | **Rejected — premise false.** SMA-414 is **Done** (`completedAt 2026-06-18`, PR #101), merged to `main`, and a direct ancestor of this branch (`1fe7743`). No moving ground; a `blockedBy` would point at a closed issue. Provenance pinned above instead. |
| #7 no `AllowRule::bash_command` | **Accepted** → added to v1 (pairs with #4). |
| #8 `PartialEq` surprise | **Accepted** → normalize the stored pattern at construction so `read(".env") == read("./.env")`. |
| #9 facade re-export timing | **Accepted** → made explicit in the release section. |

## Existing system (for reference)

The decision pipeline lives in `control.rs::authorize`:

```
deny rules › guard rules › mode › policy (canUseTool) › AskUser
```

- `PermissionMode` — `#[non_exhaustive]` enum: `Default`, `AcceptEdits`, `Plan`, `Bypass`. `Bypass` is sticky and propagates to sub-agents.
- `DenyRule` — deny-only; matches by exact tool name (`DenyRule::tool`) or Bash program (`DenyRule::bash_command`). There is **no allow-rule concept** today.
- `GuardRule` / `GuardRule::destructive_defaults()` — always-on, runs **before** mode (beats `Bypass`), may `Ask`. Already protects *absolute* system-path **writes** (`/etc`, `/usr`, …) with `Ask` semantics.
- `RunContext` carries `permission_mode`, `permission_policy`, `deny_rules`, `approval_handler`, `guard_rules`, `default_guards`. The tools (`Read`/`Write`/`Edit`, names `"Read"`/`"Write"`/`"Edit"`) all use the `path` argument.

**Constraint:** core has **no notion of a working root/cwd** — the cap-std root lives entirely in `paigasus-helikon-tools` (`sandbox.rs`). So path rules in core are **lexical glob matches on the `path` argument**, not filesystem-aware. This is why allow-path *scoping* is advisory (see below).

## New public API surface (`permission.rs`)

```rust
pub enum PermissionMode {            // existing #[non_exhaustive] enum
    Default, AcceptEdits, Plan, Bypass,
    DontAsk,                         // NEW — deny-by-default; policy never invoked
}

pub struct AllowRule { /* … */ }     // NEW — positive counterpart of DenyRule
impl AllowRule {
    pub fn tool(name: impl Into<String>) -> Self;            // mirrors DenyRule::tool
    pub fn bash_command(program: impl Into<String>) -> Self; // mirrors DenyRule::bash_command
    pub fn read(pattern: impl Into<String>) -> Self;         // gitignore-style glob on the Read tool
    pub fn edit(pattern: impl Into<String>) -> Self;         // gitignore-style glob on Edit/Write
    pub fn matches(&self, tool: &str, args: &serde_json::Value) -> bool;
}

impl DenyRule {                      // EXISTING type, extended
    pub fn read(pattern: impl Into<String>) -> Self;    // NEW path-deny
    pub fn edit(pattern: impl Into<String>) -> Self;    // NEW path-deny
}
```

**Why this shape:** all *denies* stay in `DenyRule` (→ the existing `deny_rules` vec, **no new copy-site**); all *allows* go in a new `AllowRule` (→ one new `allow_rules` vec). `allow_rules` is the *only* new `RunContext` field.

**Allow-rule semantics (documented loudly on `AllowRule` and the concept page):** an allow rule is a **global, all-modes, per-tool/per-command pre-approval** — when it matches, the call is allowed in *every* mode and `canUseTool` is **not** consulted for it. `AllowRule::tool("Bash")` therefore disables your Bash policy checks everywhere, not just under `DontAsk`; prefer `AllowRule::bash_command("git")` (every sub-command of a compound command must be allowed — same composition rule as `BashTool`'s allow list) to keep it scoped. The guard/deny steps still fire, so destructive commands remain caught.

## Revised pipeline (`control.rs::authorize`)

```
1. deny rules        → Deny     (tool / bash_command / read-path / edit-path)  — beats all, incl. Bypass
2. guard rules       → Ask/Deny (destructive defaults + NEW .git/.ssh/.env breaker)  — beats mode & allow
3. allow rules       → Allow    (short-circuit, ANY mode; pre-empts policy)       — NEW
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
- **AC2:** `DenyRule::read(".env")` fires at step 1 (beats everything); `AllowRule::edit("src/**")` is the only way a write passes under `DontAsk` (step 3), scoping writes to the glob (advisorily — see below).
- **AC3:** the breaker is a guard (step 2), which runs before the `AcceptEdits` auto-allow (step 4) and before allow rules (step 3) — so a `.git/` write is refused under `AcceptEdits`, and a user `AllowRule::edit(".git/**")` cannot override it.

## Path matching semantics

- Backed by **`globset`** + a small in-crate normalization layer. Each path rule compiles to a `GlobSet` (built `case_insensitive(true)`). Precedence among rules is the **pipeline's** (deny > allow), *not* gitignore last-match-wins; we do not promise cross-rule `!`-negation.
- **Normalization (the three gitignore behaviors we replicate):**
  - A pattern **without** a `/` is *unanchored* → matches at any depth: `.env` → `{ ".env", "**/.env" }`; `*.pem` → `{ "*.pem", "**/*.pem" }`.
  - A pattern **with** a `/` (or a leading-slash anchor we strip) is *anchored* to the path root: `src/**` → `{ "src/**" }`; `/src/**` → `{ "src/**" }`.
  - The **candidate path** (the `path` arg) is lexically cleaned before matching: trim a leading `./`, then collapse `.`/`..` components without touching the filesystem. A leading `..` that escapes the root survives the collapse and so won't match an anchored pattern (correct — it's escaping).
- **Stored pattern is normalized at construction** (trim leading `./`), so `read(".env")` and `read("./.env")` are `PartialEq`-equal (review #8). Equality compares normalized-pattern-string + kind; the compiled `GlobSet` (behind `Arc`, cheap `Clone`) is ignored in `PartialEq`/`Eq`/`Debug`, preserving `DenyRule`'s derive-style usage.
- Tool/arg mapping: `read` matches tool `"Read"`; `edit` matches `"Edit"` **and** `"Write"`; both read the `path` arg — consistent with the existing `ProtectedPathWrite` guard.

**Advisory, not a boundary (review #2).** Because core has no root, an allow-path rule is a *convenience filter*, not a containment guarantee: even with `..` collapsed and case-insensitive matching, the **real** boundary is the cap-std root in `paigasus-helikon-tools`. The concept page and `AllowRule::edit` docs must say so explicitly — do not present `AllowRule::edit("src/**")` as a sandbox.

## Protected-path breaker

New `GuardMatcher::ProtectedDotPathWrite` added to `GuardRule::destructive_defaults()`, **Ask** action:

- Matches the `Write`/`Edit` `path` arg, and Bash write-redirects (`>`, `>>`, `tee`, `dd of=`), after lexical `.`/`..` normalization.
- **Segment = exact path component** (review #3): split the normalized path on `/`; trip if **any component equals `.git` or `.ssh`**, or the **final component equals `.env` or starts with `.env.`**. This is component equality, **not** substring — so:
  - `name.git/config` → component `name.git` ≠ `.git` → **no trip** (bare repos are common; substring would false-positive).
  - `.gitignore` → final component ≠ `.env`/`.env.*` and ≠ `.git` → **no trip**.
  - `environment.env` → final component does not start with `.env.` (starts with `e`) → **no trip**.
  - `.env.local` → final component starts with `.env.` → **trip**. `.git/config`, `.ssh/id_rsa`, `src/.env` → **trip**.
- Headless (no handler) → Deny (so AC3 holds); with handler → prompt a human; disabled by `without_default_guards()`.
- **Writes only.** Reading `.env` is the user-configured `DenyRule::read(".env")` example, not a built-in. The protected-component set is a fixed constant; configurable sets are out of scope.

## Mode stickiness (tighten-only)

`RunContext::with_permission_mode` becomes a tighten-only transition (review #5):

```rust
pub fn with_permission_mode(mut self, mode: PermissionMode) -> Self {
    use PermissionMode::*;
    let allowed = match (self.permission_mode, mode) {
        (DontAsk, _)      => false,  // DontAsk is terminal — strictest, can't change
        (Bypass, DontAsk) => true,   // tighten Bypass → DontAsk is OK
        (Bypass, _)       => false,  // never loosen Bypass (existing invariant)
        _                 => true,    // normal modes freely settable
    };
    if allowed {
        self.permission_mode = mode;
    }
    self
}
```

Captures "tighten-only for the terminal pair" without a full five-mode lattice (Plan/AcceptEdits/Default ordering stays as-is — out of scope). Enables the motivating use case: a permissive parent locking an untrusted subagent down to `DontAsk`.

## RunContext state + propagation

New field `allow_rules: Vec<AllowRule>` with consuming builder `with_allow_rules` and reader `allow_rules()`. It **must** be wired through all **four** copy sites (the SMA-414 lesson — missing one is fail-open):

1. `handoff_child`
2. `subagent_child`
3. `clone_permission_fields` → `PermissionFields` → `to_tool_context`
4. `agent_as_tool::invoke`'s `sub_ctx` rebuild (already crosses mode/deny/guard/policy/handler at `agent_as_tool.rs:129–144`; `allow_rules` is the one field to add)

`DontAsk` (a `PermissionMode` value) propagates through `permission_mode`, but the sub_ctx rebuild reconstructs mode explicitly, so the propagation test must assert it crosses.

## Testing

- **Unit (`permission.rs`):** `AllowRule` tool/bash_command/read/edit matching; `DenyRule::read`/`edit` path variants; case-insensitive match (`.ENV` vs `.env`); `..` collapse (`src/../.git/config` does **not** match `src/**`); unanchored depth (`.env` at any depth) vs anchored scope (`src/**`); breaker boundary set — `name.git/config`, `environment.env`, `.gitignore` (no trip) and `.git/config`, `.ssh/id_rsa`, `.env.local`, `src/.env` (trip); `PartialEq` of `read(".env")` vs `read("./.env")`.
- **Unit (`control.rs`):** `DontAsk` denies without invoking the policy (use a policy that **panics if called**); allow rule short-circuits in `Default`/`Plan`/`AcceptEdits`; `bash_command` allow requires every sub-command allowed; deny-path beats `Bypass`; breaker beats `AcceptEdits`; an allow rule does **not** override the breaker; tighten-only — `Bypass → DontAsk` takes, `DontAsk → Bypass`/`Default` does not, `Bypass → Default` does not.
- **Integration:** extend `tests/subagent_propagation.rs` Test D to assert `allow_rules` **and** `DontAsk` cross into the agent-as-tool sub-run (the fail-open regression guard).

## Dependencies, release, docs

- **Dependency:** add `globset` to `[workspace.dependencies]` (root) and core `Cargo.toml` (`dep.workspace = true`). License is MIT/Unlicense — `deny.toml` allowlist OK. Lighter than `ignore` (no `crossbeam-deque`/`regex-automata`/`same-file`), and easy to reverse. Verify `cargo deny`, `cargo audit`, and the SBOM workflow stay green.
- **Release:** purely additive. `PermissionMode` is `#[non_exhaustive]`, so the new variant is non-breaking; new types are additive. Normal release-plz patch/minor bump — **no** stub-ascend ritual and **no** manual core bump (already-released crate gaining additive API).
- **Facade re-export (same release — review #9):** add `pub use … AllowRule;` to the facade in **this** PR (with a `///` doc, or `-D warnings` fails the docs job). The new `core` types are unreachable through `paigasus-helikon` until they're re-exported, and waiting reproduces the SMA-346 facade-drift gap. release-plz's `dependencies_update` cascade bumps the facade automatically when `core` bumps via the normal flow; the code re-export must ship in this PR regardless.
- **Docs (same PR):**
  - `docs/book/src/concepts/permissions-guardrails-hooks.md` — `DontAsk`, `AllowRule` (incl. the **global-override** caveat), path rules, the `.git`/`.ssh`/`.env` breaker, tighten-only stickiness, and the **advisory-not-a-boundary** limitation.
  - `crates/paigasus-helikon-core/README.md` — if its permission example/surface changes.

## Out of scope (YAGNI)

- Symlink allow-rule semantics — the ticket explicitly says "if later exposed."
- A configurable/extensible breaker component set — fixed constant for v1.
- Teaching core about a working root/cwd — path rules stay lexical (and therefore advisory) on the `path` arg.
- A full five-mode restrictiveness lattice — only the `Bypass`/`DontAsk` terminal pair is ordered; Plan/AcceptEdits/Default transitions are unchanged.
