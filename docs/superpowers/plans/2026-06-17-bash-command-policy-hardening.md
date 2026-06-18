# Bash Command-Policy Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Spec:** [`docs/superpowers/specs/2026-06-17-sma-414-bash-command-policy-hardening-design.md`](../specs/2026-06-17-sma-414-bash-command-policy-hardening-design.md)

**Goal:** Harden `paigasus-helikon`'s Bash command policy with operator-aware deny matching, an always-on destructive-command circuit breaker that survives `PermissionMode::Bypass`, and automatic secret redaction of tool output.

**Architecture:** A new pure, dependency-free `command_match` tokenizer in core (redirection- and quote-aware) feeds an arg-aware `DenyRule` and a new pre-mode `GuardRule` (`Deny | Ask`) evaluated before permission mode, so destructive commands are gated even under Bypass. A separate `redaction` module scrubs secret-shaped strings from tool output as the final `PostToolUse` transform. `BashTool`'s tool-local deny/allow lists adopt the same tokenizer.

**Tech Stack:** Rust (edition 2024, MSRV 1.85), `serde_json`, `async-trait`, `tokio`; workspace crates `paigasus-helikon-core`, `paigasus-helikon-tools`, `paigasus-helikon` (facade).

---

## Conventions for every task

- **TDD:** write the failing test, run it red, implement, run it green, then `cargo fmt --all` + `cargo clippy --workspace --all-features --all-targets -- -D warnings`, then commit. (The pre-commit hook is a no-op; pre-push runs fmt/clippy — run them yourself before committing so the tree is always green.)
- **Commit scope** must satisfy the local `convco` commit-msg hook. Use `feat(core)`, `feat(tools)`, `feat(facade)`, `docs(book)`, `chore(release)`. Subject lowercase after `SMA-414`.
- **Never `git add -A`** (`.env`/`.claude` are untracked-but-not-ignored). Stage explicit paths.
- Commits are signed via a 1Password SSH key; if a commit fails with "failed to fill whole buffer", ask the user to unlock their vault — do **not** bypass signing.
- Run a single core test with `cargo test -p paigasus-helikon-core <name>`; tools with `cargo test -p paigasus-helikon-tools <name>`.

## File Structure

| File | Responsibility | Action |
|------|----------------|--------|
| `crates/paigasus-helikon-core/src/command_match.rs` | Pure shell tokenizer: `split_operators`, `resolve_command`, `shell_c_payload`, `resolve_all`; types `ResolvedCommand` / `Redirect` / `RedirectOp` | Create |
| `crates/paigasus-helikon-core/src/permission.rs` | Arg-aware `DenyRule` (`Matcher` enum); `GuardRule` / `GuardAction` / `GuardMatcher`; `GuardRule::destructive_defaults()` | Modify |
| `crates/paigasus-helikon-core/src/context.rs` | `RunContext` guard + redaction fields; `without_default_guards`; `clone_permission_fields` helper; child-context inheritance | Modify |
| `crates/paigasus-helikon-core/src/tool.rs` | `ToolContext` carriers for the new fields (`with_permissions` extension) | Modify |
| `crates/paigasus-helikon-core/src/control.rs` | `authorize()` pre-mode guard step | Modify |
| `crates/paigasus-helikon-core/src/redaction.rs` | `SecretSet`, `redact`, key-name + value scans, length/entropy floor | Create |
| `crates/paigasus-helikon-core/src/agent.rs` | Final post-tool redaction transform on `final_json` | Modify |
| `crates/paigasus-helikon-core/src/lib.rs` | Module declarations + public re-exports | Modify |
| `crates/paigasus-helikon-tools/src/bash.rs` | Operator-aware tool-local deny/allow + composition rule | Modify |
| `crates/paigasus-helikon-tools/tests/bash.rs` | Integration tests (compound deny, redaction) | Modify |
| `crates/paigasus-helikon/src/lib.rs` | Facade re-exports of `GuardRule` / `GuardAction` | Modify |
| `Cargo.toml` (root) + per-crate + `CHANGELOG.md`s | Version bumps + workspace pins | Modify |
| `docs/book/src/concepts/permissions-guardrails-hooks.md`, `crates/paigasus-helikon-tools/README.md` | User docs | Modify |

---

## Task 1: `command_match` — types + `split_operators`

**Files:**
- Create: `crates/paigasus-helikon-core/src/command_match.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs` (add `pub mod command_match;`)

- [ ] **Step 1: Declare the module**

In `crates/paigasus-helikon-core/src/lib.rs`, add alongside the other `mod` declarations:

```rust
pub mod command_match;
```

- [ ] **Step 2: Write the failing test**

Create `crates/paigasus-helikon-core/src/command_match.rs` with only the test module first:

```rust
//! Pure, dependency-free shell-command tokenizer used by the permission layer
//! to match Bash sub-commands. Models Claude Code's pragmatic Bash matcher —
//! not a full POSIX shell grammar. See the crate's permissions concept page for
//! the enumerated coverage and known bypasses.

#[cfg(test)]
mod split_tests {
    use super::*;

    #[test]
    fn splits_on_control_operators() {
        assert_eq!(split_operators("echo ok && rm -rf ."), vec!["echo ok", "rm -rf ."]);
        assert_eq!(split_operators("a; b | c || d"), vec!["a", "b", "c", "d"]);
        assert_eq!(split_operators("a |& b"), vec!["a", "b"]);
        assert_eq!(split_operators("a &\nb"), vec!["a", "b"]);
    }

    #[test]
    fn does_not_split_redirection_ampersands() {
        // 2>&1, >&2, &>file must NOT split on their '&'.
        assert_eq!(split_operators("cmd 2>&1"), vec!["cmd 2>&1"]);
        assert_eq!(split_operators("cmd >&2"), vec!["cmd >&2"]);
        assert_eq!(split_operators("cmd &>file"), vec!["cmd &>file"]);
    }

    #[test]
    fn does_not_split_inside_quotes() {
        assert_eq!(split_operators("echo 'a && b'"), vec!["echo 'a && b'"]);
        assert_eq!(split_operators("echo \"x ; y\""), vec!["echo \"x ; y\""]);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-core command_match::split_tests`
Expected: FAIL — `cannot find function split_operators in this scope`.

- [ ] **Step 4: Implement `split_operators`**

Add above the test module in `command_match.rs`:

```rust
/// Split a compound command on shell control operators (`&&`, `||`, `;`, `|`,
/// `|&`, `&`, newlines), quote- and redirection-aware. The `&` of a redirection
/// (`2>&1`, `>&2`, `&>file`) is never treated as a control operator, and
/// operators inside `'…'` / `"…"` are ignored. Empty segments are dropped.
pub fn split_operators(command: &str) -> Vec<&str> {
    let bytes = command.as_bytes();
    let n = bytes.len();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let (mut in_single, mut in_double) = (false, false);

    while i < n {
        let c = bytes[i];
        if in_single {
            if c == b'\'' { in_single = false; }
            i += 1;
            continue;
        }
        if in_double {
            if c == b'"' { in_double = false; }
            i += 1;
            continue;
        }
        match c {
            b'\'' => { in_single = true; i += 1; }
            b'"' => { in_double = true; i += 1; }
            b'\\' => { i += 2; }
            b'\n' | b';' => { push_segment(command, start, i, &mut segments); i += 1; start = i; }
            b'&' => {
                if i + 1 < n && bytes[i + 1] == b'&' {
                    push_segment(command, start, i, &mut segments);
                    i += 2;
                    start = i;
                } else if (i > 0 && bytes[i - 1] == b'>') || (i + 1 < n && bytes[i + 1] == b'>') {
                    i += 1; // part of a redirection (>&, &>)
                } else {
                    push_segment(command, start, i, &mut segments);
                    i += 1;
                    start = i;
                }
            }
            b'|' => {
                let span = if i + 1 < n && (bytes[i + 1] == b'|' || bytes[i + 1] == b'&') { 2 } else { 1 };
                push_segment(command, start, i, &mut segments);
                i += span;
                start = i;
            }
            _ => { i += 1; }
        }
    }
    push_segment(command, start, n, &mut segments);
    segments
}

fn push_segment<'a>(s: &'a str, start: usize, end: usize, out: &mut Vec<&'a str>) {
    let seg = s[start..end.min(s.len())].trim();
    if !seg.is_empty() {
        out.push(seg);
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p paigasus-helikon-core command_match::split_tests`
Expected: PASS (3 tests).

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/command_match.rs crates/paigasus-helikon-core/src/lib.rs
git commit -m "feat(core): SMA-414 add operator-aware command splitter"
```

---

## Task 2: `command_match` — `resolve_command`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/command_match.rs`

- [ ] **Step 1: Write the failing test**

Add a test module to `command_match.rs`:

```rust
#[cfg(test)]
mod resolve_tests {
    use super::*;

    fn prog(seg: &str) -> String {
        resolve_command(seg).unwrap().program
    }

    #[test]
    fn strips_env_assignments_and_wrappers() {
        assert_eq!(prog("FOO=bar rm x"), "rm");
        assert_eq!(prog("timeout 5 rm x"), "rm");
        assert_eq!(prog("nice -n 10 rm x"), "rm");
        assert_eq!(prog("sudo rm -rf /"), "rm");
        assert_eq!(prog("doas rm x"), "rm");
        assert_eq!(prog("env FOO=bar nohup stdbuf -oL rm x"), "rm");
    }

    #[test]
    fn unquotes_and_unescapes_the_program_token() {
        assert_eq!(prog(r"\rm -rf /"), "rm");
        assert_eq!(prog("'rm' -rf /"), "rm");
        assert_eq!(prog(r"r''m -rf /"), "rm");
    }

    #[test]
    fn parses_redirection_targets_spaced_glued_and_quoted() {
        let r = resolve_command("echo x > /etc/passwd").unwrap();
        assert_eq!(r.program, "echo");
        assert_eq!(r.redirects, vec![Redirect { op: RedirectOp::Out, target: "/etc/passwd".into() }]);

        let r = resolve_command("echo x >/etc/passwd").unwrap();
        assert_eq!(r.redirects, vec![Redirect { op: RedirectOp::Out, target: "/etc/passwd".into() }]);

        let r = resolve_command("echo x >> \"/etc/passwd\"").unwrap();
        assert_eq!(r.redirects, vec![Redirect { op: RedirectOp::Append, target: "/etc/passwd".into() }]);

        // fd-dup is not a path target
        let r = resolve_command("cmd 2>&1").unwrap();
        assert!(r.redirects.iter().all(|x| x.op != RedirectOp::Out && x.op != RedirectOp::Append));
    }

    #[test]
    fn empty_segment_is_none() {
        assert!(resolve_command("   ").is_none());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-core command_match::resolve_tests`
Expected: FAIL — `ResolvedCommand` / `Redirect` / `resolve_command` not found.

- [ ] **Step 3: Implement the types and `resolve_command`**

Add to `command_match.rs` (above the test modules):

```rust
/// A parsed redirection (only the kinds the guard layer cares about).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectOp {
    /// `>` / `N>` / `&>` — truncating write to a path.
    Out,
    /// `>>` / `N>>` / `&>>` — appending write to a path.
    Append,
    /// `>&` / `N>&M` — file-descriptor duplication (no path target).
    FdDup,
}

/// One redirection of a sub-command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    /// The redirection kind.
    pub op: RedirectOp,
    /// The (unquoted) target. Empty for `FdDup`.
    pub target: String,
}

/// A single sub-command after wrapper-stripping and quote removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCommand {
    /// The effective program token (unquoted/unescaped, wrappers removed).
    pub program: String,
    /// Remaining argument tokens (unquoted), redirections excluded.
    pub args: Vec<String>,
    /// Parsed redirections.
    pub redirects: Vec<Redirect>,
}

/// Wrappers stripped from the front of a sub-command before resolving the
/// program. `env`/`nice`/`timeout`/`stdbuf` may carry their own flags/args;
/// we skip a leading run of `-flag`/value tokens after them pragmatically.
const WRAPPERS: &[&str] = &["timeout", "nice", "nohup", "stdbuf", "env", "command", "sudo", "doas"];

/// Resolve one segment (already split by [`split_operators`]) into its effective
/// program, args, and redirections. Returns `None` for an empty segment.
pub fn resolve_command(segment: &str) -> Option<ResolvedCommand> {
    let (mut words, redirects) = tokenize(segment);
    // Strip leading env-assignments (`FOO=bar`) and known wrappers + their args.
    loop {
        let Some(first) = words.first() else { return None };
        if is_env_assignment(first) {
            words.remove(0);
            continue;
        }
        if WRAPPERS.contains(&first.as_str()) {
            words.remove(0);
            // Skip a run of option-like tokens / their values (pragmatic).
            while let Some(w) = words.first() {
                if w.starts_with('-') {
                    words.remove(0);
                    // `-n 10` style: drop a following bare value for nice/timeout.
                    if matches!(words.first(), Some(v) if !v.starts_with('-') && v.chars().all(|c| c.is_ascii_digit())) {
                        words.remove(0);
                    }
                } else {
                    break;
                }
            }
            continue;
        }
        break;
    }
    let program = words.first()?.clone();
    let args = words.split_off(1);
    let _ = args; // args[0] removed below
    let mut iter = words.into_iter();
    let program = iter.next().unwrap_or(program);
    let args: Vec<String> = iter.collect();
    Some(ResolvedCommand { program, args, redirects })
}

fn is_env_assignment(tok: &str) -> bool {
    let Some(eq) = tok.find('=') else { return false };
    let name = &tok[..eq];
    !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
```

> **Note for the implementer:** the `program`/`args` reshuffle above is
> intentionally written so `args` excludes the program token. Simplify if your
> green implementation is cleaner — the tests below are the contract.

- [ ] **Step 4: Implement `tokenize` (word + redirection splitter)**

Add to `command_match.rs`:

```rust
/// Split a single segment into words and redirections, quote-aware.
/// ASCII-pragmatic: multibyte content only ever appears inside quoted args,
/// which never affect program/redirect detection.
fn tokenize(segment: &str) -> (Vec<String>, Vec<Redirect>) {
    let bytes = segment.as_bytes();
    let n = bytes.len();
    let mut words = Vec::new();
    let mut redirects = Vec::new();
    let mut i = 0;

    while i < n {
        while i < n && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i >= n {
            break;
        }
        // Redirection? optional leading fd digits or '&', then '>'/'<'.
        let mut j = i;
        while j < n && bytes[j].is_ascii_digit() {
            j += 1;
        }
        let amp = bytes[i] == b'&' && i + 1 < n && bytes[i + 1] == b'>';
        if amp || (j < n && (bytes[j] == b'>' || bytes[j] == b'<')) {
            let (redir, next) = parse_redirect(segment, i);
            if let Some(r) = redir {
                redirects.push(r);
            }
            i = next;
            continue;
        }
        let (word, next) = read_word(segment, i);
        if !word.is_empty() || next > i {
            words.push(word);
        }
        i = next;
    }
    (words, redirects)
}

/// Parse a redirection starting at `start`. Returns the redirect (None for `<`
/// input redirections, which the guard layer ignores) and the next index.
fn parse_redirect(s: &str, start: usize) -> (Option<Redirect>, usize) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = start;
    while i < n && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'&') {
        i += 1; // `&>`
    }
    if bytes.get(i) == Some(&b'<') {
        // input redirection — consume operator + target, ignore.
        i += 1;
        let (_t, next) = read_redirect_target(s, i);
        return (None, next);
    }
    // now at '>'
    let mut op = RedirectOp::Out;
    if bytes.get(i) == Some(&b'>') {
        i += 1;
        if bytes.get(i) == Some(&b'>') {
            op = RedirectOp::Append;
            i += 1;
        } else if bytes.get(i) == Some(&b'&') {
            op = RedirectOp::FdDup;
            i += 1;
        }
    }
    if op == RedirectOp::FdDup {
        // `>&2` — consume the fd number, no path target.
        let (_t, next) = read_redirect_target(s, i);
        return (Some(Redirect { op, target: String::new() }), next);
    }
    let (target, next) = read_redirect_target(s, i);
    (Some(Redirect { op, target }), next)
}

/// Read a redirection target: skip spaces, then read one (possibly quoted) word.
fn read_redirect_target(s: &str, start: usize) -> (String, usize) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = start;
    while i < n && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    read_word(s, i)
}

/// Read one whitespace-delimited word, removing quotes and backslash escapes.
fn read_word(s: &str, start: usize) -> (String, usize) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = start;
    let mut out = String::new();
    while i < n {
        match bytes[i] {
            b' ' | b'\t' | b'>' | b'<' => break,
            b'\'' => {
                i += 1;
                while i < n && bytes[i] != b'\'' {
                    out.push(bytes[i] as char);
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < n && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < n {
                        i += 1;
                    }
                    out.push(bytes[i] as char);
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
            }
            b'\\' => {
                if i + 1 < n {
                    out.push(bytes[i + 1] as char);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    (out, i)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core command_match::resolve_tests`
Expected: PASS (4 tests). Re-run `command_match::split_tests` to confirm no regression.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/command_match.rs
git commit -m "feat(core): SMA-414 resolve sub-command program, args, redirects"
```

---

## Task 3: `command_match` — `shell_c_payload` + `resolve_all`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/command_match.rs`

- [ ] **Step 1: Write the failing test**

Add to `command_match.rs`:

```rust
#[cfg(test)]
mod resolve_all_tests {
    use super::*;

    fn programs(cmd: &str) -> Vec<String> {
        resolve_all(cmd).into_iter().map(|c| c.program).collect()
    }

    #[test]
    fn flattens_compound_commands() {
        assert_eq!(programs("echo ok && rm -rf ."), vec!["echo", "rm"]);
    }

    #[test]
    fn recurses_into_shell_c() {
        assert!(programs("bash -c 'rm -rf /'").contains(&"rm".to_string()));
        assert!(programs("sh -c \"echo hi && rm x\"").contains(&"rm".to_string()));
    }

    #[test]
    fn recursion_is_depth_bounded() {
        // Deeply nested -c beyond MAX_REENTRY_DEPTH must not loop forever.
        let nested = "bash -c 'bash -c \"bash -c \\\"bash -c rm\\\"\"'";
        let _ = resolve_all(nested); // must terminate
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-core command_match::resolve_all_tests`
Expected: FAIL — `resolve_all` not found.

- [ ] **Step 3: Implement `shell_c_payload` and `resolve_all`**

Add to `command_match.rs`:

```rust
/// Maximum `bash -c` / `sh -c` re-entry depth the matcher follows.
pub const MAX_REENTRY_DEPTH: usize = 3;

const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash"];

/// If `cmd` invokes a known shell with a `-c <string>` argument, return the
/// inner command string for re-parsing.
pub fn shell_c_payload(cmd: &ResolvedCommand) -> Option<&str> {
    if !SHELLS.contains(&cmd.program.as_str()) {
        return None;
    }
    let mut it = cmd.args.iter();
    while let Some(a) = it.next() {
        if a == "-c" {
            return it.next().map(String::as_str);
        }
        if let Some(rest) = a.strip_prefix("-c") {
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }
    None
}

/// Split a compound command and resolve every sub-command, following
/// `bash -c` / `sh -c` re-entry up to [`MAX_REENTRY_DEPTH`].
pub fn resolve_all(command: &str) -> Vec<ResolvedCommand> {
    let mut out = Vec::new();
    resolve_into(command, 0, &mut out);
    out
}

fn resolve_into(command: &str, depth: usize, out: &mut Vec<ResolvedCommand>) {
    for seg in split_operators(command) {
        let Some(cmd) = resolve_command(seg) else { continue };
        if depth < MAX_REENTRY_DEPTH {
            if let Some(inner) = shell_c_payload(&cmd) {
                let inner = inner.to_owned();
                out.push(cmd);
                resolve_into(&inner, depth + 1, out);
                continue;
            }
        }
        out.push(cmd);
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core command_match`
Expected: PASS (all `command_match` test modules).

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/command_match.rs
git commit -m "feat(core): SMA-414 flatten compound commands with bash -c re-entry"
```

---

## Task 4: arg-aware `DenyRule`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/permission.rs`

- [ ] **Step 1: Write the failing test**

Replace the existing `deny_rule_matches_exact_tool_name_only` test body region by adding these tests in `permission.rs`'s `tests` module (keep the existing test):

```rust
    #[test]
    fn bash_command_matches_any_subcommand_program() {
        let rule = DenyRule::bash_command("rm");
        let args = json!({ "command": "echo ok && rm -rf ." });
        assert!(rule.matches("Bash", &args));
        // first-token-only matching would have missed this.
        let safe = json!({ "command": "echo ok && ls" });
        assert!(!rule.matches("Bash", &safe));
    }

    #[test]
    fn bash_command_is_tool_scoped() {
        let rule = DenyRule::bash_command("rm");
        // A non-Bash tool carrying a `command` field must not trip it.
        assert!(!rule.matches("Other", &json!({ "command": "rm -rf ." })));
    }

    #[test]
    fn bash_command_sees_through_sudo_and_bash_c() {
        let rule = DenyRule::bash_command("rm");
        assert!(rule.matches("Bash", &json!({ "command": "sudo rm -rf /" })));
        assert!(rule.matches("Bash", &json!({ "command": "bash -c 'rm -rf /'" })));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-core permission`
Expected: FAIL — `DenyRule::bash_command` not found.

- [ ] **Step 3: Refactor `DenyRule` to the `Matcher` enum**

In `permission.rs`, replace the `DenyRule` struct and its impl:

```rust
/// How a [`DenyRule`] matches a call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Matcher {
    /// Exact tool name.
    Tool(String),
    /// Any Bash sub-command whose resolved program equals this. Tool-scoped to
    /// the `Bash` tool.
    BashProgram(String),
}

/// A first-class deny rule, evaluated **before** mode — so it overrides even
/// [`PermissionMode::Bypass`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyRule {
    matcher: Matcher,
}

impl DenyRule {
    /// Deny a tool by its exact name.
    pub fn tool(name: impl Into<String>) -> Self {
        Self { matcher: Matcher::Tool(name.into()) }
    }

    /// Deny a Bash call whose compound command contains a sub-command whose
    /// resolved program equals `program` (operator-, wrapper-, and
    /// `bash -c`-aware). Only matches the `Bash` tool.
    pub fn bash_command(program: impl Into<String>) -> Self {
        Self { matcher: Matcher::BashProgram(program.into()) }
    }

    /// `true` if this rule denies `tool` invoked with `args`.
    pub fn matches(&self, tool: &str, args: &serde_json::Value) -> bool {
        match &self.matcher {
            Matcher::Tool(name) => name == tool,
            Matcher::BashProgram(program) => {
                if tool != "Bash" {
                    return false;
                }
                let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
                    return false;
                };
                crate::command_match::resolve_all(command)
                    .iter()
                    .any(|c| &c.program == program)
            }
        }
    }
}
```

Keep the existing `deny_rule_matches_exact_tool_name_only` test — `DenyRule::tool` and `matches` still behave identically for the `Tool` matcher.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core permission`
Expected: PASS (existing + 3 new tests).

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/permission.rs
git commit -m "feat(core): SMA-414 add operator-aware DenyRule::bash_command"
```

---

## Task 5: `GuardRule` + destructive defaults

**Files:**
- Modify: `crates/paigasus-helikon-core/src/permission.rs`

- [ ] **Step 1: Write the failing test**

Add a new test module at the bottom of `permission.rs`:

```rust
#[cfg(test)]
mod guard_tests {
    use super::*;
    use serde_json::json;

    fn matched(cmd: &str) -> bool {
        let bash = json!({ "command": cmd });
        GuardRule::destructive_defaults().iter().any(|g| g.matches("Bash", &bash))
    }

    #[test]
    fn matches_rm_rf_root_and_home() {
        assert!(matched("rm -rf /"));
        assert!(matched("rm -rf ~"));
        assert!(matched("rm -fr /"));
        assert!(matched("sudo rm -rf /"));
        assert!(matched("bash -c 'rm -rf /'"));
        assert!(matched("rm -rf / tmp")); // spacing bug
    }

    #[test]
    fn ignores_safe_rm() {
        assert!(!matched("rm -rf ./build"));
        assert!(!matched("rm file.txt"));
    }

    #[test]
    fn matches_protected_path_write_but_allows_dev_null() {
        assert!(matched("echo x > /etc/passwd"));
        assert!(matched("echo x >/etc/passwd"));
        assert!(matched("tee /etc/hosts"));
        assert!(!matched("echo x > /dev/null"));
        assert!(!matched("cmd 2> /dev/null"));
    }

    #[test]
    fn protected_path_write_matches_write_tool_path_arg() {
        let g = &GuardRule::destructive_defaults();
        let write = json!({ "path": "/etc/passwd", "content": "x" });
        assert!(g.iter().any(|r| r.matches("Write", &write)));
        let safe = json!({ "path": "./notes.txt", "content": "x" });
        assert!(!g.iter().any(|r| r.matches("Write", &safe)));
    }

    #[test]
    fn destructive_defaults_use_ask_action() {
        assert!(GuardRule::destructive_defaults()
            .iter()
            .all(|g| matches!(g.action(), GuardAction::Ask { .. })));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-core guard_tests`
Expected: FAIL — `GuardRule` / `GuardAction` not found.

- [ ] **Step 3: Implement `GuardRule`, `GuardAction`, `GuardMatcher`**

Add to `permission.rs`:

```rust
/// The action a tripped [`GuardRule`] takes. Evaluated **before** mode, so it
/// overrides even [`PermissionMode::Bypass`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuardAction {
    /// Hard-deny with a reason.
    Deny {
        /// Human-readable denial reason.
        reason: String,
    },
    /// Ask a human via the [`ApprovalHandler`] (default Deny when none).
    Ask {
        /// Prompt shown to the approver.
        prompt: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GuardMatcher {
    /// `rm` with recursive+force flags targeting `/` or `~` (literal).
    RmRecursiveRootOrHome,
    /// A write whose target resolves under a protected prefix (Bash redirects,
    /// `tee`/`dd`, or the Write/Edit `path` arg). Honors the device-node allowlist.
    ProtectedPathWrite,
}

/// A pre-mode safety rule. Like [`DenyRule`] it runs before permission mode and
/// beats `Bypass`, but it may **ask** a human instead of hard-denying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardRule {
    matcher: GuardMatcher,
    action: GuardAction,
}

/// Protected path prefixes. A write resolving under any of these trips
/// [`GuardMatcher::ProtectedPathWrite`].
const PROTECTED_PREFIXES: &[&str] =
    &["/etc", "/usr", "/bin", "/sbin", "/sys", "/boot", "/dev"];

/// Device nodes that are safe write targets despite the `/dev` prefix. Checked
/// before the protected-prefix rule so `cmd > /dev/null` is never denied.
const DEVICE_ALLOWLIST: &[&str] = &[
    "/dev/null", "/dev/zero", "/dev/full", "/dev/stdout", "/dev/stderr",
    "/dev/tty", "/dev/random", "/dev/urandom",
];

impl GuardRule {
    /// The action this rule takes when it matches.
    pub fn action(&self) -> &GuardAction {
        &self.action
    }

    /// `true` if this guard trips for `tool` invoked with `args`.
    pub fn matches(&self, tool: &str, args: &serde_json::Value) -> bool {
        match self.matcher {
            GuardMatcher::RmRecursiveRootOrHome => {
                let Some(cmd) = bash_command_str(tool, args) else { return false };
                crate::command_match::resolve_all(cmd).iter().any(is_rm_rf_root_or_home)
            }
            GuardMatcher::ProtectedPathWrite => protected_path_write(tool, args),
        }
    }

    /// The always-on destructive guard set: `rm -rf /`, `rm -rf ~`, and
    /// protected-path writes. All default to [`GuardAction::Ask`].
    pub fn destructive_defaults() -> Vec<GuardRule> {
        vec![
            GuardRule {
                matcher: GuardMatcher::RmRecursiveRootOrHome,
                action: GuardAction::Ask {
                    prompt: "destructive command: recursive force-remove of / or ~".to_owned(),
                },
            },
            GuardRule {
                matcher: GuardMatcher::ProtectedPathWrite,
                action: GuardAction::Ask {
                    prompt: "write to a protected system path".to_owned(),
                },
            },
        ]
    }
}

fn bash_command_str<'a>(tool: &str, args: &'a serde_json::Value) -> Option<&'a str> {
    if tool != "Bash" {
        return None;
    }
    args.get("command").and_then(|v| v.as_str())
}

fn is_rm_rf_root_or_home(cmd: &crate::command_match::ResolvedCommand) -> bool {
    if cmd.program != "rm" {
        return false;
    }
    let mut recursive = false;
    let mut force = false;
    let mut targets: Vec<&str> = Vec::new();
    for a in &cmd.args {
        if a.starts_with("--") {
            match a.as_str() {
                "--recursive" => recursive = true,
                "--force" => force = true,
                _ => {}
            }
        } else if let Some(flags) = a.strip_prefix('-') {
            if flags.contains('r') || flags.contains('R') {
                recursive = true;
            }
            if flags.contains('f') {
                force = true;
            }
        } else {
            targets.push(a);
        }
    }
    recursive && force && targets.iter().any(|t| is_root_or_home(t))
}

fn is_root_or_home(target: &str) -> bool {
    matches!(target, "/" | "/*" | "~" | "~/" | "${HOME}" | "$HOME")
}

fn protected_path_write(tool: &str, args: &serde_json::Value) -> bool {
    // Structured Write/Edit path arg.
    if matches!(tool, "Write" | "Edit") {
        if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
            return is_protected_path(p);
        }
    }
    // Bash redirects + tee/dd targets.
    if let Some(cmd) = bash_command_str(tool, args) {
        for c in crate::command_match::resolve_all(cmd) {
            for r in &c.redirects {
                use crate::command_match::RedirectOp;
                if matches!(r.op, RedirectOp::Out | RedirectOp::Append) && is_protected_path(&r.target) {
                    return true;
                }
            }
            if c.program == "tee" && c.args.iter().any(|a| is_protected_path(a)) {
                return true;
            }
            if c.program == "dd" {
                if let Some(of) = c.args.iter().find_map(|a| a.strip_prefix("of=")) {
                    if is_protected_path(of) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn is_protected_path(path: &str) -> bool {
    if DEVICE_ALLOWLIST.contains(&path) {
        return false;
    }
    if path == "/" {
        return true;
    }
    PROTECTED_PREFIXES.iter().any(|p| path == *p || path.starts_with(&format!("{p}/")))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core guard_tests`
Expected: PASS (5 tests).

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/permission.rs
git commit -m "feat(core): SMA-414 add GuardRule and destructive-command defaults"
```

---

## Task 6: `RunContext` / `ToolContext` guard + redaction config

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs`
- Modify: `crates/paigasus-helikon-core/src/tool.rs`

- [ ] **Step 1: Write the failing test**

Add to `context.rs`'s `runcontext_tests` module:

```rust
    #[test]
    fn guard_rules_default_on_and_inherit_through_children() {
        use crate::{GuardRule, PermissionMode};
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .with_permission_mode(PermissionMode::Bypass)
        .with_guard_rules(vec![GuardRule::destructive_defaults().remove(0)]);

        assert!(ctx.default_guards());
        assert_eq!(ctx.guard_rules().len(), 1);
        // inheritance through all three child paths
        assert_eq!(ctx.handoff_child().guard_rules().len(), 1);
        assert_eq!(ctx.subagent_child().guard_rules().len(), 1);
        assert_eq!(ctx.to_tool_context().guard_rules().len(), 1);
        assert!(ctx.handoff_child().default_guards());
    }

    #[test]
    fn without_default_guards_disables_builtins() {
        let ctx: RunContext<()> = RunContext::new(
            Arc::new(()),
            Arc::new(MemorySession::new()) as Arc<dyn Session>,
            HookRegistry::new(),
            TracerHandle::default(),
            CancellationToken::new(),
        )
        .without_default_guards();
        assert!(!ctx.default_guards());
        assert!(!ctx.subagent_child().default_guards());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-core runcontext_tests`
Expected: FAIL — `with_guard_rules` / `default_guards` / `guard_rules` not found.

- [ ] **Step 3: Add fields, builders, accessors, and inheritance to `RunContext`**

In `context.rs`, add to the `RunContext` struct:

```rust
    /// User-supplied guard rules, evaluated before mode (can Ask or Deny).
    guard_rules: Vec<crate::GuardRule>,
    /// Whether the always-on destructive default guards are consulted.
    default_guards: bool,
    /// Whether tool output is redacted before re-entering context.
    redact_output: bool,
    /// Extra secret values to redact, beyond the auto-sourced env set.
    extra_secrets: Vec<String>,
```

In `RunContext::new`, initialize them:

```rust
            guard_rules: Vec::new(),
            default_guards: true,
            redact_output: true,
            extra_secrets: Vec::new(),
```

Add builders + accessors:

```rust
    /// Install user guard rules (evaluated before mode; can Ask or Deny).
    pub fn with_guard_rules(mut self, rules: Vec<crate::GuardRule>) -> Self {
        self.guard_rules = rules;
        self
    }

    /// Disable the always-on built-in destructive guard set (power-user opt-out).
    pub fn without_default_guards(mut self) -> Self {
        self.default_guards = false;
        self
    }

    /// Disable automatic secret redaction of tool output.
    pub fn without_output_redaction(mut self) -> Self {
        self.redact_output = false;
        self
    }

    /// Add extra secret values to redact from tool output.
    pub fn with_extra_secrets(mut self, secrets: Vec<String>) -> Self {
        self.extra_secrets = secrets;
        self
    }

    /// The run's user guard rules.
    pub fn guard_rules(&self) -> &[crate::GuardRule] {
        &self.guard_rules
    }

    /// Whether built-in destructive guards are active.
    pub fn default_guards(&self) -> bool {
        self.default_guards
    }

    /// Whether tool-output redaction is active.
    pub fn redact_output(&self) -> bool {
        self.redact_output
    }

    /// Extra secret values to redact.
    pub fn extra_secrets(&self) -> &[String] {
        &self.extra_secrets
    }
```

In **`handoff_child`** and **`subagent_child`**, add to the struct literal (alongside `deny_rules: self.deny_rules.clone()`):

```rust
            guard_rules: self.guard_rules.clone(),
            default_guards: self.default_guards,
            redact_output: self.redact_output,
            extra_secrets: self.extra_secrets.clone(),
```

In **`with_state`** (the `pub(crate)` one) the literal is not used — it mutates self, so no change needed there. Verify every `Self { … }` literal in `context.rs` includes the four new fields (the compiler will error on any you miss).

- [ ] **Step 4: Thread the fields into `ToolContext`**

In `tool.rs`, extend `ToolContext` with `pub(crate)` carriers (read by `agent_as_tool` rebuild):

```rust
    pub(crate) guard_rules: Vec<crate::GuardRule>,
    pub(crate) default_guards: bool,
    pub(crate) redact_output: bool,
    pub(crate) extra_secrets: Vec<String>,
```

Initialize them in `ToolContext::new` (`Vec::new()`, `true`, `true`, `Vec::new()`), and extend `with_permissions` to accept and set them. Add an accessor used by the test:

```rust
    /// The run's user guard rules (carrier for `agent_as_tool` rebuild).
    pub fn guard_rules(&self) -> &[crate::GuardRule] {
        &self.guard_rules
    }
```

In `context.rs::to_tool_context`, pass the four fields through `with_permissions` (extend its signature accordingly).

> **Implementer note:** to avoid the four-fields-in-five-places drift, introduce
> a private `struct PermissionFields { mode, policy, deny_rules, approval_handler,
> guard_rules, default_guards, redact_output, extra_secrets }` plus
> `RunContext::clone_permission_fields(&self) -> PermissionFields`, and use it in
> the three child constructors. The inheritance test above is what proves it.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core runcontext_tests`
Expected: PASS. Then `cargo test -p paigasus-helikon-core` to confirm no regressions in the existing context/tool tests.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/context.rs crates/paigasus-helikon-core/src/tool.rs
git commit -m "feat(core): SMA-414 carry guard + redaction config through run contexts"
```

---

## Task 7: `authorize()` pre-mode guard step

**Files:**
- Modify: `crates/paigasus-helikon-core/src/control.rs`

- [ ] **Step 1: Write the failing test**

Add to `control.rs`'s `authorize_tests` module:

```rust
    #[tokio::test]
    async fn destructive_guard_denies_under_bypass_without_handler() {
        let c = ctx().with_permission_mode(PermissionMode::Bypass);
        let i = interceptors(&c);
        let args = json!({ "command": "rm -rf /" });
        assert!(matches!(
            i.authorize("Bash", ToolEffect::SideEffect, &args).await,
            PermissionDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn destructive_guard_asks_under_bypass_with_handler() {
        let c = ctx()
            .with_permission_mode(PermissionMode::Bypass)
            .with_approval_handler(Arc::new(AllowHandler));
        let i = interceptors(&c);
        let args = json!({ "command": "rm -rf /" });
        assert!(matches!(
            i.authorize("Bash", ToolEffect::SideEffect, &args).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn without_default_guards_lets_bypass_allow_destructive() {
        let c = ctx()
            .with_permission_mode(PermissionMode::Bypass)
            .without_default_guards();
        let i = interceptors(&c);
        let args = json!({ "command": "rm -rf /" });
        assert!(matches!(
            i.authorize("Bash", ToolEffect::SideEffect, &args).await,
            PermissionDecision::Allow
        ));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-core authorize_tests`
Expected: FAIL — `rm -rf /` is allowed under Bypass (no guard step yet).

- [ ] **Step 3: Insert the guard step into `authorize`**

In `control.rs::authorize`, immediately after the deny-rules block (step 1) and **before** the mode `match` (step 2), add:

```rust
        // 1a/1b. Guard rules — built-in destructive defaults (unless opted out)
        // then user guard rules. Run before mode, so they beat Bypass; may Ask.
        let builtin = if self.ctx.default_guards() {
            crate::GuardRule::destructive_defaults()
        } else {
            Vec::new()
        };
        for guard in builtin.iter().chain(self.ctx.guard_rules()) {
            if guard.matches(tool, args) {
                return match guard.action() {
                    crate::GuardAction::Deny { reason } => {
                        PermissionDecision::Deny { reason: reason.clone() }
                    }
                    crate::GuardAction::Ask { prompt } => match self.ctx.approval_handler() {
                        None => PermissionDecision::Deny {
                            reason: format!("destructive command requires approval: {prompt}"),
                        },
                        Some(handler) => match handler.decide(tool, prompt, args).await {
                            crate::ApprovalOutcome::Allow => PermissionDecision::Allow,
                            crate::ApprovalOutcome::Deny { reason } => {
                                PermissionDecision::Deny { reason }
                            }
                        },
                    },
                };
            }
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core authorize_tests`
Expected: PASS (existing + 3 new). Confirm `deny_rule_beats_bypass` still passes.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/control.rs
git commit -m "feat(core): SMA-414 evaluate guard rules before mode in authorize"
```

---

## Task 8: `redaction` module

**Files:**
- Create: `crates/paigasus-helikon-core/src/redaction.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs` (`pub mod redaction;`)

- [ ] **Step 1: Declare the module + write the failing test**

Add `pub mod redaction;` to `lib.rs`. Create `redaction.rs`:

```rust
//! Secret redaction for tool output. Scrubs secret-shaped strings (by key-name
//! pattern and by known env value) before tool output re-enters the model
//! context or the session trajectory.

use serde_json::Value;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(values: &[&str]) -> SecretSet {
        SecretSet::from_values(values.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn redacts_key_name_patterns() {
        let out = redact(&json!({ "stdout": "OPENAI_API_KEY=sk-abc123xyz\n" }), &set(&[]));
        assert_eq!(out["stdout"], "OPENAI_API_KEY=***\n");
        let out = redact(&json!({ "stdout": "export DB_PASSWORD=hunter2pass" }), &set(&[]));
        assert_eq!(out["stdout"], "export DB_PASSWORD=***");
        let out = redact(&json!({ "stdout": "AUTH_TOKEN: abcdefgh12" }), &set(&[]));
        assert_eq!(out["stdout"], "AUTH_TOKEN: ***");
    }

    #[test]
    fn redacts_known_env_values() {
        let out = redact(&json!({ "stdout": "using sk-abc123xyz to auth" }), &set(&["sk-abc123xyz"]));
        assert_eq!(out["stdout"], "using *** to auth");
    }

    #[test]
    fn value_scan_has_a_length_floor() {
        // Short/common values are dropped from the set, so output is untouched.
        let s = SecretSet::from_values(vec!["dev".into(), "true".into(), "1234".into()]);
        let out = redact(&json!({ "stdout": "mode=dev ok=true n=1234" }), &s);
        assert_eq!(out["stdout"], "mode=dev ok=true n=1234");
    }

    #[test]
    fn walks_nested_json_and_is_idempotent() {
        let v = json!({ "a": { "b": ["X_TOKEN=abcdefgh12"] } });
        let once = redact(&v, &set(&[]));
        assert_eq!(once["a"]["b"][0], "X_TOKEN=***");
        assert_eq!(redact(&once, &set(&[])), once);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-core redaction`
Expected: FAIL — `SecretSet` / `redact` not found.

- [ ] **Step 3: Implement `SecretSet` and `redact`**

Add to `redaction.rs`:

```rust
/// Suffixes (case-insensitive) that mark a key as secret-shaped.
const SECRET_SUFFIXES: &[&str] =
    &["_API_KEY", "_TOKEN", "_SECRET", "_PASSWORD", "_CREDENTIAL"];

/// Minimum length for a value to be eligible for the value-scan (avoids
/// corrupting output by matching short/common substrings).
const MIN_SECRET_LEN: usize = 8;

const REDACTED: &str = "***";

/// A snapshot of secret values eligible for literal value-scanning. Built once
/// per run; values below [`MIN_SECRET_LEN`] or that look like common words are
/// dropped.
#[derive(Debug, Clone, Default)]
pub struct SecretSet {
    values: Vec<String>,
}

impl SecretSet {
    /// Build a set from explicit values, applying the length/entropy floor.
    pub fn from_values(values: Vec<String>) -> Self {
        let values = values
            .into_iter()
            .filter(|v| v.len() >= MIN_SECRET_LEN && !is_common_word(v))
            .collect();
        Self { values }
    }

    /// Snapshot the parent process environment: take the values of variables
    /// whose names match a secret suffix, then `extra`, applying the floor.
    pub fn from_env_and_extra(extra: &[String]) -> Self {
        let mut values: Vec<String> = std::env::vars()
            .filter(|(name, _)| name_is_secret(name))
            .map(|(_, val)| val)
            .collect();
        values.extend(extra.iter().cloned());
        Self::from_values(values)
    }
}

fn name_is_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_SUFFIXES.iter().any(|s| upper.ends_with(s))
}

fn is_common_word(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "true" | "false" | "dev" | "prod" | "test" | "none" | "null")
        || v.chars().all(|c| c.is_ascii_digit())
}

/// Walk `value`, redacting every string within. Never errors.
pub fn redact(value: &Value, secrets: &SecretSet) -> Value {
    match value {
        Value::String(s) => Value::String(redact_str(s, secrets)),
        Value::Array(a) => Value::Array(a.iter().map(|v| redact(v, secrets)).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), redact(v, secrets))).collect())
        }
        other => other.clone(),
    }
}

fn redact_str(s: &str, secrets: &SecretSet) -> String {
    let mut out = redact_key_values(s);
    for secret in &secrets.values {
        if out.contains(secret) {
            out = out.replace(secret, REDACTED);
        }
    }
    out
}

/// Replace the value in `KEY=val` / `KEY: val` / `export KEY=val` where KEY ends
/// in a secret suffix. Operates line-by-line, token-wise.
fn redact_key_values(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for (idx, line) in s.split_inclusive('\n').enumerate() {
        let _ = idx;
        result.push_str(&redact_line(line));
    }
    result
}

fn redact_line(line: &str) -> String {
    // Find `KEY=` or `KEY:` separators; redact the rest of the token (up to ws).
    for sep in ['=', ':'] {
        if let Some(pos) = line.find(sep) {
            let (head, tail) = line.split_at(pos);
            let key = head.rsplit([' ', '\t']).next().unwrap_or(head).trim();
            if name_is_secret(key) {
                let after = &tail[1..]; // skip sep
                // preserve a single leading space after ':'
                let (space, rest) = match after.strip_prefix(' ') {
                    Some(r) => (" ", r),
                    None => ("", after),
                };
                let trailing_ws: String = rest.chars().rev().take_while(|c| c.is_whitespace()).collect();
                let trailing: String = trailing_ws.chars().rev().collect();
                if rest.trim().is_empty() {
                    return line.to_string(); // KEY= with no value: leave as-is
                }
                return format!("{head}{sep}{space}{REDACTED}{trailing}");
            }
        }
    }
    line.to_string()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core redaction`
Expected: PASS (4 tests).

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/redaction.rs crates/paigasus-helikon-core/src/lib.rs
git commit -m "feat(core): SMA-414 add secret-redaction module"
```

---

## Task 9: wire redaction into the post-tool path

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs`

- [ ] **Step 1: Write the failing test**

Add an integration test at the bottom of `agent.rs` (or extend an existing agent-loop test module). If `agent.rs` has no convenient unit harness for `run_tools_concurrent`, add this as `crates/paigasus-helikon-core/tests/redaction_wiring.rs` driving a minimal agent run with a tool that echoes a secret. Concretely, add to `agent.rs`'s test module a focused test of the redaction transform helper:

```rust
#[cfg(test)]
mod redaction_wiring_tests {
    use crate::redaction::{redact, SecretSet};
    use serde_json::json;

    #[test]
    fn final_json_is_redacted_after_hooks() {
        // The post-tool transform applies redaction to the final JSON.
        let secrets = SecretSet::from_values(vec![]);
        let output = json!({ "stdout": "FOO_API_KEY=supersecretvalue" });
        let redacted = redact(&output, &secrets);
        assert_eq!(redacted["stdout"], "FOO_API_KEY=***");
    }
}
```

> **Implementer note:** the unit test above pins the transform; the wiring itself
> (that `run_tools_concurrent` actually calls `redact` on `final_json`) is
> verified end-to-end by the `BashTool` integration test in Task 11
> (`FOO_API_KEY=secret` echo → `***`). Keep both.

- [ ] **Step 2: Run the test to verify it fails (compile-time)**

Run: `cargo test -p paigasus-helikon-core redaction_wiring`
Expected: PASS only after the import path exists; if `redaction` isn't `pub`, FAIL. (It is, from Task 8.) This step mainly guards the wiring edit below.

- [ ] **Step 3: Apply redaction to `final_json`**

In `agent.rs::run_tools_concurrent`, the closure currently computes
`let final_json = post.replacement.unwrap_or(output_json);` (around `agent.rs:617`).
Replace that line so redaction is the **final** transform after the PostToolUse hook:

```rust
                        let final_json = post.replacement.unwrap_or(output_json);
                        let final_json = if redact_output {
                            crate::redaction::redact(&final_json, &secrets)
                        } else {
                            final_json
                        };
```

To make `redact_output` and `secrets` available inside the closure, compute them
once before the `calls.iter().map(...)` and capture by reference:

```rust
    let redact_output = interceptors.ctx.redact_output();
    let secrets = crate::redaction::SecretSet::from_env_and_extra(interceptors.ctx.extra_secrets());
```

Add `let redact_output = &redact_output; let secrets = &secrets;` shadowing for
capture if the closure is `move`, mirroring the existing `denied_events` capture
pattern.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (whole core suite, including the new wiring test).

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/agent.rs
git commit -m "feat(core): SMA-414 redact tool output as the final post-tool transform"
```

---

## Task 10: public exports (core + facade)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Modify: `crates/paigasus-helikon/src/lib.rs`

- [ ] **Step 1: Re-export new core types**

In `crates/paigasus-helikon-core/src/lib.rs`, add to the existing `pub use permission::{…}` line (or add one) — every re-export needs a `///` doc or `-D warnings` fails the docs job:

```rust
pub use permission::{GuardAction, GuardRule};
```

Confirm `DenyRule`, `PermissionDecision`, etc. remain exported. The `command_match` and `redaction` modules are already `pub mod`, so their public items are reachable as `paigasus_helikon_core::command_match::…` / `::redaction::…`.

- [ ] **Step 2: Re-export through the facade**

In `crates/paigasus-helikon/src/lib.rs`, find the block that re-exports core permission types and add `GuardRule`, `GuardAction` with a doc comment, e.g.:

```rust
/// Pre-mode safety guard rule and its action (re-exported from core).
pub use paigasus_helikon_core::{GuardAction, GuardRule};
```

(Match the existing facade re-export style — if core types are surfaced via a glob or a curated list, follow that pattern.)

- [ ] **Step 3: Verify docs build clean**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core -p paigasus-helikon --all-features --no-deps`
Expected: builds with no warnings.

- [ ] **Step 4: fmt, clippy, commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon/src/lib.rs
git commit -m "feat(facade): SMA-414 export GuardRule and GuardAction"
```

---

## Task 11: `BashTool` operator-aware deny/allow + integration tests

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/bash.rs`
- Modify: `crates/paigasus-helikon-tools/tests/bash.rs`

- [ ] **Step 1: Write the failing unit test**

In `bash.rs`, add a test module:

```rust
#[cfg(test)]
mod policy_tests {
    use super::*;

    fn tool(deny: &[&str], allow: Option<&[&str]>) -> BashTool<()> {
        // A no-op backend is fine; check_command_allowed never runs it.
        let backend = std::sync::Arc::new(crate::exec::HostBackend::builder().build());
        let mut b = BashTool::builder(backend).deny_commands(deny.iter().copied());
        if let Some(a) = allow {
            b = b.allow_commands(a.iter().copied());
        }
        b.build()
    }

    #[test]
    fn deny_matches_any_subcommand() {
        let t = tool(&["rm"], None);
        assert!(t.check_command_allowed("echo ok && rm -rf .").is_err());
        assert!(t.check_command_allowed("nice rm -rf .").is_err()); // wrapper-stripped
        assert!(t.check_command_allowed("echo ok && ls").is_ok());
    }

    #[test]
    fn allow_requires_all_subcommands() {
        let t = tool(&[], Some(&["git", "echo"]));
        assert!(t.check_command_allowed("echo hi && git status").is_ok());
        assert!(t.check_command_allowed("git status && rm -rf .").is_err());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-tools policy_tests`
Expected: FAIL — first-token matching still allows `echo ok && rm -rf .`.

- [ ] **Step 3: Rewrite `check_command_allowed` to use `command_match`**

In `bash.rs`, replace `check_command_allowed`:

```rust
    fn check_command_allowed(&self, command: &str) -> Result<(), ToolError> {
        let resolved = paigasus_helikon_core::command_match::resolve_all(command);
        let programs: Vec<&str> = resolved.iter().map(|c| c.program.as_str()).collect();

        // Deny if ANY sub-command program is denied.
        if let Some(bad) = programs.iter().find(|p| self.deny_commands.iter().any(|d| d == *p)) {
            return Err(ToolError::Denied {
                reason: format!("command `{bad}` is blocked by the deny list"),
            });
        }
        // With an allowlist, ALL sub-command programs must be allowed.
        if let Some(allow) = &self.allow_commands {
            if let Some(bad) = programs.iter().find(|p| !allow.iter().any(|a| a == *p)) {
                return Err(ToolError::Denied {
                    reason: format!("command `{bad}` is not in the allow list"),
                });
            }
        }
        Ok(())
    }
```

Update the `BashTool` rustdoc (and the builder docs for `deny_commands`/`allow_commands`) to state the compound composition rule: deny if any sub-command matches; allow only if every sub-command is allowed.

- [ ] **Step 4: Run the unit tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools policy_tests`
Expected: PASS.

- [ ] **Step 5: Add the redaction integration test**

In `crates/paigasus-helikon-tools/tests/bash.rs`, add a test that runs a real agent turn (or the existing harness used by other tests in that file — match the established pattern) executing `echo FOO_API_KEY=supersecretvalue` through `BashTool` and asserts the tool result seen by the model contains `***`, not `supersecretvalue`. Reuse the test scaffolding already present in `tests/bash.rs` / `tests/common/mod.rs`.

```rust
#[tokio::test]
async fn bash_output_secrets_are_redacted() {
    // Pattern: drive a single tool call through the runner with redaction on
    // (default), capture the ToolResult content, assert it is scrubbed.
    // Use the existing helper in tests/common/mod.rs to build the agent + run.
    let result = run_single_bash("echo FOO_API_KEY=supersecretvalue").await;
    assert!(result.contains("FOO_API_KEY=***"), "got: {result}");
    assert!(!result.contains("supersecretvalue"), "secret leaked: {result}");
}
```

> **Implementer note:** if `tests/common/mod.rs` lacks a `run_single_bash`
> helper, add one there modeled on the existing Bash integration tests in the
> file; do not invent a new harness.

- [ ] **Step 6: Run the integration test**

Run: `cargo test -p paigasus-helikon-tools bash_output_secrets_are_redacted`
Expected: PASS.

- [ ] **Step 7: fmt, clippy, commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/bash.rs crates/paigasus-helikon-tools/tests/bash.rs crates/paigasus-helikon-tools/tests/common/mod.rs
git commit -m "feat(tools): SMA-414 operator-aware Bash deny/allow and output redaction"
```

---

## Task 12: version bumps + CHANGELOGs (same-PR core + facade dance)

**Files:**
- Modify: `crates/paigasus-helikon-core/Cargo.toml`, `crates/paigasus-helikon-tools/Cargo.toml`, `crates/paigasus-helikon/Cargo.toml`
- Modify: root `Cargo.toml` (`[workspace.dependencies]` pins)
- Modify: `crates/paigasus-helikon-core/CHANGELOG.md`, `crates/paigasus-helikon-tools/CHANGELOG.md`, `crates/paigasus-helikon/CHANGELOG.md`

This is the CLAUDE.md "same-PR core bump + facade bump" requirement: tools consumes new core API (`command_match`, `GuardRule`), so `cargo publish --verify` builds the tools tarball against the **registry** core — bump core so the published core carries the new API, and bump the facade so it republishes with current sibling reqs.

- [ ] **Step 1: Read current versions**

Run: `grep -h '^version' crates/paigasus-helikon-core/Cargo.toml crates/paigasus-helikon-tools/Cargo.toml crates/paigasus-helikon/Cargo.toml`
Record the three current versions (call them `CORE`, `TOOLS`, `FACADE`).

- [ ] **Step 2: Bump the three crate versions (patch)**

In each crate's `Cargo.toml`, increment the patch component:
- `crates/paigasus-helikon-core/Cargo.toml`: `version = "<CORE+patch>"`
- `crates/paigasus-helikon-tools/Cargo.toml`: `version = "<TOOLS+patch>"`
- `crates/paigasus-helikon/Cargo.toml`: `version = "<FACADE+patch>"`

- [ ] **Step 3: Update the workspace dependency pins**

In root `Cargo.toml` `[workspace.dependencies]`, update the `version` of the
`paigasus-helikon-core`, `paigasus-helikon-tools`, and `paigasus-helikon` path
entries to the new numbers (keep the `path = ...` key).

- [ ] **Step 4: Add CHANGELOG entries**

Prepend an `## [Unreleased]`-style entry (match each file's existing format) to all three CHANGELOGs. The core and facade entries MUST include the behavior-change banner:

```markdown
### Added
- SMA-414: operator-aware `DenyRule::bash_command`, pre-mode `GuardRule`
  (`Deny`/`Ask`) destructive circuit breaker, and automatic tool-output secret
  redaction.

### Changed
- **Behavior change: the destructive-command floor is now on by default.** In
  `Default` mode with no policy and no approval handler, `rm -rf /`, `rm -rf ~`,
  and writes to protected system paths now resolve to deny (or prompt, with an
  `ApprovalHandler`) instead of run. Install an `ApprovalHandler`, or call
  `RunContext::without_default_guards()`, to restore the prior behavior.
```

The tools CHANGELOG entry notes the operator-aware deny/allow composition change.

- [ ] **Step 5: Verify the workspace builds and tarball-verifies**

Run:
```bash
cargo build --workspace --all-features
cargo package -p paigasus-helikon-core --allow-dirty --no-verify
```
Expected: builds clean. (Full `--verify` requires the new core on the registry; release-plz handles publish ordering on CI.)

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/Cargo.toml crates/paigasus-helikon-tools/Cargo.toml crates/paigasus-helikon/Cargo.toml Cargo.toml crates/paigasus-helikon-core/CHANGELOG.md crates/paigasus-helikon-tools/CHANGELOG.md crates/paigasus-helikon/CHANGELOG.md
git commit -m "chore(release): SMA-414 bump core, tools, facade for command-policy hardening"
```

---

## Task 13: documentation (book + README)

**Files:**
- Modify: `docs/book/src/concepts/permissions-guardrails-hooks.md`
- Modify: `crates/paigasus-helikon-tools/README.md`

- [ ] **Step 1: Update the permissions concept page**

Add sections to `docs/book/src/concepts/permissions-guardrails-hooks.md` covering:
- **Guard rules** — `GuardRule` (`Deny`/`Ask`), evaluated before mode, beats `Bypass`; the always-on destructive defaults; `RunContext::without_default_guards()`.
- **Operator-aware deny matching** — `DenyRule::bash_command`, compound splitting, `sudo`/`bash -c` coverage.
- **The device-node allowlist** — `> /dev/null` is allowed.
- **Secret redaction** — on by default; key-name + env-value scans; the length floor; `without_output_redaction()` / `with_extra_secrets`.
- **Scope limitations** — `bash -c` recursion depth, and the documented bypasses (`find -delete`, `xargs`, `eval "$VAR"`, `$(…)`, shell expansion).

- [ ] **Step 2: Update the tools README**

Add to `crates/paigasus-helikon-tools/README.md`: the operator-aware Bash deny/allow behavior, the compound composition rule (deny-any / allow-all), and the on-by-default output redaction.

- [ ] **Step 3: Verify the book builds clean**

Run: `mdbook build docs/book`
Expected: builds with no link-check errors (`[output.linkcheck] warning-policy = "error"`).

- [ ] **Step 4: Commit**

```bash
git add docs/book/src/concepts/permissions-guardrails-hooks.md crates/paigasus-helikon-tools/README.md
git commit -m "docs(book): SMA-414 document guard rules, deny matching, and redaction"
```

---

## Final verification (run before opening the PR)

Reproduce the CI gates locally (from CLAUDE.md):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
mdbook build docs/book
```

All must pass. Confirm the three acceptance criteria are demonstrably covered:
- **AC-1** — `bash_command_matches_any_subcommand_program` + tools `deny_matches_any_subcommand`.
- **AC-2** — `destructive_guard_denies_under_bypass_without_handler` / `_asks_..._with_handler` + the `/dev/null` allow test.
- **AC-3** — `redaction::tests` + tools `bash_output_secrets_are_redacted`.
