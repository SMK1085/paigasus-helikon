# SMA-415 PermissionPolicy: DontAsk + filesystem path rules — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `PermissionMode::DontAsk` lockdown, a positive `AllowRule` (tool / bash-program / path-glob), `DenyRule` path-glob variants, and a `.git`/`.ssh`/`.env` write breaker — all wired through the existing `deny › guard › allow › mode › policy › AskUser` pipeline.

**Architecture:** A new `path_match` module backs gitignore-style globbing on the `path` argument with `globset` (core has no filesystem root, so matching is lexical and *advisory* — the cap-std root in `paigasus-helikon-tools` is the real boundary). Allows live in a new `AllowRule` (one new `RunContext` field, propagated through all four context copy sites); denies extend the existing `DenyRule` (no new field). `DontAsk` and tighten-only stickiness are pure additions to `PermissionMode` and `with_permission_mode`.

**Tech Stack:** Rust, `globset 0.4`, `async-trait`, `serde_json`. Crate: `paigasus-helikon-core`.

**Design spec:** [`docs/superpowers/specs/2026-06-18-sma-415-permissionpolicy-dontask-path-rules-design.md`](../specs/2026-06-18-sma-415-permissionpolicy-dontask-path-rules-design.md) (read it first; this plan implements every decision in its table).

---

## File structure

| File | Responsibility | Action |
|------|----------------|--------|
| `Cargo.toml` (root) | `globset` workspace pin | Modify |
| `crates/paigasus-helikon-core/Cargo.toml` | depend on `globset` | Modify |
| `crates/paigasus-helikon-core/src/path_match.rs` | lexical path cleaning, `PathGlob` glob matcher, `.git/.ssh/.env` component test | **Create** |
| `crates/paigasus-helikon-core/src/lib.rs` | register `mod path_match;` | Modify |
| `crates/paigasus-helikon-core/src/permission.rs` | `DontAsk` variant, `AllowRule`, `DenyRule::read/edit`, `ProtectedDotPathWrite` breaker | Modify |
| `crates/paigasus-helikon-core/src/context.rs` | `allow_rules` field + builder/accessor + 2 copy sites + `clone_permission_fields`; tighten-only `with_permission_mode` | Modify |
| `crates/paigasus-helikon-core/src/tool.rs` | `PermissionFields.allow_rules`, `ToolContext.allow_rules` carrier, `with_permissions` | Modify |
| `crates/paigasus-helikon-core/src/control.rs` | allow short-circuit (step 3) + `DontAsk` mode branch | Modify |
| `crates/paigasus-helikon-core/src/agent_as_tool.rs` | 4th copy site: `.with_allow_rules(...)` | Modify |
| `crates/paigasus-helikon-core/tests/subagent_propagation.rs` | assert `allow_rules` + `DontAsk` cross | Modify |
| `docs/book/src/concepts/permissions-guardrails-hooks.md` | document the new surface | Modify |
| `crates/paigasus-helikon-core/README.md` | refresh permission surface if shown | Modify (conditional) |

**No facade edit:** the facade re-exports core wholesale (`pub use paigasus_helikon_core as core;`) and core does `pub use permission::*;`, so `paigasus_helikon::core::AllowRule` resolves automatically once `AllowRule` is `pub`. (This refines spec review-item #9 — there is no per-type re-export to add; release-plz's cascade handles the version bump.)

---

## Task 1: Add the `globset` dependency

**Files:**
- Modify: `Cargo.toml` (root `[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon-core/Cargo.toml` (`[dependencies]`)

- [ ] **Step 1: Add the workspace pin**

In root `Cargo.toml`, under `[workspace.dependencies]`, add (keep the column alignment of the surrounding entries):

```toml
globset               = "0.4"
```

- [ ] **Step 2: Reference it from core**

In `crates/paigasus-helikon-core/Cargo.toml`, under `[dependencies]`, add after the `jiff` line:

```toml
globset        = { workspace = true }
```

- [ ] **Step 3: Verify it resolves and licenses pass**

Run: `cargo build -p paigasus-helikon-core`
Expected: builds clean (globset + already-present `aho-corasick`/`regex-automata`/`memchr` resolve).

Run: `cargo deny check licenses 2>&1 | tail -5`
Expected: no error. (globset is `Unlicense OR MIT`; the MIT branch is in the `deny.toml` allowlist, and its transitive deps are already vendored.)

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/paigasus-helikon-core/Cargo.toml
git commit -m "build(core): SMA-415 add globset dependency for path-glob rules"
```

---

## Task 2: `path_match` module — lexical cleaning, `PathGlob`, dotpath test

**Files:**
- Create: `crates/paigasus-helikon-core/src/path_match.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs` (add `mod path_match;`)

- [ ] **Step 1: Register the module**

In `crates/paigasus-helikon-core/src/lib.rs`, add alongside the other `mod` declarations (it is internal — `pub(crate)` items only, so do **not** add a `pub use`):

```rust
mod path_match;
```

- [ ] **Step 2: Write the module with its failing tests**

Create `crates/paigasus-helikon-core/src/path_match.rs`:

```rust
//! Lexical path matching for permission path-rules (SMA-415).
//!
//! Core has no filesystem root (the cap-std root lives in
//! `paigasus-helikon-tools`), so all matching here is **lexical** on the tool's
//! `path` argument and therefore *advisory* — a convenience filter, not a
//! containment boundary. Patterns follow a small gitignore-style subset:
//! a pattern without a `/` matches at any depth; a pattern with a `/` is
//! anchored to the path root. Matching is case-insensitive.

use std::sync::Arc;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Lexically clean a candidate path: strip a leading `./`, drop `.` components,
/// and collapse `..` without touching the filesystem. A leading `..` that
/// escapes the root survives (so it will not match an anchored pattern).
pub(crate) fn clean_path(path: &str) -> String {
    let trimmed = path.strip_prefix("./").unwrap_or(path);
    let mut out: Vec<&str> = Vec::new();
    for comp in trimmed.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                if out.last().map_or(false, |c| *c != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

/// `true` if the cleaned path writes into a protected VCS/secret location:
/// any `.git` or `.ssh` path component, or a final component equal to `.env`
/// or beginning `.env.` (e.g. `.env.local`). Component equality — never a
/// substring — so `name.git/`, `.gitignore`, and `environment.env` do not trip.
pub(crate) fn is_protected_dotpath(path: &str) -> bool {
    let cleaned = clean_path(path);
    let comps: Vec<&str> = cleaned.split('/').filter(|c| !c.is_empty()).collect();
    if comps.iter().any(|c| *c == ".git" || *c == ".ssh") {
        return true;
    }
    matches!(comps.last(), Some(last) if *last == ".env" || last.starts_with(".env."))
}

/// A compiled, case-insensitive path-glob. Cheap to clone (the matcher is
/// behind `Arc`). Equality/Debug use the normalized source pattern only — the
/// compiled `GlobSet` is opaque — so `DenyRule`/`AllowRule` keep derive-style
/// `PartialEq`/`Eq`/`Debug`.
#[derive(Clone)]
pub(crate) struct PathGlob {
    pattern: String,
    set: Arc<GlobSet>,
}

impl PathGlob {
    /// Compile `pattern` (normalized by trimming a leading `./` or `/`).
    pub(crate) fn new(pattern: impl Into<String>) -> Self {
        let pattern = normalize_pattern(pattern.into());
        let set = Arc::new(build_globset(&pattern));
        Self { pattern, set }
    }

    /// `true` if `path` (lexically cleaned) matches this glob.
    pub(crate) fn matches_path(&self, path: &str) -> bool {
        self.set.is_match(clean_path(path))
    }
}

impl PartialEq for PathGlob {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern
    }
}
impl Eq for PathGlob {}
impl std::fmt::Debug for PathGlob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathGlob").field("pattern", &self.pattern).finish()
    }
}

/// Trim a leading `./` then a single leading `/` (gitignore anchor — we anchor
/// instead by the presence of an interior `/`).
fn normalize_pattern(pat: String) -> String {
    let p = pat.strip_prefix("./").unwrap_or(&pat);
    let p = p.strip_prefix('/').unwrap_or(p);
    p.to_owned()
}

/// Build a case-insensitive `GlobSet`. A pattern with no `/` is unanchored
/// (matches at any depth) → `{pat, **/pat}`; a pattern with a `/` is anchored.
fn build_globset(pattern: &str) -> GlobSet {
    let globs: Vec<String> = if pattern.contains('/') {
        vec![pattern.to_owned()]
    } else {
        vec![pattern.to_owned(), format!("**/{pattern}")]
    };
    let mut builder = GlobSetBuilder::new();
    for g in globs {
        if let Ok(glob) = GlobBuilder::new(&g)
            .case_insensitive(true)
            .literal_separator(true)
            .build()
        {
            builder.add(glob);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_path_strips_dot_and_collapses_dotdot() {
        assert_eq!(clean_path("./a/b"), "a/b");
        assert_eq!(clean_path("a/../b"), "b");
        assert_eq!(clean_path("src/../.git/config"), ".git/config");
        assert_eq!(clean_path("../escape"), "../escape"); // leading .. survives
    }

    #[test]
    fn unanchored_pattern_matches_any_depth() {
        let g = PathGlob::new(".env");
        assert!(g.matches_path(".env"));
        assert!(g.matches_path("a/b/.env"));
        assert!(g.matches_path("./.env"));
        assert!(!g.matches_path(".envrc"));
    }

    #[test]
    fn extension_pattern_matches_any_depth() {
        let g = PathGlob::new("*.pem");
        assert!(g.matches_path("key.pem"));
        assert!(g.matches_path("secrets/key.pem"));
        assert!(!g.matches_path("key.pub"));
    }

    #[test]
    fn anchored_pattern_scopes_to_root() {
        let g = PathGlob::new("src/**");
        assert!(g.matches_path("src/main.rs"));
        assert!(g.matches_path("src/a/b.rs"));
        assert!(!g.matches_path("tests/main.rs"));
        // `..` cannot escape the anchored prefix once collapsed.
        assert!(!g.matches_path("src/../.git/config"));
    }

    #[test]
    fn leading_slash_anchor_is_stripped() {
        let g = PathGlob::new("/src/**");
        assert!(g.matches_path("src/main.rs"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let g = PathGlob::new(".env");
        assert!(g.matches_path(".ENV"));
        assert!(g.matches_path(".Env"));
    }

    #[test]
    fn path_glob_eq_is_normalized() {
        assert_eq!(PathGlob::new(".env"), PathGlob::new("./.env"));
    }

    #[test]
    fn protected_dotpath_trips_on_component_only() {
        // trips
        assert!(is_protected_dotpath(".git/config"));
        assert!(is_protected_dotpath("a/.ssh/id_rsa"));
        assert!(is_protected_dotpath(".env"));
        assert!(is_protected_dotpath("src/.env.local"));
        // does NOT trip
        assert!(!is_protected_dotpath("name.git/config")); // bare repo
        assert!(!is_protected_dotpath(".gitignore"));
        assert!(!is_protected_dotpath("environment.env"));
        assert!(!is_protected_dotpath("src/main.rs"));
    }
}
```

- [ ] **Step 3: Run the tests — expect them to pass once implemented**

Run: `cargo test -p paigasus-helikon-core path_match`
Expected: PASS (8 tests). If `**/.env` fails to match top-level `.env` on your globset build, the `{pat, **/pat}` pair in `build_globset` covers it — both globs are added.

- [ ] **Step 4: Format + clippy**

Run: `cargo fmt -p paigasus-helikon-core && cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/path_match.rs crates/paigasus-helikon-core/src/lib.rs
git commit -m "feat(core): SMA-415 add path_match module (PathGlob + dotpath test)"
```

---

## Task 3: Protected-dotpath write breaker (`permission.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/permission.rs` (`GuardMatcher`, `GuardRule::matches`, `destructive_defaults`, new helper, tests)

- [ ] **Step 1: Write the failing test**

In `permission.rs`, inside `mod guard_tests` (after `destructive_defaults_use_ask_action`), add:

```rust
#[test]
fn matches_protected_dotpath_write() {
    // Write/Edit tool path arg
    let g = GuardRule::destructive_defaults();
    assert!(g.iter().any(|r| r.matches("Write", &json!({ "path": ".git/config", "content": "x" }))));
    assert!(g.iter().any(|r| r.matches("Edit", &json!({ "path": "a/.ssh/known_hosts" }))));
    assert!(g.iter().any(|r| r.matches("Write", &json!({ "path": ".env.local", "content": "x" }))));
    // bare repo / lookalikes do NOT trip
    assert!(!g.iter().any(|r| r.matches("Write", &json!({ "path": "repo.git/HEAD", "content": "x" }))));
    assert!(!g.iter().any(|r| r.matches("Write", &json!({ "path": ".gitignore", "content": "x" }))));
    // bash redirect into .git
    assert!(matched("echo x > .git/config"));
    assert!(matched("echo x | tee .ssh/authorized_keys"));
    assert!(!matched("echo x > notes.txt"));
}
```

- [ ] **Step 2: Run it — expect failure**

Run: `cargo test -p paigasus-helikon-core matches_protected_dotpath_write`
Expected: FAIL (variant/behavior not present).

- [ ] **Step 3: Add the `GuardMatcher` variant**

In `permission.rs`, extend the `GuardMatcher` enum (currently `RmRecursiveRootOrHome`, `ProtectedPathWrite`):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
enum GuardMatcher {
    /// `rm` with recursive+force flags targeting `/` or `~` (literal).
    RmRecursiveRootOrHome,
    /// A write whose target resolves under a protected prefix (Bash redirects,
    /// `tee`/`dd`, or the Write/Edit `path` arg). Honors the device-node allowlist.
    ProtectedPathWrite,
    /// A write whose target has a `.git`/`.ssh` path component or a `.env`(`.env.*`)
    /// final component (Bash redirects, `tee`/`dd`, or the Write/Edit `path` arg).
    ProtectedDotPathWrite,
}
```

- [ ] **Step 4: Handle it in `GuardRule::matches`**

In the `match &self.matcher` block of `GuardRule::matches`, add the arm after `ProtectedPathWrite`:

```rust
            GuardMatcher::ProtectedDotPathWrite => protected_dotpath_write(tool, args),
```

- [ ] **Step 5: Add the breaker to `destructive_defaults`**

In `GuardRule::destructive_defaults()`, append a third entry to the returned `vec!`:

```rust
            GuardRule {
                matcher: GuardMatcher::ProtectedDotPathWrite,
                action: GuardAction::Ask {
                    prompt: "write to a protected VCS/secret path (.git, .ssh, .env)".to_owned(),
                },
            },
```

- [ ] **Step 6: Add the helper**

In `permission.rs`, next to `protected_path_write`, add:

```rust
fn protected_dotpath_write(tool: &str, args: &serde_json::Value) -> bool {
    if matches!(tool, "Write" | "Edit") {
        if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
            if crate::path_match::is_protected_dotpath(p) {
                return true;
            }
        }
    }
    if let Some(cmd) = bash_command_str(tool, args) {
        for c in crate::command_match::resolve_all(cmd) {
            for r in &c.redirects {
                use crate::command_match::RedirectOp;
                if matches!(r.op, RedirectOp::Out | RedirectOp::Append)
                    && crate::path_match::is_protected_dotpath(&r.target)
                {
                    return true;
                }
            }
            if c.program == "tee" && c.args.iter().any(|a| crate::path_match::is_protected_dotpath(a)) {
                return true;
            }
            if c.program == "dd" {
                if let Some(of) = c.args.iter().find_map(|a| a.strip_prefix("of=")) {
                    if crate::path_match::is_protected_dotpath(of) {
                        return true;
                    }
                }
            }
        }
    }
    false
}
```

- [ ] **Step 7: Run the test — expect pass**

Run: `cargo test -p paigasus-helikon-core matches_protected_dotpath_write`
Expected: PASS. Also run `cargo test -p paigasus-helikon-core guard_tests` — the existing `destructive_defaults_use_ask_action` still passes (the new rule is `Ask`).

- [ ] **Step 8: Format, clippy, commit**

```bash
cargo fmt -p paigasus-helikon-core && cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/permission.rs
git commit -m "feat(core): SMA-415 add .git/.ssh/.env write breaker (Ask)"
```

---

## Task 4: `AllowRule` + `DenyRule` path variants (`permission.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/permission.rs` (extend `Matcher`, `DenyRule`; add `AllowRule`; shared path helper; tests)

- [ ] **Step 1: Write the failing tests**

In `permission.rs` `mod tests`, add:

```rust
#[test]
fn deny_rule_read_edit_path_variants() {
    let dr = DenyRule::read(".env");
    assert!(dr.matches("Read", &json!({ "path": "config/.env" })));
    assert!(!dr.matches("Read", &json!({ "path": "config/app.toml" })));
    assert!(!dr.matches("Edit", &json!({ "path": ".env" }))); // read-scoped

    let de = DenyRule::edit("src/**");
    assert!(de.matches("Edit", &json!({ "path": "src/a.rs" })));
    assert!(de.matches("Write", &json!({ "path": "src/a.rs" }))); // edit covers Write
    assert!(!de.matches("Read", &json!({ "path": "src/a.rs" })));
}

#[test]
fn allow_rule_tool_and_path() {
    assert!(AllowRule::tool("WebSearch").matches("WebSearch", &json!({})));
    assert!(!AllowRule::tool("WebSearch").matches("Bash", &json!({})));

    assert!(AllowRule::read("src/**").matches("Read", &json!({ "path": "src/a.rs" })));
    assert!(AllowRule::edit("src/**").matches("Write", &json!({ "path": "src/a.rs" })));
    assert!(!AllowRule::edit("src/**").matches("Write", &json!({ "path": "etc/x" })));
}

#[test]
fn allow_rule_bash_command_requires_every_subcommand() {
    let rule = AllowRule::bash_command("git");
    assert!(rule.matches("Bash", &json!({ "command": "git status && git push" })));
    // a non-git sub-command means the allow rule does NOT fire (fail-closed)
    assert!(!rule.matches("Bash", &json!({ "command": "git status && rm -rf ." })));
    assert!(!rule.matches("Bash", &json!({ "command": "" })));
    assert!(!rule.matches("Other", &json!({ "command": "git status" })));
}
```

- [ ] **Step 2: Run — expect failure**

Run: `cargo test -p paigasus-helikon-core allow_rule`
Expected: FAIL (`AllowRule` / `DenyRule::read` not defined).

- [ ] **Step 3: Extend the `Matcher` enum (for `DenyRule`)**

Replace the `Matcher` enum in `permission.rs` with:

```rust
/// How a [`DenyRule`] matches a call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Matcher {
    /// Exact tool name.
    Tool(String),
    /// Any Bash sub-command whose resolved program equals this. Tool-scoped to
    /// the `Bash` tool.
    BashProgram(String),
    /// `Read` tool whose `path` arg matches this glob.
    ReadPath(crate::path_match::PathGlob),
    /// `Edit`/`Write` tool whose `path` arg matches this glob.
    EditPath(crate::path_match::PathGlob),
}
```

- [ ] **Step 4: Add `DenyRule::read`/`edit` and the path arms**

Add the constructors in `impl DenyRule` (after `bash_command`):

```rust
    /// Deny a `Read` whose `path` arg matches `pattern` (gitignore-style; see
    /// [`crate::path_match`]). Lexical and **advisory** — not a sandbox.
    pub fn read(pattern: impl Into<String>) -> Self {
        Self {
            matcher: Matcher::ReadPath(crate::path_match::PathGlob::new(pattern)),
        }
    }

    /// Deny an `Edit`/`Write` whose `path` arg matches `pattern`.
    pub fn edit(pattern: impl Into<String>) -> Self {
        Self {
            matcher: Matcher::EditPath(crate::path_match::PathGlob::new(pattern)),
        }
    }
```

In `DenyRule::matches`, add the two arms to the `match &self.matcher` block:

```rust
            Matcher::ReadPath(glob) => path_arg_matches(tool, args, PathKind::Read, glob),
            Matcher::EditPath(glob) => path_arg_matches(tool, args, PathKind::Edit, glob),
```

- [ ] **Step 5: Add the shared path helper**

Add near the bottom of `permission.rs` (free items, not in a `mod`):

```rust
/// Which tool family a path-rule applies to.
#[derive(Clone, Copy)]
enum PathKind {
    Read,
    Edit,
}

/// `true` if `tool` is in `kind`'s family and its `path` arg matches `glob`.
/// `Read` → tool `"Read"`; `Edit` → tools `"Edit"` and `"Write"`.
fn path_arg_matches(
    tool: &str,
    args: &serde_json::Value,
    kind: PathKind,
    glob: &crate::path_match::PathGlob,
) -> bool {
    let applies = match kind {
        PathKind::Read => tool == "Read",
        PathKind::Edit => tool == "Edit" || tool == "Write",
    };
    if !applies {
        return false;
    }
    args.get("path")
        .and_then(|v| v.as_str())
        .map(|p| glob.matches_path(p))
        .unwrap_or(false)
}
```

- [ ] **Step 6: Add the `AllowRule` type**

Add after the `DenyRule` block in `permission.rs`:

```rust
/// How an [`AllowRule`] matches a call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AllowMatcher {
    Tool(String),
    BashProgram(String),
    ReadPath(crate::path_match::PathGlob),
    EditPath(crate::path_match::PathGlob),
}

/// A positive permission rule: a **global, all-modes, per-tool/per-command
/// pre-approval**. When an allow rule matches, the call is allowed in *every*
/// mode and `canUseTool` is **not** consulted for it (the deny and guard steps
/// still run first). Prefer [`AllowRule::bash_command`] over
/// `AllowRule::tool("Bash")` so a single allowed program does not disable all
/// Bash policy checks. Evaluated after deny + guard, before mode (see
/// `control.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowRule {
    matcher: AllowMatcher,
}

impl AllowRule {
    /// Allow a tool by its exact name.
    pub fn tool(name: impl Into<String>) -> Self {
        Self {
            matcher: AllowMatcher::Tool(name.into()),
        }
    }

    /// Allow a Bash call **only when every** resolved sub-command's program
    /// equals `program` (operator-, wrapper-, `bash -c`-aware). A compound
    /// command with any other program does not match (fail-closed). Only the
    /// `Bash` tool. v1 does not compose multiple `bash_command` allows across a
    /// single compound command.
    pub fn bash_command(program: impl Into<String>) -> Self {
        Self {
            matcher: AllowMatcher::BashProgram(program.into()),
        }
    }

    /// Allow a `Read` whose `path` arg matches `pattern` (gitignore-style;
    /// advisory, not a sandbox — see [`crate::path_match`]).
    pub fn read(pattern: impl Into<String>) -> Self {
        Self {
            matcher: AllowMatcher::ReadPath(crate::path_match::PathGlob::new(pattern)),
        }
    }

    /// Allow an `Edit`/`Write` whose `path` arg matches `pattern`.
    pub fn edit(pattern: impl Into<String>) -> Self {
        Self {
            matcher: AllowMatcher::EditPath(crate::path_match::PathGlob::new(pattern)),
        }
    }

    /// `true` if this rule allows `tool` invoked with `args`.
    pub fn matches(&self, tool: &str, args: &serde_json::Value) -> bool {
        match &self.matcher {
            AllowMatcher::Tool(name) => name == tool,
            AllowMatcher::BashProgram(program) => {
                if tool != "Bash" {
                    return false;
                }
                let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
                    return false;
                };
                let subs = crate::command_match::resolve_all(command);
                !subs.is_empty() && subs.iter().all(|c| &c.program == program)
            }
            AllowMatcher::ReadPath(glob) => path_arg_matches(tool, args, PathKind::Read, glob),
            AllowMatcher::EditPath(glob) => path_arg_matches(tool, args, PathKind::Edit, glob),
        }
    }
}
```

- [ ] **Step 7: Run the tests — expect pass**

Run: `cargo test -p paigasus-helikon-core 'allow_rule' && cargo test -p paigasus-helikon-core deny_rule_read_edit_path_variants`
Expected: PASS. Run the whole `permission` test set too: `cargo test -p paigasus-helikon-core --lib permission` — existing deny/guard tests still pass.

- [ ] **Step 8: Format, clippy, commit**

```bash
cargo fmt -p paigasus-helikon-core && cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/permission.rs
git commit -m "feat(core): SMA-415 add AllowRule + DenyRule path-glob variants"
```

---

## Task 5: `RunContext.allow_rules` + 3 of 4 copy sites (`context.rs`, `tool.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs:85` (field), `:121-128` (new), `:216-221` (handoff_child), `:244-249` (subagent_child), `:278-308` (builder/accessor), `:357-367` (clone_permission_fields)
- Modify: `crates/paigasus-helikon-core/src/tool.rs:99-110` (`PermissionFields`), `:135-149` (`ToolContext` field), `:165-180` (`ToolContext::new`), `:245-253` (`with_permissions`)

- [ ] **Step 1: Write the failing propagation unit test**

In `context.rs` `#[cfg(test)] mod tests`, near `guard_rules_default_on_and_inherit_through_children`, add:

```rust
#[test]
fn allow_rules_inherit_through_children() {
    use crate::AllowRule;
    let ctx = RunContext::new(
        Arc::new(()),
        Arc::new(MemorySession::new()) as Arc<dyn Session>,
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
    .with_allow_rules(vec![AllowRule::tool("WebSearch")]);
    assert_eq!(ctx.allow_rules().len(), 1);
    assert_eq!(ctx.handoff_child().allow_rules().len(), 1);
    assert_eq!(ctx.subagent_child().allow_rules().len(), 1);
    assert_eq!(ctx.to_tool_context().clone_permission_fields_len(), 1);
}
```

> Note: `clone_permission_fields_len` is a tiny test-only shim — add it in Step 6 so the test can observe the projected field without exposing `PermissionFields`. If `to_tool_context()` is not in scope for tests, replace that final assert with the `clone_permission_fields()` check shown in Step 6.

- [ ] **Step 2: Run — expect failure**

Run: `cargo test -p paigasus-helikon-core allow_rules_inherit_through_children`
Expected: FAIL (`with_allow_rules` not defined).

- [ ] **Step 3: Add the field + default + import**

In `context.rs`, ensure `AllowRule` is imported — find the `use crate::{... DenyRule ...}` and add `AllowRule`.

Add the field after `deny_rules` (`:85`):

```rust
    /// Allow rules evaluated after deny+guard, before mode (positive
    /// short-circuit in any mode; the only path to Allow under `DontAsk`).
    allow_rules: Vec<AllowRule>,
```

In `RunContext::new` (`:123`), after `deny_rules: Vec::new(),`:

```rust
            allow_rules: Vec::new(),
```

- [ ] **Step 4: Wire the two `*_child` copy sites**

In `handoff_child` (after `deny_rules: self.deny_rules.clone(),` at `:218`) and again in `subagent_child` (`:246`), add:

```rust
            allow_rules: self.allow_rules.clone(),
```

- [ ] **Step 5: Add the builder + accessor**

After `with_deny_rules` (`:282`) add:

```rust
    /// Install allow rules (positive short-circuit; see [`crate::AllowRule`]).
    pub fn with_allow_rules(mut self, rules: Vec<AllowRule>) -> Self {
        self.allow_rules = rules;
        self
    }
```

After `deny_rules()` (`:303`) add:

```rust
    /// The run's allow rules.
    pub fn allow_rules(&self) -> &[AllowRule] {
        &self.allow_rules
    }
```

- [ ] **Step 6: Project through `clone_permission_fields` + the test shim**

In `clone_permission_fields` (`:358`) add `allow_rules: self.allow_rules.clone(),` to the struct literal. Then add a test-only shim method on `RunContext` (inside the same `impl`, gated):

```rust
    #[cfg(test)]
    pub(crate) fn clone_permission_fields_len(&self) -> usize {
        self.clone_permission_fields().allow_rules.len()
    }
```

> If `to_tool_context()` isn't reachable from the test module, change the test's last line to `assert_eq!(ctx.clone_permission_fields_len(), 1);`.

- [ ] **Step 7: Extend `PermissionFields` and `ToolContext` (`tool.rs`)**

In `tool.rs`, ensure `AllowRule` is imported (add to the `use crate::{... DenyRule ...}`).

In `struct PermissionFields` (`:105`), after `deny_rules`:

```rust
    pub(crate) allow_rules: Vec<AllowRule>,
```

In `struct ToolContext` (after the `deny_rules` carrier, `:139`):

```rust
    /// Carrier: allow rules from the parent [`crate::RunContext`].
    pub(crate) allow_rules: Vec<AllowRule>,
```

In `ToolContext::new` (after `deny_rules: Vec::new(),`):

```rust
            allow_rules: Vec::new(),
```

In `with_permissions` (after `self.deny_rules = fields.deny_rules;`):

```rust
        self.allow_rules = fields.allow_rules;
```

- [ ] **Step 8: Run the test — expect pass**

Run: `cargo test -p paigasus-helikon-core allow_rules_inherit_through_children`
Expected: PASS.

- [ ] **Step 9: Format, clippy, commit**

```bash
cargo fmt -p paigasus-helikon-core && cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/context.rs crates/paigasus-helikon-core/src/tool.rs
git commit -m "feat(core): SMA-415 thread allow_rules through RunContext + ToolContext"
```

---

## Task 6: Tighten-only `with_permission_mode` + `DontAsk` variant (`permission.rs`, `context.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/permission.rs` (`PermissionMode` enum)
- Modify: `crates/paigasus-helikon-core/src/context.rs:265-270` (`with_permission_mode`) + tests

- [ ] **Step 1: Add the `DontAsk` variant**

In `permission.rs`, extend the `PermissionMode` enum (after `Bypass`):

```rust
    /// Locked-down headless inverse of `Bypass`: deny-by-default. The policy
    /// (`canUseTool`) is never invoked; only an [`crate::AllowRule`] (after
    /// the deny+guard steps) can permit a call. Sticky and terminal — once set
    /// it cannot be loosened.
    DontAsk,
```

- [ ] **Step 2: Write the failing stickiness test**

In `context.rs` tests, add (next to the existing `with_permission_mode_is_monotonic_on_bypass`):

```rust
#[test]
fn permission_mode_is_tighten_only() {
    use crate::PermissionMode::*;
    let base = || {
        RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
    };
    // Bypass can tighten to DontAsk …
    assert_eq!(base().with_permission_mode(Bypass).with_permission_mode(DontAsk).permission_mode(), DontAsk);
    // … but cannot loosen to Default/Plan.
    assert_eq!(base().with_permission_mode(Bypass).with_permission_mode(Default).permission_mode(), Bypass);
    // DontAsk is terminal — no transition off it.
    assert_eq!(base().with_permission_mode(DontAsk).with_permission_mode(Bypass).permission_mode(), DontAsk);
    assert_eq!(base().with_permission_mode(DontAsk).with_permission_mode(Default).permission_mode(), DontAsk);
    // Normal modes still settable.
    assert_eq!(base().with_permission_mode(Plan).permission_mode(), Plan);
}
```

- [ ] **Step 3: Run — expect failure**

Run: `cargo test -p paigasus-helikon-core permission_mode_is_tighten_only`
Expected: FAIL (current guard blocks `Bypass → DontAsk`).

- [ ] **Step 4: Replace `with_permission_mode`**

In `context.rs`, replace the body (`:265-269`):

```rust
    /// Set the permission mode — **tighten-only**. Loosening is refused:
    /// `DontAsk` is terminal (no transition off it), and `Bypass` may only
    /// tighten to `DontAsk`, never loosen. All other transitions apply.
    pub fn with_permission_mode(mut self, mode: PermissionMode) -> Self {
        use PermissionMode::*;
        let allowed = match (self.permission_mode, mode) {
            (DontAsk, _) => false,
            (Bypass, DontAsk) => true,
            (Bypass, _) => false,
            _ => true,
        };
        if allowed {
            self.permission_mode = mode;
        }
        self
    }
```

- [ ] **Step 5: Run tests — expect pass**

Run: `cargo test -p paigasus-helikon-core permission_mode`
Expected: PASS (new test + the existing `with_permission_mode_is_monotonic_on_bypass`, which asserts `Bypass → Plan` stays `Bypass` — still true).

- [ ] **Step 6: Format, clippy, commit**

```bash
cargo fmt -p paigasus-helikon-core && cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/permission.rs crates/paigasus-helikon-core/src/context.rs
git commit -m "feat(core): SMA-415 add DontAsk mode + tighten-only stickiness"
```

---

## Task 7: Pipeline — allow short-circuit + `DontAsk` deny (`control.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/control.rs:111-192` (`authorize`) + `mod authorize_tests`

- [ ] **Step 1: Write the failing tests**

In `control.rs` `mod authorize_tests`, add a panicking policy and the cases:

```rust
struct PanicPolicy;
#[async_trait]
impl PermissionPolicy<()> for PanicPolicy {
    async fn check(&self, _: &RunContext<()>, _: &str, _: &serde_json::Value) -> PermissionDecision {
        panic!("policy must not be consulted under DontAsk");
    }
}

#[tokio::test]
async fn dont_ask_denies_without_invoking_policy() {
    use crate::AllowRule;
    let c = ctx()
        .with_permission_mode(PermissionMode::DontAsk)
        .with_permission_policy(Arc::new(PanicPolicy))
        .with_allow_rules(vec![AllowRule::tool("Read")]);
    let i = interceptors(&c);
    // allowed tool → Allow (policy never called)
    assert!(matches!(
        i.authorize("Read", ToolEffect::ReadOnly, &json!({"path": "a"})).await,
        PermissionDecision::Allow
    ));
    // unlisted tool → Deny (policy never called → no panic)
    assert!(matches!(
        i.authorize("Bash", ToolEffect::SideEffect, &json!({"command": "ls"})).await,
        PermissionDecision::Deny { .. }
    ));
}

#[tokio::test]
async fn allow_rule_short_circuits_in_default_mode() {
    use crate::AllowRule;
    let c = ctx()
        .with_permission_policy(Arc::new(AskPolicy)) // would otherwise Ask→Deny
        .with_allow_rules(vec![AllowRule::tool("WebSearch")]);
    let i = interceptors(&c);
    assert!(matches!(
        i.authorize("WebSearch", ToolEffect::SideEffect, &json!({})).await,
        PermissionDecision::Allow
    ));
}

#[tokio::test]
async fn deny_path_beats_bypass() {
    use crate::DenyRule;
    let c = ctx()
        .with_permission_mode(PermissionMode::Bypass)
        .with_deny_rules(vec![DenyRule::read(".env")]);
    let i = interceptors(&c);
    assert!(matches!(
        i.authorize("Read", ToolEffect::ReadOnly, &json!({"path": "cfg/.env"})).await,
        PermissionDecision::Deny { .. }
    ));
}

#[tokio::test]
async fn breaker_beats_accept_edits_and_allow_rule() {
    use crate::AllowRule;
    let c = ctx()
        .with_permission_mode(PermissionMode::AcceptEdits)
        .with_allow_rules(vec![AllowRule::edit(".git/**")]); // must NOT override breaker
    let i = interceptors(&c);
    // no approval handler installed → Ask resolves to Deny
    assert!(matches!(
        i.authorize("Write", ToolEffect::Write, &json!({"path": ".git/config", "content": "x"})).await,
        PermissionDecision::Deny { .. }
    ));
}
```

- [ ] **Step 2: Run — expect failure/compile error**

Run: `cargo test -p paigasus-helikon-core --lib authorize_tests`
Expected: FAIL (`DontAsk` arm missing; allow short-circuit absent → `breaker_beats…`/`allow_rule_short_circuits…` fail).

- [ ] **Step 3: Insert the allow short-circuit (step 3 of the pipeline)**

In `authorize`, immediately after the guard-rules `for` loop closes (before `// 2. Mode.`), insert:

```rust
        // 3. Allow rules — positive short-circuit in ANY mode (after deny+guard,
        // before mode). A global per-tool/per-command pre-approval that skips
        // the policy. Deny and guard already ran, so this cannot resurrect a
        // denied/guarded call.
        if self.ctx.allow_rules().iter().any(|r| r.matches(tool, args)) {
            return PermissionDecision::Allow;
        }
```

(Optionally renumber the existing `// 2. Mode.` / `// 3. Policy` comments to keep them monotonic; not required for correctness.)

- [ ] **Step 4: Add the `DontAsk` mode branch**

In the `match self.ctx.permission_mode()` block, add an explicit arm (the existing `_ => {}` would otherwise fall through to the policy — wrong for `DontAsk`):

```rust
            PermissionMode::DontAsk => {
                return PermissionDecision::Deny {
                    reason: format!("DontAsk mode: no allow rule matched `{tool}`"),
                };
            }
```

- [ ] **Step 5: Run — expect pass**

Run: `cargo test -p paigasus-helikon-core --lib authorize_tests`
Expected: PASS (all new + existing authorize tests; `PanicPolicy` never panics).

- [ ] **Step 6: Format, clippy, commit**

```bash
cargo fmt -p paigasus-helikon-core && cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/control.rs
git commit -m "feat(core): SMA-415 wire allow short-circuit + DontAsk into authorize"
```

---

## Task 8: 4th copy site + integration propagation test

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent_as_tool.rs:129-133` (sub_ctx chain)
- Modify: `crates/paigasus-helikon-core/tests/subagent_propagation.rs` (Test D)

- [ ] **Step 1: Extend the integration test (Test D) to assert allow_rules + DontAsk cross**

In `tests/subagent_propagation.rs`, in the observer struct and the `guard_and_redaction_config_propagates_into_agent_as_tool_sub_run` test, capture two more fields. Add to the observer's recorded struct:

```rust
        allow_rules_len: usize,
        permission_mode: paigasus_helikon_core::PermissionMode,
```

In the inner observing tool's `invoke`, record them from the sub-run `RunContext`:

```rust
        obs.allow_rules_len = ctx.allow_rules().len();
        obs.permission_mode = ctx.permission_mode();
```

In the parent-context setup, add an allow rule and tighten to `DontAsk` **last** (so it is the terminal mode). Because `DontAsk` denies everything without an allow rule, you must allow the inner observing tool's name so the sub-run can still invoke it — set the allow rule to that tool name:

```rust
    .with_allow_rules(vec![paigasus_helikon_core::AllowRule::tool(INNER_TOOL_NAME)])
    .with_permission_mode(paigasus_helikon_core::PermissionMode::DontAsk)
```

After the invoke, assert:

```rust
    assert_eq!(obs.allow_rules_len, 1, "allow_rules must propagate into the sub-run");
    assert_eq!(
        obs.permission_mode,
        paigasus_helikon_core::PermissionMode::DontAsk,
        "DontAsk must propagate into the sub-run"
    );
```

> Use the actual inner tool's registered name for `INNER_TOOL_NAME` (read it from the existing test's tool definition). If `Plan` was previously the propagated mode in this test, keep a *separate* existing assertion or move the `Plan` check to its own test — do not assert both `Plan` and `DontAsk` for the same run.

- [ ] **Step 2: Run — expect failure**

Run: `cargo test -p paigasus-helikon-core --test subagent_propagation`
Expected: FAIL (`allow_rules` does not cross — `agent_as_tool` doesn't copy it yet; `allow_rules_len` is 0).

- [ ] **Step 3: Add the 4th copy site**

In `agent_as_tool.rs`, in the `sub_ctx` builder chain (after `.with_deny_rules(ctx.deny_rules.clone())`), add:

```rust
        .with_allow_rules(ctx.allow_rules.clone())
```

- [ ] **Step 4: Run — expect pass**

Run: `cargo test -p paigasus-helikon-core --test subagent_propagation`
Expected: PASS (allow_rules length 1 and mode `DontAsk` both observed in the sub-run).

- [ ] **Step 5: Format, clippy, commit**

```bash
cargo fmt -p paigasus-helikon-core && cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/agent_as_tool.rs crates/paigasus-helikon-core/tests/subagent_propagation.rs
git commit -m "feat(core): SMA-415 propagate allow_rules into agent-as-tool sub-run"
```

---

## Task 9: Documentation (mdBook concept page + README)

**Files:**
- Modify: `docs/book/src/concepts/permissions-guardrails-hooks.md`
- Modify: `crates/paigasus-helikon-core/README.md` (only if it shows the permission surface)

- [ ] **Step 1: Update the `PermissionMode` bullet**

In `permissions-guardrails-hooks.md`, in the `PermissionMode` bullet (the list under "## Permissions"), add `DontAsk` and the tighten-only note:

```markdown
- `PermissionMode` — a `#[non_exhaustive]` enum: `Default`, `AcceptEdits`, `Plan`, `Bypass`, and `DontAsk` (deny-by-default headless lockdown — the policy is never invoked; only an allow rule can permit a call). Mode is **tighten-only**: `Bypass` may tighten to `DontAsk` but never loosen, and `DontAsk` is terminal.
```

- [ ] **Step 2: Add an "Allow rules & path rules" subsection**

After the "### Operator-aware deny matching" subsection, add:

```markdown
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
(`src/**`).

### The `.git`/`.ssh`/`.env` write breaker

A third built-in guard joins `destructive_defaults()`: a write whose target has
a `.git` or `.ssh` path component, or a final component `.env`/`.env.*`, is
`Ask` (→ Deny when headless). Component-exact, so `name.git/`, `.gitignore`, and
`environment.env` are unaffected. It runs before mode and before allow rules, so
a `.git/` write is refused even under `AcceptEdits` and even with a matching
`AllowRule::edit(".git/**")`. Disabled by `without_default_guards()`.
```

- [ ] **Step 3: Update the pipeline line + "How they compose"**

In the "## Permissions" intro and in "## How they compose", change the pipeline string from `deny rules › guard rules › mode › policy › AskUser` to:

```markdown
`deny rules › guard rules › allow rules › mode › policy › AskUser`
```

- [ ] **Step 4: Build the book — expect clean**

Run: `mdbook build docs/book`
Expected: success, no linkcheck warnings (`warning-policy = "error"`).

- [ ] **Step 5: README check**

Run: `grep -n "DenyRule\|PermissionMode\|permission" crates/paigasus-helikon-core/README.md`
If the README shows a permission example or the mode list, add `AllowRule`/`DontAsk` to match. If it does not mention permissions, no change (make this a conscious skip).

- [ ] **Step 6: Commit**

```bash
git add docs/book/src/concepts/permissions-guardrails-hooks.md
git add crates/paigasus-helikon-core/README.md 2>/dev/null || true
git commit -m "docs(core): SMA-415 document DontAsk, AllowRule, path rules & breaker"
```

---

## Task 10: Full CI-gate sweep

**Files:** none (verification only; commit any fixes the gates surface)

- [ ] **Step 1: Confirm `AllowRule` is reachable through the facade**

Run: `cargo build -p paigasus-helikon --all-features` then
`echo 'fn _t() { let _ = paigasus_helikon::core::AllowRule::tool("x"); }' >/dev/null` (sanity — the path resolves via the wholesale `pub use … as core`). No code change expected.

- [ ] **Step 2: Run every gate locally (mirrors `.github/workflows/ci.yml`)**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: all green. The new `pub` items (`AllowRule`, `DontAsk`, `DenyRule::read/edit`) carry `///` docs, so the `docs` job and the `missing_docs` lint stay clean.

- [ ] **Step 3: Doc-coverage gate**

```bash
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
```

Expected: ≥ 80%. (Requires `rustup toolchain install nightly-2026-05-01`; skip locally if unavailable and rely on CI.)

- [ ] **Step 4: Supply-chain gates**

```bash
cargo deny check 2>&1 | tail -10
```

Expected: advisories/licenses/bans pass (globset added in Task 1).

- [ ] **Step 5: Commit any fixes, then stop for review**

If a gate forced a change, commit it as `fix(core): SMA-415 <what>`. Otherwise the branch is ready for the PR.

---

## Self-review (completed by plan author)

**Spec coverage** — every spec section maps to a task:
- DontAsk variant + sticky/tighten-only → Task 6; pipeline deny → Task 7.
- AllowRule (tool/bash_command/read/edit) → Task 4; reach/short-circuit → Task 7.
- DenyRule path variants → Task 4.
- globset engine + normalization + case-insensitive + `..` collapse + advisory → Tasks 1, 2.
- `.git/.ssh/.env` breaker (Ask, component-exact, boundary tests) → Tasks 2, 3.
- 4 copy sites + propagation test → Tasks 5, 8.
- PartialEq normalization (#8) → Task 2 (`path_glob_eq_is_normalized`).
- Facade re-export (#9) → Task 10 Step 1 (automatic; verified, no edit).
- Docs (concept page + README) → Task 9.

**Placeholder scan** — no TBD/TODO; every code step shows complete code; every test shows assertions and the run command + expected result.

**Type consistency** — `PathGlob::new/matches_path`, `clean_path`, `is_protected_dotpath`, `path_arg_matches(_, _, PathKind, _)`, `AllowRule::{tool,bash_command,read,edit,matches}`, `DenyRule::{read,edit}`, `with_allow_rules`/`allow_rules()`, `PermissionFields.allow_rules`, `ToolContext.allow_rules`, `PermissionMode::DontAsk` — names are identical across Tasks 2→8. The breaker reuses `bash_command_str`, `command_match::{resolve_all, RedirectOp}` already present in `permission.rs`.

**One verification caveat for the executor:** Task 5 Step 1's test references `to_tool_context()`/`clone_permission_fields_len`; if `to_tool_context` isn't visible from the test module, use the `clone_permission_fields_len` shim assert (noted inline). Task 8 Step 1 must read the existing inner-tool's registered name rather than assume one.
