# SMA-557 — `paigasus::*` tracing target namespace: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Document the `paigasus::*` tracing target namespace, its prefix-matching
semantics and its stability contract in the public mdBook, and add a guard that
keeps the documented component list in sync with source.

**Architecture:** Four tasks. Task 1 extends the existing internal lexer
(`mask_trivia`) to also report string-literal spans — the bytes it currently
blanks are exactly the bytes a target scan must read. Task 2 adds `scan_targets`
on top of it. Task 3 writes the book section, whose component table sits inside
HTML markers. Task 4 wires the two together into a workspace-walking test with
two-directional mutation checks.

**Tech Stack:** Rust (edition/MSRV inherited from `[workspace.package]`), `std`
only — `tests/workspace-lints` has an empty `[dependencies]` and must keep it.
mdBook with `mdbook-linkcheck`.

**Design source of truth:** `docs/superpowers/specs/2026-08-20-sma-557-tracing-target-namespace-design.md`.
Read it before starting; the section references below (§1.2, §4.2, …) point into it.

## Global Constraints

- **No new dependencies.** `tests/workspace-lints/Cargo.toml` has an empty
  `[dependencies]` table. Keep it empty — `std` only, no `regex`, no `once_cell`.
- **`missing_docs` is enforced** (`[lints] workspace = true`). Every new `pub`
  item needs a `///` doc comment or the required `docs` CI job fails.
- **No published crate may be touched.** `tests/workspace-lints` is
  `publish = false`; `docs/` is not packaged. If a task makes you want to edit
  anything under `crates/*/src`, stop — that is out of scope (spec §6).
- **Commit scopes must come from `.versionrc:18`.** Use `test(lints)` for
  `tests/workspace-lints` and `docs(docs)` for the book. **There is no `book`
  scope** — do not invent one.
- **Commit subjects start lowercase** after the `SMA-557 ` token.
- **Run `cargo fmt --all` before every commit.** The `pre-commit` hook is a
  deliberate no-op; `pre-push` runs fmt + clippy and will reject otherwise.
- **`target:` vs `target =`:** this repo's `tracing` call sites use `target:`
  (SMA-543). Any test fixture you write that means a real target must use the
  colon form.
- Every command below runs from the repo root
  (`.claude/worktrees/sma-557-tracing-targets` in this session).

## Prerequisite — already done

Spec §6 requires the follow-up issue to exist before the book page is written,
because Task 3's prose refers to it. **It is filed: SMA-568, "Decide whether core
and the runtime crates should adopt `paigasus::*` tracing targets"**, in the
`Paigasus Helikon` project, related to SMA-557.

Task 3 refers to it in **prose only, with no URL** — `linear.app` appears nowhere
in `docs/book/` today, the workspace is private, and `docs/book/book.toml:20` sets
`follow-web-links = false`, so linkcheck could not catch a wrong link anyway. Do
not add the URL.

---

### Task 1: `mask_trivia` reports string-literal spans

`mask_trivia` blanks comments *and* string literals, and returns only the line-comment
ranges. `scan_targets` (Task 2) needs to read literal **contents**, which the masked
buffer no longer holds. Rather than add a second lexer — explicitly warned against at
`tests/workspace-lints/src/lib.rs:206-208` — extend the one that already exists.

**Files:**
- Modify: `tests/workspace-lints/src/lib.rs:308-377` (`mask_trivia`), `:121` (its only caller)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `struct Masked { buf: Vec<u8>, line_comments: Vec<(usize, usize)>, string_literals: Vec<(usize, usize)> }`
  (private to the crate) and `fn mask_trivia(src: &str) -> Masked`. Task 2 consumes
  `Masked.buf` and `Masked.string_literals`.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block at the bottom of `tests/workspace-lints/src/lib.rs`:

```rust
    /// `mask_trivia` must report the byte span of every string literal, so a
    /// later scan can read the literal's *contents* out of the original source
    /// (the masked buffer has blanked them). Char literals are deliberately
    /// excluded — they can never hold a tracing target.
    #[test]
    fn mask_trivia_reports_string_literal_spans() {
        let src = "let a = \"one\"; let b = 'x'; let c = r#\"two\"#;";
        let masked = mask_trivia(src);
        let texts: Vec<&str> = masked
            .string_literals
            .iter()
            .map(|&(s, e)| &src[s..e])
            .collect();
        assert_eq!(texts, vec!["\"one\"", "r#\"two\"#"]);
    }

    /// The existing line-comment reporting must survive the signature change.
    #[test]
    fn mask_trivia_still_reports_line_comments() {
        let src = "// note\nlet a = 1;\n";
        let masked = mask_trivia(src);
        assert_eq!(masked.line_comments.len(), 1);
        let (s, e) = masked.line_comments[0];
        assert_eq!(&src[s..e], "// note");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-workspace-lints --lib`
Expected: FAIL to **compile** — `mask_trivia` returns a tuple, so `masked.string_literals`
is `no field ... on type (Vec<u8>, Vec<(usize, usize)>)`. A compile failure is the
correct red here.

- [ ] **Step 3: Introduce the struct**

Insert immediately **above** the `fn mask_trivia` doc comment (currently near line 290):

```rust
/// What [`mask_trivia`] found while masking one file's source.
struct Masked {
    /// Source bytes with comments and literals blanked to spaces, so a scan
    /// over it sees only genuine code.
    buf: Vec<u8>,
    /// Byte ranges of genuine `//` line comments (including `///` and `//!`).
    /// [`collect_allow_marker_lines`] uses these to tell a real
    /// `// allow(tracing-target-syntax)` from that text inside a string.
    line_comments: Vec<(usize, usize)>,
    /// Byte ranges of string literals — plain, raw, byte and C strings —
    /// delimiters included. Char literals are excluded: they cannot hold a
    /// tracing target, and a lifetime (`'a`) is not a literal at all.
    ///
    /// `scan_targets` needs these because `buf` has blanked the very bytes a
    /// target string is made of. Reporting the span instead of un-blanking
    /// keeps one lexer authoritative over what counts as code — re-deriving
    /// literal boundaries in a second scanner is what the note at
    /// `collect_allow_marker_lines` warns against.
    string_literals: Vec<(usize, usize)>,
}
```

- [ ] **Step 4: Change the signature and record the spans**

In `fn mask_trivia`, change the signature line:

```rust
fn mask_trivia(src: &str) -> Masked {
```

Change the two local declarations near the top of the body:

```rust
    let mut line_comments = Vec::new();
    let mut string_literals = Vec::new();
```

…and update the existing `line_comment_ranges.push((start, i));` inside the `b'/'`
line-comment arm to `line_comments.push((start, i));`.

In the `b'r' | b'b' | b'c'` arm, record the span before advancing:

```rust
            b'r' | b'b' | b'c' => match raw_or_byte_string_end(b, i) {
                Some(end) => {
                    blank(&mut out, i, end);
                    string_literals.push((i, end));
                    i = end;
                }
                None => i += 1,
            },
```

In the `b'"'` arm, record the span after the scan loop, right beside the existing
`blank` call:

```rust
                blank(&mut out, start, i);
                string_literals.push((start, i));
```

Leave the `b'\''` (char literal) arm **unchanged** — it must not push.

Replace the final `(out, line_comment_ranges)` with:

```rust
    Masked {
        buf: out,
        line_comments,
        string_literals,
    }
```

- [ ] **Step 5: Update the only caller**

`tests/workspace-lints/src/lib.rs:121-123` currently reads:

```rust
    let (masked, line_comment_ranges) = mask_trivia(src);
    let allow_lines = collect_allow_marker_lines(src, &line_comment_ranges);
    let b = &masked[..];
```

Replace with:

```rust
    let masked = mask_trivia(src);
    let allow_lines = collect_allow_marker_lines(src, &masked.line_comments);
    let b = &masked.buf[..];
```

- [ ] **Step 6: Run the full crate suite**

Run: `cargo test -p paigasus-helikon-workspace-lints`
Expected: PASS — both new unit tests, every pre-existing unit test, and the
workspace-walking `tracing_target_syntax.rs`. That last one is the regression gate
for this signature change; if it fails, the masking behaviour changed and Step 4 is
wrong.

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-workspace-lints --all-targets -- -D warnings
git add tests/workspace-lints/src/lib.rs
git commit -m "test(lints): SMA-557 report string-literal spans from mask_trivia"
```

---

### Task 2: `scan_targets`

**Files:**
- Modify: `tests/workspace-lints/src/lib.rs` (add `scan_targets`, `component_of`, `find_sub`, and unit tests)

**Interfaces:**
- Consumes: `Masked` / `mask_trivia` from Task 1.
- Produces: `pub fn scan_targets(src: &str) -> std::collections::BTreeSet<String>`.
  Task 4 calls this once per `.rs` file and unions the results.

**Why this is safe against self-scan.** The crate's own fixtures are written as
`let src = "…target: \"paigasus::x\"…"` — a single *outer* literal. `mask_trivia`
blanks the whole span, so no `target:` token is ever visible inside it and no phantom
component is produced. This preserves the deliberate absence of path-based
self-exclusion documented at `tests/workspace-lints/src/lib.rs:846-848`.

**Accepted limitation, to be stated in the doc comment.** `scan_targets` is *not*
macro-aware. It keys on a `target:` token followed by a literal whose content starts
`paigasus::`. A non-`tracing` struct field named `target` holding a `paigasus::`-prefixed
string would be a false positive. No such site exists today (the workspace's other
`target:` fields hold `"/etc/passwd"`, `"budgeting specialist"`, and non-literals), and
the failure mode is a loud, self-correcting CI message rather than a silent miss.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block:

```rust
    /// The ordinary case: a component is taken from between the `paigasus::`
    /// prefix and the next `::`.
    #[test]
    fn scan_targets_extracts_components() {
        let src = concat!(
            "tracing::debug!(target: \"paigasus::openai::chat\", \"m\");\n",
            "tracing::warn!(target: \"paigasus::litellm::stream\", \"m\");\n",
            "tracing::warn!(target: \"paigasus::openai::responses\", \"m\");\n",
        );
        let got: Vec<String> = scan_targets(src).into_iter().collect();
        assert_eq!(got, vec!["litellm".to_owned(), "openai".to_owned()]);
    }

    /// A macro spanning several lines is the dominant real-world shape.
    #[test]
    fn scan_targets_handles_multiline_macros() {
        let src = "tracing::warn!(\n    target: \"paigasus::bedrock::translate\",\n    \"m\"\n);\n";
        let got: Vec<String> = scan_targets(src).into_iter().collect();
        assert_eq!(got, vec!["bedrock".to_owned()]);
    }

    /// A literal with no second `::` still yields a component. This shape does
    /// not occur in the workspace today; it must not panic.
    #[test]
    fn scan_targets_accepts_a_bare_component() {
        let src = "tracing::warn!(target: \"paigasus::gemini\", \"m\");\n";
        let got: Vec<String> = scan_targets(src).into_iter().collect();
        assert_eq!(got, vec!["gemini".to_owned()]);
    }

    /// Targets outside the namespace are not components.
    #[test]
    fn scan_targets_ignores_foreign_targets() {
        let src = "tracing::warn!(target: \"hyper::client\", \"m\");\n";
        assert!(scan_targets(src).is_empty());
    }

    /// `target =` is the SMA-543 defect: it records an ordinary field and the
    /// event never lands on that target, so it is not a target site at all.
    #[test]
    fn scan_targets_ignores_the_equals_form() {
        let src = "tracing::warn!(target = \"paigasus::openai::chat\", \"m\");\n";
        assert!(scan_targets(src).is_empty());
    }

    /// Comments are not code. This is not hypothetical: a `///` doc comment at
    /// `crates/paigasus-helikon-providers-litellm/src/translate/request.rs:497`
    /// made the spec's first inventory count 57 sites where there are 56.
    #[test]
    fn scan_targets_ignores_comments() {
        for src in [
            "// tracing::warn!(target: \"paigasus::ghost::x\", \"m\");\n",
            "/// reinstates `target: \"paigasus::ghost::x\"` inside this\n",
            "/* tracing::warn!(target: \"paigasus::ghost::x\"); */\n",
        ] {
            assert!(scan_targets(src).is_empty(), "leaked from: {src}");
        }
    }

    /// A target inside an outer string literal is a test fixture, not a call
    /// site. This property is what lets the guard scan its own source without
    /// path-based self-exclusion.
    #[test]
    fn scan_targets_ignores_nested_literals() {
        let src = "let fixture = \"tracing::warn!(target: \\\"paigasus::ghost::x\\\")\";\n";
        assert!(scan_targets(src).is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p paigasus-helikon-workspace-lints --lib`
Expected: FAIL to compile — `cannot find function 'scan_targets' in this scope`.

- [ ] **Step 3: Write the implementation**

Add near the other free functions in `tests/workspace-lints/src/lib.rs` (after
`try_scan` is a good home), plus the `BTreeSet` import at the top of the file:

```rust
use std::collections::BTreeSet;
```

```rust
/// Distinct `<component>` segments of every `target: "paigasus::…"` literal in
/// one file's source.
///
/// This is the source half of the doc-sync guard in
/// `tests/workspace-lints/tests/tracing_target_docs.rs`: the components found
/// here must match the ones the mdBook documents. It reports **components
/// only** — the `::<subsystem>` leaf is explicitly free to change (SMA-557 D1),
/// so guarding it would redden CI on legitimate refactors.
///
/// Comments, char literals and text nested inside a string literal are invisible
/// to it, because it looks for `target:` in `mask_trivia`'s masked buffer and
/// reads the literal's contents back out of the original source.
///
/// Not macro-aware: it keys on a `target:` token followed by a `paigasus::`
/// literal, so a non-`tracing` field named `target` holding such a string would
/// be a false positive. No such site exists in this workspace, and the failure
/// mode is a loud mismatch rather than a silent miss.
///
/// A comment may sit between `target:` and its literal — `tracing` accepts
/// `target: /* note */ "paigasus::x::y"` — and that form is recognised. What is
/// **not** recognised is a target that is not a literal at all:
/// `target: SOME_CONST`, a `const &'static str`, which `tracing` also accepts,
/// yields no component. No such site exists in this workspace today.
pub fn scan_targets(src: &str) -> BTreeSet<String> {
    const NEEDLE: &[u8] = b"target:";
    let masked = mask_trivia(src);
    let b = &masked.buf[..];
    let mut out = BTreeSet::new();
    let mut i = 0;
    while let Some(rel) = find_sub(&b[i..], NEEDLE) {
        let after = i + rel + NEEDLE.len();
        // Take the next literal whose span is separated from `target:` by
        // nothing but whitespace *in the masked buffer*. That test is what
        // makes a comment in the gap transparent: `mask_trivia` blanks
        // comments to spaces, so they read as whitespace here, while any real
        // token — an identifier, a `format!`, an opening paren — does not, and
        // correctly rejects the match. Testing the raw source instead would
        // stop at the comment's leading `/` and silently skip the site.
        if let Some(&(start, end)) = masked.string_literals.iter().find(|&&(start, _)| {
            start >= after && b[after..start].iter().all(u8::is_ascii_whitespace)
        }) {
            if let Some(component) = component_of(&src[start..end]) {
                out.insert(component);
            }
        }
        i = after;
    }
    out
}

/// The `<component>` of a `"paigasus::<component>::…"` string literal, given the
/// literal's raw text **including** its delimiters.
///
/// Returns `None` for a literal outside the namespace, or one whose component
/// segment is empty (`"paigasus::"`).
fn component_of(literal: &str) -> Option<String> {
    let open = literal.find('"')?;
    let close = literal.rfind('"')?;
    if close <= open {
        return None;
    }
    let content = literal.get(open + 1..close)?;
    let rest = content.strip_prefix("paigasus::")?;
    let component = match rest.find("::") {
        Some(k) => &rest[..k],
        None => rest,
    };
    if component.is_empty() {
        None
    } else {
        Some(component.to_owned())
    }
}

/// Index of the first occurrence of `needle` in `haystack`, or `None`.
///
/// `std` has no substring search for `&[u8]`, and this crate takes no
/// dependencies.
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p paigasus-helikon-workspace-lints --lib`
Expected: PASS — all seven new tests plus every pre-existing one.

If `scan_targets_ignores_the_equals_form` fails, the needle is matching `target`
without the colon — check `NEEDLE` is `b"target:"`.

- [ ] **Step 5: Verify it reads real source**

Run:
```bash
cargo test -p paigasus-helikon-workspace-lints
cargo fmt --all
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-workspace-lints --no-deps
cargo clippy -p paigasus-helikon-workspace-lints --all-targets -- -D warnings
```
Expected: all green.

**`cargo doc` is in that list for a reason.** `scan_targets` is `pub`, and a
`///` intra-doc link from a `pub` item to a private one (writing
`` [`mask_trivia`] `` rather than `` `mask_trivia` ``) fails
`rustdoc::private_intra_doc_links` under `-D warnings` — while `cargo test`,
`cargo clippy` and `cargo fmt` all stay green. CI's `docs` job is a required
context, so that slip blocks merge and nothing else catches it.

- [ ] **Step 6: Commit**

```bash
git add tests/workspace-lints/src/lib.rs
git commit -m "test(lints): SMA-557 add scan_targets to extract namespace components"
```

---

### Task 3: The book section

**Files:**
- Modify: `docs/book/src/concepts/observability-evaluation.md` — insert after line 96
  (the paragraph ending `…for what each crate ships.`) and before line 98 (`## Evaluation`)

**Interfaces:**
- Consumes: nothing.
- Produces: an HTML-marked region containing a table whose first column holds
  `` `paigasus::<component>` `` cells. Task 4 parses exactly that.

**The facts below were verified by execution** (spec §1.2) — do not "correct" them
from intuition. `EnvFilter` matches by raw string prefix, so `paigasus=debug` reaches
`paigasus_helikon_core::session` too, and `paigasus::openai=debug` would also reach a
hypothetical `paigasus::openai_compat::chat`.

- [ ] **Step 1: Confirm the baseline builds**

Run: `mdbook build docs/book`
Expected: clean. If it is not clean *before* your edit, stop and report — the gate
is `warning-policy = "error"` and you need a trustworthy baseline.

- [ ] **Step 2: Insert the section**

Insert this verbatim between the `…for what each crate ships.` paragraph and
`## Evaluation`:

````markdown
### Filtering by target

Every `tracing` event and span carries a **target**. Helikon's targets come from
two namespaces:

- **`paigasus::<component>::<subsystem>`** — hand-chosen targets, written
  explicitly at the call site and independent of the Rust module the code lives
  in. Today these are the five model providers, plus one call site in
  `paigasus-helikon-runtime-temporal`.
- **`paigasus_helikon_*::…`** — ordinary Rust module paths, the `tracing`
  default. Nearly everything in `paigasus-helikon-core` and the runtime crates
  emits here.

**`EnvFilter` matches a directive against a target by raw string prefix, not by
`::` segment.** That one fact decides every recipe below:

| Directive | Reaches |
| --- | --- |
| `paigasus` | **Both** namespaces — it is a raw prefix, so it also matches `paigasus_helikon_core::session`. |
| `paigasus::` | The hand-chosen namespace only. The trailing `::` is what excludes the module paths. |
| `paigasus::openai` | One component. |
| `paigasus::openai::chat` | One subsystem. |

<!-- tracing-components:start — keep in sync; asserted by
     tests/workspace-lints/tests/tracing_target_docs.rs -->

| Component | Crate | Subsystems today | Status |
| --- | --- | --- | --- |
| `paigasus::openai` | `paigasus-helikon-providers-openai` | `translate`, `chat`, `responses` | stable |
| `paigasus::anthropic` | `paigasus-helikon-providers-anthropic` | `translate`, `stream`, `sse` | stable |
| `paigasus::bedrock` | `paigasus-helikon-providers-bedrock` | `translate`, `stream`, `builder` | stable |
| `paigasus::gemini` | `paigasus-helikon-providers-gemini` | `translate`, `sse` | stable |
| `paigasus::litellm` | `paigasus-helikon-providers-litellm` | `translate`, `stream`, `sse`, `http` | stable |
| `paigasus::temporal` | `paigasus-helikon-runtime-temporal` | `activities` | provisional |

<!-- tracing-components:end -->

The **Subsystems today** column lists what exists at the time of writing, not a
fixed set — see the stability rules below.

#### What is not in this namespace

`paigasus-helikon-core` and the runtime crates do **not** use hand-chosen
targets, with the single exception of `paigasus::temporal::activities` noted
below. Their events and spans land on module paths, so you select them by
crate:

```
paigasus_helikon_core
paigasus_helikon_runtime_axum
paigasus_helikon_runtime_actix
paigasus_helikon_runtime_agentcore
paigasus_helikon_runtime_temporal   # all but one site; see paigasus::temporal below
paigasus_helikon_runtime_tokio
```

This includes **the run/turn/chat trace tree described above** — the
`agent.run`, `agent.turn`, `gen_ai.chat` and `tool.execute` spans come from
`paigasus_helikon_core`, not from `paigasus::*`. (The `invoke_agent` /
`agent.turn` / `chat` / `execute_tool` operation names used above are the
`gen_ai.operation.name` fields set on these same spans.) Most are raised in
`paigasus_helikon_core::agent`; the multi-agent constructs — the sequential,
parallel and loop workflows, plus the graph and swarm agents — raise their own
`agent.run` span in `paigasus_helikon_core::workflow`. Filter on
`paigasus_helikon_core` to catch both: a narrower `paigasus_helikon_core::agent`
silently misses a multi-agent run's top-level span.

`paigasus::temporal` is a single call site in a crate that is otherwise
untargeted, which is why it is marked *provisional* above: it is listed so the
namespace is completely described, but it does not carry the guarantee the
provider components do. Whether the core and runtime crates should adopt
hand-chosen targets is tracked as a follow-up.

#### Stability

The namespace is a two-tier contract.

- **`paigasus::` and `paigasus::<component>` are stable**, for every component
  the table above marks *stable*. Renaming or removing one is a breaking change,
  made through a commit carrying a `BREAKING CHANGE:` footer so it appears in the
  crate's CHANGELOG. A component marked *provisional* carries no such promise and
  may be renamed or removed in any release.
- **No component name will ever be a prefix of another.** This one is
  namespace-wide and binds *provisional* components exactly as much as stable
  ones — it is not part of the guarantee above. A collision would silently widen
  a filter that is already deployed, since matching is prefix-based, and a new
  component's status is no comfort to an operator whose alert quietly started
  matching more than it did yesterday.
- **The `::<subsystem>` leaf is an implementation detail** and may change in any
  release without notice.

So: use **exactly two segments** for anything durable — alerting rules,
dashboards, saved queries. Use three segments for interactive debugging, and
expect them to move. A bare `paigasus` is a raw prefix, not a namespace
selector; reach for `paigasus::` when you mean the curated targets.

This guarantee begins with this document and is not retroactive.

#### Recipes

Warnings everywhere, one provider verbose:

```
RUST_LOG='warn,paigasus::openai=debug'
```

The hand-chosen namespace only, excluding core and runtime module paths — note
the trailing `::`:

```
RUST_LOG='warn,paigasus::=debug'
```

One subsystem and nothing else. This is a three-segment selector, so treat it as
a debugging tool: the `stream` leaf may be renamed in any release, and if this
example ever stops matching, that is why.

```
RUST_LOG='off,paigasus::litellm::stream=trace'
```

The agent trace tree — the `agent.run` / `agent.turn` / `gen_ai.chat` /
`tool.execute` spans:

```
RUST_LOG='warn,paigasus_helikon_core=debug'
```

These set the level for a `tracing-subscriber` `EnvFilter`; see
[`tracing_subscriber::EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
for the full directive grammar.
````

- [ ] **Step 3: Verify the book still builds**

Run: `mdbook build docs/book`
Expected: clean. `follow-web-links = false`, so the docs.rs link is not fetched —
but a malformed markdown table or a broken relative link will fail here.

- [ ] **Step 4: Verify the markers are unique and well-formed**

Run: `grep -c 'tracing-components:start\|tracing-components:end' docs/book/src/concepts/observability-evaluation.md`
Expected: `2`

- [ ] **Step 5: Commit**

```bash
git add docs/book/src/concepts/observability-evaluation.md
git commit -m "docs(docs): SMA-557 document the paigasus tracing target namespace"
```

---

### Task 4: The drift guard

**Files:**
- Create: `tests/workspace-lints/tests/tracing_target_docs.rs`

**Interfaces:**
- Consumes: `paigasus_helikon_workspace_lints::scan_targets` (Task 2); the marked
  region in `docs/book/src/concepts/observability-evaluation.md` (Task 3).
- Produces: nothing later tasks use.

The walker mirrors `tests/workspace-lints/tests/tracing_target_syntax.rs`. Read that
file first — this task deliberately reuses its shape (manifest-relative root,
symlink-safe walk, anti-vacuity floor) so the two stay recognisably siblings.
Beyond the source-vs-docs diff, the test also asserts that no two components
collide by prefix (SMA-557 D1(b)): `EnvFilter` matches targets by raw string
prefix, so a saved `paigasus::openai` filter would otherwise silently widen to
catch a future `paigasus::openai_compat`.

> **Authoritative source:** the code block in Step 1 is a snapshot of
> `tests/workspace-lints/tests/tracing_target_docs.rs`. If the two ever
> disagree, the shipped test wins — treat this block as illustrative, not as
> something to diff against blindly.

- [ ] **Step 1: Write the test**

Create `tests/workspace-lints/tests/tracing_target_docs.rs`:

```rust
//! The `paigasus::<component>` set in source must equal the set documented in
//! the mdBook (SMA-557 D3).
//!
//! Components only. The `::<subsystem>` leaf is explicitly free to change, so
//! guarding it would redden CI on legitimate refactors.
//!
//! This asserts **presence, not guarantee**: the table's `Status` column
//! (`stable` / `provisional`) is ignored here. A provisional row is tracked
//! exactly like a stable one — the column is documentation for humans.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use paigasus_helikon_workspace_lints::scan_targets;

const BOOK_PAGE: &str = "docs/book/src/concepts/observability-evaluation.md";
const MARK_START: &str = "tracing-components:start";
const MARK_END: &str = "tracing-components:end";

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
///
/// Uses [`std::fs::symlink_metadata`] rather than [`Path::is_dir`]: `is_dir`
/// follows symlinks, so a symlink pointing at an ancestor directory would
/// recurse this walk unboundedly.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        let meta = std::fs::symlink_metadata(&path)
            .unwrap_or_else(|e| panic!("symlink_metadata {path:?}: {e}"));
        if meta.is_symlink() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if meta.is_dir() {
            if name == "target" || name == ".git" {
                continue;
            }
            collect_rs(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// Components named in the marked region of the book page.
///
/// Parse rule, pinned deliberately: the **first cell of each table body row**
/// between the markers must be exactly `` `paigasus::<component>` ``. Header and
/// separator rows are skipped; any other non-empty row is a hard failure, so a
/// stray placeholder becomes a loud error instead of a phantom component.
fn documented_components(page: &str) -> BTreeSet<String> {
    // Exactly one pair, not merely at least one. `find` takes the first hit, so
    // a duplicated marker pair would silently parse only the first region and
    // ignore whatever the second one documents — a drift the guard exists to
    // catch reading as a clean pass.
    for (name, count) in [
        (MARK_START, page.matches(MARK_START).count()),
        (MARK_END, page.matches(MARK_END).count()),
    ] {
        assert_eq!(
            count, 1,
            "`{name}` appears {count} time(s) in {BOOK_PAGE}; expected exactly 1"
        );
    }
    let start = page
        .find(MARK_START)
        .unwrap_or_else(|| panic!("missing `{MARK_START}` marker in {BOOK_PAGE}"));
    let end = page
        .find(MARK_END)
        .unwrap_or_else(|| panic!("missing `{MARK_END}` marker in {BOOK_PAGE}"));
    assert!(
        end > start,
        "`{MARK_END}` precedes `{MARK_START}` in {BOOK_PAGE}"
    );

    let mut out = BTreeSet::new();
    for line in page[start..end].lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let first = line
            .trim_start_matches('|')
            .split('|')
            .next()
            .unwrap_or_default()
            .trim();
        // Header row and the `| --- |` separator carry no component. The
        // emptiness guard matters: `str::chars().all(...)` is vacuously true
        // on an empty string, so without it a row with an accidentally blank
        // first cell (`| | some-crate | ... | ... |`) would be misclassified
        // as a separator and silently skipped instead of hitting the hard
        // failure below, as the doc comment above promises.
        if first == "Component"
            || (!first.is_empty() && first.chars().all(|c| c == '-' || c == ':'))
        {
            continue;
        }
        let component = first
            .strip_prefix("`paigasus::")
            .and_then(|r| r.strip_suffix('`'))
            .unwrap_or_else(|| {
                panic!(
                    "row in the marked region of {BOOK_PAGE} has first cell {first:?}; \
                     expected exactly `paigasus::<component>`"
                )
            });
        assert!(
            !component.is_empty()
                && component
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "component {component:?} in {BOOK_PAGE} is not [a-z0-9_]+"
        );
        out.insert(component.to_owned());
    }
    out
}

#[test]
fn documented_components_match_source() {
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

    // Anti-vacuity: a truncated walk must fail, not pass. Set well below the
    // repo's actual file count so it does not couple to workspace size.
    assert!(
        files.len() >= 100,
        "scanned only {} .rs files — the walk is not reaching the workspace",
        files.len()
    );

    // Anti-vacuity: prove the scanner reads real source rather than returning a
    // constant. A path-existence assertion would not — `tests/` contributes no
    // components at all, so reaching it proves nothing about extraction.
    let probe = root.join("crates/paigasus-helikon-providers-openai/src/backend/chat.rs");
    let probe_src =
        std::fs::read_to_string(&probe).unwrap_or_else(|e| panic!("read {}: {e}", probe.display()));
    assert_eq!(
        scan_targets(&probe_src),
        BTreeSet::from(["openai".to_owned()]),
        "scan_targets did not extract `openai` from {}",
        probe.display()
    );

    let mut in_source = BTreeSet::new();
    for file in &files {
        let src = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        in_source.extend(scan_targets(&src));
    }
    assert!(
        !in_source.is_empty(),
        "no `paigasus::` targets found in {} files — the scan is not working",
        files.len()
    );

    // Enforces the book's stability-contract clause (SMA-557 D1(b)): "No
    // component name will ever be a prefix of another — that would silently
    // widen a saved filter." This is a correctness property of the namespace
    // itself, not a style rule — `EnvFilter` matches by raw string prefix, so
    // a future `openai_compat` alongside `openai` would otherwise pass this
    // guard as long as its row was added.
    let mut prefix_collisions = Vec::new();
    for shorter in &in_source {
        for longer in &in_source {
            if shorter != longer && longer.starts_with(shorter.as_str()) {
                prefix_collisions.push((shorter.clone(), longer.clone()));
            }
        }
    }
    assert!(
        prefix_collisions.is_empty(),
        "tracing component name(s) collide by prefix: {prefix_collisions:?}\n\
         A saved `paigasus::<shorter>` filter would silently widen to include \
         `paigasus::<longer>` too, since EnvFilter matches targets by raw \
         string prefix. Rename one of the colliding components (SMA-557 D1)."
    );

    let page_path = root.join(BOOK_PAGE);
    let page = std::fs::read_to_string(&page_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", page_path.display()));
    let in_docs = documented_components(&page);
    assert!(
        !in_docs.is_empty(),
        "the marked region in {BOOK_PAGE} documents no components"
    );

    let undocumented: Vec<&String> = in_source.difference(&in_docs).collect();
    let stale: Vec<&String> = in_docs.difference(&in_source).collect();
    assert!(
        undocumented.is_empty() && stale.is_empty(),
        "tracing component drift between source and {BOOK_PAGE}:\n  \
         in source but not documented: {undocumented:?}\n  \
         documented but not in source: {stale:?}\n\
         Add or remove the row in the marked region. For a component the table \
         marks `stable`, renaming or removing it is a breaking change \
         (SMA-557 D1) — use a `BREAKING CHANGE:` footer. A `provisional` \
         component carries no such guarantee."
    );
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs`
Expected: PASS. Both sets should be
`{anthropic, bedrock, gemini, litellm, openai, temporal}`, and since none of
those names is a prefix of another, the prefix-collision assertion passes
silently alongside the source-vs-docs check.

If it fails with `documented but not in source`, Task 3's table has a typo. If it
fails with `in source but not documented`, re-read the failure — it is telling you
the truth about source.

- [ ] **Step 3: Mutation check — doc side**

```bash
sed -i '' '/| `paigasus::gemini` |/d' docs/book/src/concepts/observability-evaluation.md
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```
Expected: **FAIL**, naming `gemini` under *in source but not documented*.

Restore from the backup and re-verify:
```bash
cp "$BACKUP_DIR/observability-evaluation.md" docs/book/src/concepts/observability-evaluation.md
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```
Expected: PASS.

- [ ] **Step 4: Mutation check — source side**

A doc-side mutation alone would still pass if `scan_targets` returned a hardcoded
set. Mutate source too:

```bash
sed -i '' 's/target: "paigasus::openai::chat"/target: "paigasus::zzz::chat"/' \
  crates/paigasus-helikon-providers-openai/src/backend/chat.rs
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```
Expected: **FAIL**, naming `zzz` under *in source but not documented*.

Restore from the backup and re-verify:
```bash
cp "$BACKUP_DIR/chat.rs" crates/paigasus-helikon-providers-openai/src/backend/chat.rs
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```
Expected: PASS.

> **Restore from a backup copy, never `git checkout -- <path>`.** That command
> reverts the file to `HEAD`, discarding *every* uncommitted change in it — not
> just your mutation. This is not hypothetical: during this ticket's own final
> fix wave it silently wiped legitimate in-progress edits to the book page,
> which had to be reconstructed. Take the backups before Step 3:
> ```bash
> BACKUP_DIR=$(mktemp -d)
> cp docs/book/src/concepts/observability-evaluation.md "$BACKUP_DIR/"
> cp crates/paigasus-helikon-providers-openai/src/backend/chat.rs "$BACKUP_DIR/"
> ```

> Confirm `git status --short` is clean of those two paths before continuing. A
> leftover mutation committed by accident is a silent corruption of a published
> crate.

- [ ] **Step 5: Full local gates**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-workspace-lints
mdbook build docs/book
```
Expected: all green. Run these **synchronously in the foreground** — do not
background them and end your turn.

- [ ] **Step 6: Commit**

```bash
git add tests/workspace-lints/tests/tracing_target_docs.rs
git commit -m "test(lints): SMA-557 assert documented components match source"
```

---

## Final verification

Run before opening the PR — this is the full CI-equivalent set for this change:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
mdbook build docs/book
```

`cargo test --workspace --all-features` matters here even though no published crate
changed: this work **adds a test**, and `test (ubuntu-latest, stable)` is a required
context.

> **Known local red herring, not caused by this change:** on macOS, running the
> workspace suite from the primary checkout path can produce ~48 bedrock
> `NATIVE_ROOTS` failures in ~15s. That tracks the checkout path, not the code —
> this worktree is the control. If you see it, confirm by checking whether the
> failures exist on a clean `main` in the same tree before attributing them here.

Also confirm no stray mutation survived:

```bash
git status --short
git diff main --stat
```
Expected: only `docs/book/src/concepts/observability-evaluation.md`,
`tests/workspace-lints/src/lib.rs`, `tests/workspace-lints/tests/tracing_target_docs.rs`,
and the two `docs/superpowers/` artifacts.

## Out of scope

Do not, in this PR (spec §6):

- Add `target:` to any of the 41 untargeted `core`/runtime call sites.
- Resolve `runtime-temporal`'s 1-targeted/3-untargeted split.
- Add a `book` scope to `.versionrc`.
- Edit any crate `README.md` — no crate's public surface changes.
