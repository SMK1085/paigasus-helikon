# SMA-543 tracing target syntax — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the seven `tracing::warn!` call sites in the OpenAI and LiteLLM chat translators emit on their declared `paigasus::…` target, and add a permanent guard so the `target =` / `parent =` slip cannot recur anywhere in the workspace.

**Architecture:** A new non-published workspace member `tests/workspace-lints` exposes a pure `scan(&str) -> Vec<Offense>` detector, unit-tested against every erroneous macro form that actually compiles. A repo-walking integration test runs it over `crates/` and `tests/` with anti-vacuity assertions. Separately, one capture-`Layer` unit test in `providers-openai` pins the *semantic* property that `target:` sets `metadata().target()`.

**Tech Stack:** Rust 2021, `tracing` 0.1, `tracing-subscriber` 0.3 (workspace-pinned), no new third-party dependencies.

**Spec:** `docs/superpowers/specs/2026-08-19-sma-543-tracing-target-design.md`

## Global Constraints

- MSRV is `1.94`; edition and all metadata inherit from `[workspace.package]`. Per-crate `Cargo.toml` sets only `name`, `description`, `version`, `publish`, and deps.
- Every new crate opts into workspace lints with a `[lints] workspace = true` block. That enables `missing_docs = "warn"`, and CI runs `-D warnings`: **every public item needs a `///` doc comment.**
- `scripts/check-doc-coverage.sh` builds its crate list from `cargo metadata --no-deps` and excludes only `paigasus-helikon-cli`, so the new member counts toward the 80% workspace doc-coverage gate. Fully documenting its small public surface keeps this a non-issue.
- No new third-party dependency. `regex` is not a workspace dep and must not become one for this.
- Both provider crates change in the same PR (SMA-451 design decision D6 — see spec §2).
- Commit messages: `<type>(<scope>): SMA-543 <lowercase message>`. Allowed scopes come from `.versionrc`'s `scopeRegex`; use `providers`, `workspace`, `spec`, or `plan`. **`workspace-lints` is not an allowed scope.**
- Run `cargo fmt --all` before every commit — the `pre-commit` hook is a deliberate no-op, so nothing catches formatting until `pre-push`.
- Work in the worktree at `/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/9b05f5a3-b575-4168-b547-dcac57dbf4c5/scratchpad/wt-sma-543`. All paths below are relative to it.

---

### Task 1: The `scan` detector and its unit tests

Creates the new workspace member and the pure detector. No repo walking yet — this task's deliverable is a function that provably fires on bad input and stays silent on good input.

**Files:**
- Create: `tests/workspace-lints/Cargo.toml`
- Create: `tests/workspace-lints/src/lib.rs`
- Modify: `Cargo.toml:3` (workspace `members` list)
- Modify: `release-plz.toml` (append a `[[package]]` block)

**Interfaces:**
- Produces: `paigasus_helikon_workspace_lints::scan(src: &str) -> Vec<Offense>` and `paigasus_helikon_workspace_lints::Offense { line: usize, macro_name: String, keyword: String }`. Task 2 consumes both.

- [ ] **Step 1: Register the member in the workspace**

Edit `Cargo.toml` line 3:

```toml
members  = ["crates/*", "tests/runtime-http-conformance", "tests/workspace-lints"]
```

- [ ] **Step 2: Keep release-plz away from it**

Append to `release-plz.toml`, mirroring the two existing blocks:

```toml
# Internal workspace-wide source lints (SMA-543). Never published, so it
# carries the same publish=false / release=false pair as the members above.
[[package]]
name = "paigasus-helikon-workspace-lints"
publish = false
release = false
```

- [ ] **Step 3: Create the manifest**

Create `tests/workspace-lints/Cargo.toml`:

```toml
[package]
name        = "paigasus-helikon-workspace-lints"
description = "Internal: workspace-wide source lints for the Helikon repo."
version     = "0.0.0"
publish     = false
edition.workspace = true
rust-version.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
keywords.workspace = true
categories.workspace = true

[dependencies]

[lints]
workspace = true
```

- [ ] **Step 4: Write the failing unit tests**

Create `tests/workspace-lints/src/lib.rs` containing **only** the test module for now, so the tests fail to compile against a missing `scan`:

```rust
//! Workspace-wide source lints for the Helikon repo.
//!
//! Internal, never published. See
//! `docs/superpowers/specs/2026-08-19-sma-543-tracing-target-design.md`.

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(usize, String, String)> {
        scan(src)
            .into_iter()
            .map(|o| (o.line, o.macro_name, o.keyword))
            .collect()
    }

    /// Every form that actually compiles against `tracing` 0.1 and silently
    /// records a field instead of setting the metadata target. Verified with
    /// rustc; see spec §4.4 for the compile matrix.
    #[test]
    fn flags_every_compiling_bad_form() {
        let cases: &[(&str, &str, &str)] = &[
            (r#"tracing::warn!(target = "x", "m");"#, "warn", "target"),
            (r#"warn!(target="x", "m");"#, "warn", "target"),
            (r#"tracing::warn!(parent: None, target = "x", "m");"#, "warn", "target"),
            (r#"tracing::info_span!("nm", target = "x");"#, "info_span", "target"),
            (r#"tracing::event!(Level::WARN, target = "x", "m");"#, "event", "target"),
            (r#"tracing::info!(parent = p, "m");"#, "info", "parent"),
        ];
        for (src, mac, kw) in cases {
            let got = kinds(src);
            assert_eq!(
                got,
                vec![(1, (*mac).to_owned(), (*kw).to_owned())],
                "expected one offense for `{src}`"
            );
        }
    }

    /// The real shape of the SMA-543 bug: the keyword is on its own line.
    #[test]
    fn flags_the_multiline_shape_and_reports_the_keyword_line() {
        let src = "fn f() {\n    tracing::warn!(\n        target = \"paigasus::openai::translate\",\n        \"unknown Item variant; skipping\"\n    );\n}\n";
        assert_eq!(kinds(src), vec![(3, "warn".to_owned(), "target".to_owned())]);
    }

    /// `cargo test --workspace --all-features` also runs on windows-latest.
    #[test]
    fn line_numbers_survive_crlf() {
        let src = "fn f() {\r\n    tracing::warn!(\r\n        target = \"x\",\r\n        \"m\"\r\n    );\r\n}\r\n";
        assert_eq!(kinds(src), vec![(3, "warn".to_owned(), "target".to_owned())]);
    }

    #[test]
    fn accepts_correct_and_unrelated_forms() {
        let cases = [
            r#"tracing::warn!(target: "x", "m");"#,
            r#"tracing::info_span!(parent: parent, "nm");"#,
            r#"tracing::info!(count = 1, "m");"#,
            r#"let target = "x";"#,
            r#"if a == b { let parent = 1; }"#,
            r#"foo!(target = "x");"#,
            r#"Thing { target: "x" }"#,
            r#"fn f<'a>(x: &'a str) { tracing::warn!(target: "t", "m"); }"#,
            r#"tracing::warn!(target: "t", other = compute(a, b), "m");"#,
        ];
        for src in cases {
            assert_eq!(kinds(src), vec![], "false positive on `{src}`");
        }
    }

    /// The guard scans its own source. Because the lexer blanks comments and
    /// literals, the bad forms in this very file are invisible to it — which
    /// is why there is deliberately no path-based self-exclusion (spec §4.5).
    #[test]
    fn ignores_comments_and_literals() {
        let cases = [
            r#"// tracing::warn!(target = "x", "m");"#,
            r#"/// tracing::warn!(target = "x", "m");"#,
            r#"/* tracing::warn!(target = "x", "m"); */"#,
            r#"/* outer /* nested */ tracing::warn!(target = "x"); */"#,
            r#"let s = "tracing::warn!(target = \"x\")";"#,
            "let s = r#\"tracing::warn!(target = \"x\")\"#;",
            r#"let s = b"warn!(target = 1)";"#,
            r#"let c = '"'; let d = '\''; "#,
        ];
        for src in cases {
            assert_eq!(kinds(src), vec![], "false positive on `{src}`");
        }
    }

    #[test]
    fn reports_every_offense_in_a_file() {
        let src = "tracing::warn!(target = \"a\", \"m\");\ntracing::warn!(target: \"ok\", \"m\");\ntracing::error!(target = \"b\", \"m\");\n";
        assert_eq!(
            kinds(src),
            vec![
                (1, "warn".to_owned(), "target".to_owned()),
                (3, "error".to_owned(), "target".to_owned()),
            ]
        );
    }

    /// A string containing an unbalanced paren must not desynchronise the
    /// argument walk.
    #[test]
    fn unbalanced_paren_inside_a_literal_does_not_desync() {
        let src = "tracing::warn!(target: \"a)b\", \"m\");\ntracing::warn!(target = \"c\", \"m\");\n";
        assert_eq!(kinds(src), vec![(2, "warn".to_owned(), "target".to_owned())]);
    }
}
```

- [ ] **Step 5: Run the tests to verify they fail**

```bash
cargo test -p paigasus-helikon-workspace-lints
```

Expected: FAIL — `cannot find function `scan` in this scope` and `cannot find type `Offense``.

- [ ] **Step 6: Implement the detector**

Insert this **above** the `#[cfg(test)] mod tests` block in `tests/workspace-lints/src/lib.rs`:

```rust
/// One `target =` / `parent =` argument found inside a `tracing` macro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offense {
    /// 1-based line number of the offending keyword.
    pub line: usize,
    /// Macro it appeared in, unqualified (e.g. `warn` for `tracing::warn!`).
    pub macro_name: String,
    /// Which keyword was misused: `target` or `parent`.
    pub keyword: String,
}

/// Macros where `target:` / `parent:` are macro syntax, not field names.
const TRACING_MACROS: &[&str] = &[
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    "event",
    "span",
    "trace_span",
    "debug_span",
    "info_span",
    "warn_span",
    "error_span",
];

/// Keywords that must be introduced with `:` and never with `=`.
const KEYWORDS: &[&str] = &["target", "parent"];

/// Scan Rust source for `tracing` macro arguments written `target = …` or
/// `parent = …`, which the macros silently treat as ordinary fields.
///
/// Every top-level argument is inspected, not just the first: for the span and
/// event macros the correct syntax puts `target:` *before* the level or span
/// name, so the erroneous form is only reachable in a later position.
///
/// Comments and literals are blanked before scanning, so a macro written out
/// inside a comment, doc example or string is never flagged.
pub fn scan(src: &str) -> Vec<Offense> {
    let masked = mask_trivia(src);
    let b = &masked[..];
    let mut offenses = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'!' {
            i += 1;
            continue;
        }
        let Some(name) = ident_before(b, i) else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if b.get(j) != Some(&b'(') {
            i += 1;
            continue;
        }
        if TRACING_MACROS.contains(&name) {
            collect_args(b, j, name, &mut offenses);
        }
        i = j + 1;
    }
    offenses
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The identifier immediately preceding `at`, if any. Stops at `:`, so a
/// qualified `tracing::warn!` yields `warn`.
fn ident_before(b: &[u8], at: usize) -> Option<&str> {
    let mut s = at;
    while s > 0 && is_ident_byte(b[s - 1]) {
        s -= 1;
    }
    if s == at {
        return None;
    }
    std::str::from_utf8(&b[s..at]).ok()
}

fn blank(out: &mut [u8], from: usize, to: usize) {
    for byte in &mut out[from..to] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

/// Replace every byte inside a comment or literal with a space, preserving
/// length, byte offsets and newlines so offsets still map onto the original.
fn mask_trivia(src: &str) -> Vec<u8> {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let start = i;
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                blank(&mut out, start, i);
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let start = i;
                let mut depth = 1usize;
                i += 2;
                while i < b.len() && depth > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                blank(&mut out, start, i);
            }
            b'r' | b'b' => match raw_or_byte_string_end(b, i) {
                Some(end) => {
                    blank(&mut out, i, end);
                    i = end;
                }
                None => i += 1,
            },
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                    } else if b[i] == b'"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                blank(&mut out, start, i);
            }
            b'\'' => match char_literal_end(b, i) {
                Some(end) => {
                    blank(&mut out, i, end);
                    i = end;
                }
                // A lifetime, not a literal — leave it alone.
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    out
}

/// End (exclusive) of a raw or byte string starting at `i`, if one does.
fn raw_or_byte_string_end(b: &[u8], i: usize) -> Option<usize> {
    // `bar` must not read as a byte string starting at its `r`.
    if i > 0 && is_ident_byte(b[i - 1]) {
        return None;
    }
    let mut j = i;
    if b[j] == b'b' {
        j += 1;
    }
    let raw = b.get(j) == Some(&b'r');
    if raw {
        j += 1;
        let hashes_start = j;
        while b.get(j) == Some(&b'#') {
            j += 1;
        }
        let hashes = j - hashes_start;
        if b.get(j) != Some(&b'"') {
            return None;
        }
        j += 1;
        while j < b.len() {
            if b[j] == b'"' {
                let mut k = j + 1;
                let mut seen = 0;
                while seen < hashes && b.get(k) == Some(&b'#') {
                    k += 1;
                    seen += 1;
                }
                if seen == hashes {
                    return Some(k);
                }
            }
            j += 1;
        }
        return Some(b.len());
    }
    if b[i] != b'b' || b.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    while j < b.len() {
        if b[j] == b'\\' {
            j += 2;
        } else if b[j] == b'"' {
            return Some(j + 1);
        } else {
            j += 1;
        }
    }
    Some(b.len())
}

/// End (exclusive) of a char literal starting at `i`, or `None` for a lifetime.
fn char_literal_end(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i + 1) == Some(&b'\\') {
        let mut j = i + 2;
        while j < b.len() && b[j] != b'\'' {
            j += 1;
        }
        return if j < b.len() { Some(j + 1) } else { None };
    }
    let mut j = i + 1;
    if j >= b.len() {
        return None;
    }
    j += 1;
    // Consume UTF-8 continuation bytes of a multi-byte char.
    while j < b.len() && b[j] & 0b1100_0000 == 0b1000_0000 {
        j += 1;
    }
    if b.get(j) == Some(&b'\'') {
        Some(j + 1)
    } else {
        None
    }
}

fn starts_with_ident(b: &[u8], at: usize, word: &str) -> bool {
    let w = word.as_bytes();
    b.len() >= at + w.len()
        && &b[at..at + w.len()] == w
        && !b.get(at + w.len()).is_some_and(|&c| is_ident_byte(c))
}

fn line_of(b: &[u8], at: usize) -> usize {
    b[..at].iter().filter(|&&c| c == b'\n').count() + 1
}

/// Walk one macro invocation's argument list, flagging any top-level argument
/// that opens with `target =` or `parent =`.
fn collect_args(b: &[u8], open: usize, macro_name: &str, out: &mut Vec<Offense>) {
    let mut k = open + 1;
    let mut depth = 0usize;
    let mut at_arg_start = true;
    while k < b.len() {
        let c = b[k];
        if c == b'(' || c == b'[' || c == b'{' {
            depth += 1;
            at_arg_start = false;
        } else if c == b')' || c == b']' || c == b'}' {
            if depth == 0 {
                return;
            }
            depth -= 1;
            at_arg_start = false;
        } else if c == b',' && depth == 0 {
            at_arg_start = true;
        } else if c.is_ascii_whitespace() {
            // Whitespace never ends an argument-start position.
        } else {
            if at_arg_start && depth == 0 {
                if let Some(kw) = KEYWORDS.iter().find(|kw| starts_with_ident(b, k, kw)) {
                    let mut m = k + kw.len();
                    while m < b.len() && b[m].is_ascii_whitespace() {
                        m += 1;
                    }
                    if b.get(m) == Some(&b'=') && b.get(m + 1) != Some(&b'=') {
                        out.push(Offense {
                            line: line_of(b, k),
                            macro_name: macro_name.to_owned(),
                            keyword: (*kw).to_owned(),
                        });
                    }
                }
            }
            at_arg_start = false;
        }
        k += 1;
    }
}
```

- [ ] **Step 7: Run the tests to verify they pass**

```bash
cargo fmt --all
cargo test -p paigasus-helikon-workspace-lints
```

Expected: PASS, 7 tests. If `flags_every_compiling_bad_form` or
`accepts_correct_and_unrelated_forms` fails, fix `collect_args` / `mask_trivia`
— do **not** weaken the test table.

- [ ] **Step 8: Verify lints and docs are clean**

```bash
cargo clippy -p paigasus-helikon-workspace-lints --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-workspace-lints --no-deps
```

Expected: both clean. `missing_docs` is on via `[lints] workspace = true`, so any
undocumented public item fails here.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock release-plz.toml tests/workspace-lints
git commit -m "test(workspace): SMA-543 add a tracing target syntax detector"
```

---

### Task 2: The repo walk, and the seven-site fix

The repo-level test is written first and observed failing with all seven real sites named, then the fix turns it green. Both halves land in one commit so the branch never carries a red tree — and so both provider crates change together, as D6 requires.

**Files:**
- Create: `tests/workspace-lints/tests/tracing_target_syntax.rs`
- Modify: `crates/paigasus-helikon-providers-openai/src/translate/request.rs` (lines 83, 205, 211, 345)
- Modify: `crates/paigasus-helikon-providers-litellm/src/translate/request.rs` (lines 85, 207, 213)

**Interfaces:**
- Consumes: `paigasus_helikon_workspace_lints::{scan, Offense}` from Task 1.

- [ ] **Step 1: Write the repo-walking test**

Create `tests/workspace-lints/tests/tracing_target_syntax.rs`:

```rust
//! No `tracing` macro anywhere in the workspace may pass `target` or `parent`
//! with `=` instead of `:` — the `=` form silently records an ordinary field
//! and leaves the event on its module-path target (SMA-543).

use std::path::{Path, PathBuf};

use paigasus_helikon_workspace_lints::scan;

/// Repo root, derived from this crate's manifest directory rather than the
/// process CWD so it survives the member being moved.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root must resolve from CARGO_MANIFEST_DIR")
}

/// Collect `.rs` files under `dir`, skipping build output.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if path.is_dir() {
            if name == "target" || name == ".git" {
                continue;
            }
            collect_rs(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_tracing_macro_passes_target_or_parent_with_equals() {
    let root = repo_root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "resolved repo root {root:?} has no Cargo.toml"
    );

    // Only the two directories that hold workspace members. Deliberately NOT
    // the repo root: `.claude/worktrees/` can hold full checkouts of other
    // branches, and scanning those would make this test's verdict depend on
    // which unrelated worktrees a developer happens to have.
    let mut files = Vec::new();
    for sub in ["crates", "tests"] {
        let dir = root.join(sub);
        assert!(dir.is_dir(), "expected workspace directory {dir:?}");
        collect_rs(&dir, &mut files);
    }

    // Anti-vacuity: a walk that finds nothing must fail, not pass. Without
    // this, a wrong root or a moved directory reports identically to a clean
    // workspace. Same reasoning as the vacuous-pass guard in
    // `crates/paigasus-helikon/tests/openai_litellm_message_parity.rs`.
    assert!(
        files.len() >= 300,
        "scanned only {} .rs files — the walk is not reaching the workspace",
        files.len()
    );
    for required in [
        "crates/paigasus-helikon-providers-openai/src/translate/request.rs",
        "crates/paigasus-helikon-providers-litellm/src/translate/request.rs",
        // Proves the non-`crates/` root is live: this member sits outside
        // `crates/` and an earlier draft of the walk would have missed it.
        "tests/runtime-http-conformance/src/lib.rs",
    ] {
        assert!(
            files.iter().any(|f| f.ends_with(required)),
            "{required} was not among the {} files scanned",
            files.len()
        );
    }

    let mut offenses = Vec::new();
    for file in &files {
        let src = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let rel = file.strip_prefix(&root).unwrap_or(file);
        for o in scan(&src) {
            offenses.push(format!(
                "{}:{} — {}! passes `{} =`; it must be `{}:`",
                rel.display(),
                o.line,
                o.macro_name,
                o.keyword,
                o.keyword
            ));
        }
    }

    assert!(
        offenses.is_empty(),
        "{} tracing macro call site(s) use `=` where the macro requires `:`:\n{}",
        offenses.len(),
        offenses.join("\n")
    );
}
```

- [ ] **Step 2: Run it and confirm it fails, naming all seven sites**

```bash
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_syntax
```

Expected: FAIL listing exactly seven lines — openai `request.rs` 83, 205, 211,
345 and litellm `request.rs` 85, 207, 213. If the count is not seven, stop and
reconcile before changing any provider source: either the detector is wrong or
the tree has moved since the spec was written.

- [ ] **Step 3: Fix all seven sites**

In both files the offending token is identical. Replace `target = ` with
`target: ` at each of the seven locations — and nowhere else:

```bash
sed -i '' 's/^\([[:space:]]*\)target = "paigasus::openai::translate",$/\1target: "paigasus::openai::translate",/' \
  crates/paigasus-helikon-providers-openai/src/translate/request.rs
sed -i '' 's/^\([[:space:]]*\)target = "paigasus::litellm::translate",$/\1target: "paigasus::litellm::translate",/' \
  crates/paigasus-helikon-providers-litellm/src/translate/request.rs
```

`[[:space:]]`, not `\s`: this is BSD sed on macOS, where `\s` matches nothing
**and still exits 0** — the substitution silently does nothing and the run looks
like it worked. Step 4 is what catches that, so do not skip it.

- [ ] **Step 4: Confirm exactly seven lines changed, and only those**

```bash
git diff --numstat crates/paigasus-helikon-providers-openai/src/translate/request.rs \
                   crates/paigasus-helikon-providers-litellm/src/translate/request.rs
git diff -U0 crates/paigasus-helikon-providers-openai/src/translate/request.rs \
             crates/paigasus-helikon-providers-litellm/src/translate/request.rs
```

Expected: `4 4` for openai and `3 3` for litellm, and every hunk a lone
`target =` → `target:` substitution. Anything else means the `sed` over-matched
— revert and redo.

- [ ] **Step 5: Run the guard again to verify it passes**

```bash
cargo test -p paigasus-helikon-workspace-lints
```

Expected: PASS, 8 tests (7 unit + 1 repo walk).

- [ ] **Step 6: Confirm the two translators still match**

```bash
diff crates/paigasus-helikon-providers-openai/src/translate/request.rs \
     crates/paigasus-helikon-providers-litellm/src/translate/request.rs
```

Expected — and nothing else (spec §5.2):
- the module doc block (openai 1–6 vs litellm 1–8);
- four target strings, `paigasus::openai::translate` vs
  `paigasus::litellm::translate`, at openai 83/172/205/211 ↔ litellm
  85/174/207/213 (line numbers are unchanged by the fix, which substitutes in
  place);
- openai-only `to_responses_input` (245–352) and `mod responses_tests` (564–630);
- litellm-only `plain_text_user_turn_emits_string_content` (457–463) and
  `tool_call_then_result_round_trips` (465–486).

The **ticket's** stated expectation omits the last item and is wrong; the list
above is the baseline.

- [ ] **Step 7: Confirm the provider suites still pass**

```bash
cargo test -p paigasus-helikon-providers-openai -p paigasus-helikon-providers-litellm
```

Expected: PASS. No assertion depends on the target, so this is a
no-behaviour-change check.

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
git add tests/workspace-lints/tests/tracing_target_syntax.rs \
        crates/paigasus-helikon-providers-openai/src/translate/request.rs \
        crates/paigasus-helikon-providers-litellm/src/translate/request.rs
git commit -m "fix(providers): SMA-543 route chat-translator warnings to their declared tracing target"
```

---

### Task 3: Behavioural test — `target:` really sets the metadata target

The static guard proves syntax. This proves meaning, and makes the spec §1.1 probe reproducible inside the repo. It must be an **inline unit test**: `to_chat_messages` is `pub(crate)`, so an integration test under `tests/` cannot call it.

It goes in a new module at the **end** of the file, after `responses_tests` — not inside `chat_tests`, which is the block ported verbatim into the LiteLLM crate. Appending here follows the file's existing shape (both copies already carry their own tail-end additions) and leaves the D6 shared region untouched.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/Cargo.toml` (`[dev-dependencies]`)
- Modify: `crates/paigasus-helikon-providers-openai/src/translate/request.rs` (append a module at end of file)

**Interfaces:**
- Consumes: `to_chat_messages` and `ContentPart`/`Item`/`MediaSource`, already in scope via `use super::*`.

- [ ] **Step 1: Add the dev-dependency**

In `crates/paigasus-helikon-providers-openai/Cargo.toml`, add to `[dev-dependencies]`:

```toml
tracing-subscriber = { workspace = true }
```

The workspace already pins it as `{ version = "0.3", features = ["env-filter", "fmt"] }` with default features on, so `registry` is available.

- [ ] **Step 2: Write the failing test**

Append to the very end of `crates/paigasus-helikon-providers-openai/src/translate/request.rs`:

```rust
/// The static guard in `tests/workspace-lints` proves these call sites use
/// `target:` rather than `target =`. This proves what that *buys*: the event
/// actually lands on the declared target, so `RUST_LOG` /
/// `EnvFilter` selectors naming it work (SMA-543).
#[cfg(test)]
mod tracing_target_tests {
    use std::sync::{Arc, Mutex};

    use tracing::subscriber::with_default;
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;

    use super::*;

    /// Records the metadata target of every event it sees.
    #[derive(Clone, Default)]
    struct TargetCapture(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber + for<'l> LookupSpan<'l>> Layer<S> for TargetCapture {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            self.0
                .lock()
                .expect("capture mutex")
                .push(event.metadata().target().to_owned());
        }
    }

    #[test]
    fn dropped_multimodal_part_warns_on_the_declared_target() {
        let capture = TargetCapture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());

        with_default(subscriber, || {
            // Same input as `assistant_image_content_part_is_dropped_with_warning`:
            // an Image part on an AssistantMessage is not representable in the
            // Chat assistant role, so `assistant_message` warns and drops it.
            let items = vec![Item::AssistantMessage {
                content: vec![ContentPart::Image {
                    source: MediaSource::Url {
                        url: "x".to_owned(),
                    },
                }],
                agent: None,
            }];
            let _ = to_chat_messages(&items);
        });

        let targets = capture.0.lock().expect("capture mutex").clone();
        assert_eq!(
            targets,
            vec!["paigasus::openai::translate".to_owned()],
            "the warn must land on its declared target, not on the module path"
        );
    }
}
```

- [ ] **Step 3: Verify the test genuinely discriminates**

Temporarily revert line 205 to the broken form and confirm the test fails:

```bash
sed -i '' '205s/target: /target = /' crates/paigasus-helikon-providers-openai/src/translate/request.rs
cargo test -p paigasus-helikon-providers-openai tracing_target_tests
```

Expected: FAIL, with the observed target being the module path
(`paigasus_helikon_providers_openai::translate::request`) rather than
`paigasus::openai::translate`. A test that passes in both directions is worthless
— if it passes here, it is not testing what it claims.

Then restore:

```bash
sed -i '' '205s/target = /target: /' crates/paigasus-helikon-providers-openai/src/translate/request.rs
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p paigasus-helikon-providers-openai tracing_target_tests
```

Expected: PASS, 1 test.

- [ ] **Step 5: Confirm the guard still sees a clean tree**

```bash
cargo test -p paigasus-helikon-workspace-lints
```

Expected: PASS. Step 3's temporary revert must have been undone; if the guard
reports a site here, the restore did not take.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-openai/Cargo.toml \
        crates/paigasus-helikon-providers-openai/src/translate/request.rs \
        Cargo.lock
git commit -m "test(providers): SMA-543 assert chat-translator warns emit on their declared target"
```

---

### Task 4: Full gate run

**Files:** none modified unless a gate fails.

- [ ] **Step 1: Run every CI gate locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: all four clean. Run them from the worktree, not the main checkout —
the bedrock suite fails on this machine when run from
`~/dev/paigasus/paigasus-helikon` for reasons tied to the checkout path, and a
worktree under the scratchpad gives a true signal.

- [ ] **Step 2: Verify doc coverage still clears the gate**

```bash
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
```

Expected: PASS. The new member is in the denominator (the script builds its list
from `cargo metadata --no-deps`), and its public surface is fully documented.

- [ ] **Step 3: Verify the commit messages pass convco**

```bash
convco check "$(git merge-base origin/main HEAD)..HEAD"
```

Expected: PASS. Use the merge-base, never `origin/main`'s tip — `convco check
A..B` silently walks the entire history when `A` is not an ancestor of `B`, and
three commits early in this repo predate the scope allowlist.

- [ ] **Step 4: Report**

State the result of each gate, with the actual command output for anything that
failed. Do not report completion on an unrun gate.
