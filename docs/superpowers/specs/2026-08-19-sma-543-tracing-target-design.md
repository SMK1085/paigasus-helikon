# SMA-543 — `tracing::warn!` uses `target =` instead of `target:`

**Status:** revised after adversarial challenge (2026-08-19)
**Linear:** [SMA-543](https://linear.app/smaschek/issue/SMA-543/tracingwarn-uses-target-instead-of-target-in-both-providers-chat)
**Branch:** `feature/sma-543-tracingwarn-uses-target-instead-of-target-in-both-providers`

## 1. Problem

Seven `tracing::warn!` call sites pass `target = "…"` (equals) where the macro
expects `target: "…"` (colon). `target:` is macro syntax that sets the event's
metadata target; `target =` is an ordinary key-value field, so the event stays
on the default module-path target *and* carries a misleading field named
`target`.

### 1.1 Confirmed empirically, not assumed

Probed against `tracing` 0.1 / `tracing-subscriber` 0.3 with a capture `Layer`:

| Form | `metadata().target()` | recorded fields |
|---|---|---|
| `warn!(target = "paigasus::openai::translate", "msg")` | `<module path>` | `message`, **`target="paigasus::openai::translate"`** |
| `warn!(target: "paigasus::openai::translate", "msg")` | `paigasus::openai::translate` | `message` |

And with `EnvFilter::new("off,paigasus::openai::translate=warn")`, **only the
colon form passes the filter**. The ticket's claim — an operator filtering on
the claimed target silently sees nothing — is reproduced, not inferred.

This probe was a throwaway scratch crate. §4.6 makes the property it
established permanently reproducible inside the repo, so this table stops
being an unverifiable assertion in a document.

### 1.2 Exact inventory

The ticket estimates "four … and the same four, duplicated". The real count is
**seven**, asymmetric:

| File | Lines |
|---|---|
| `crates/paigasus-helikon-providers-openai/src/translate/request.rs` | 83, 205, 211, 345 |
| `crates/paigasus-helikon-providers-litellm/src/translate/request.rs` | 85, 207, 213 |

LiteLLM has **three**, not four: openai's line 345 lives in
`to_responses_input`, which the LiteLLM copy deliberately omits (Chat
Completions only, SMA-451 D10). A scan confirms no other occurrence anywhere
else in the workspace, including the non-`crates/` member
`tests/runtime-http-conformance`.

### 1.3 Reachability

Three of the seven are unreachable today. Lines openai 83, openai 345 and
litellm 85 sit in `_ =>` arms over `Item`. The wildcard arm is *mandatory* —
the provider crates are downstream of `paigasus-helikon-core`, where `Item` is
declared `#[non_exhaustive]` — but it is *unreachable*, because the arms above
it exhaust all five variants that exist today
(`core/src/item.rs:20-61`). The four reachable sites match named `ContentPart`
variants (`Image`/`Audio`/`ToolResult`) and do fire.

This caps what any behavioural test can cover at 4 of 7, and is why §4.6 adds
one narrow behavioural test rather than a suite.

## 2. Constraint: both crates in the same PR (D6)

`paigasus-helikon-providers-litellm` duplicates `to_chat_messages` and its
helpers from `providers-openai` (SMA-451 design decision D6, §13.1). The
duplication's safety story rests on the two copies staying identical in the
shared region apart from the target strings. Fixing one crate alone would
diverge them in a way the cross-crate parity test
(`crates/paigasus-helikon/tests/openai_litellm_message_parity.rs`) cannot
detect, because that test compares *translated `messages`*, not source text.

The requirement is therefore **both crates change in the same PR**. The
ticket's "single commit" phrasing is stricter than the reason supports: two
commits on one branch reach an identical end state, the PR squash-merges to one
`main` commit either way, and release-plz attributes bumps by touched path
regardless of commit granularity. One commit is still the natural shape here
and is what §7 specifies — but as tidiness, not as a correctness constraint.

## 3. The fix

Seven edits, `=` → `:`, at the lines in §1.2. No message text, control flow, or
public-API change. Line 345 is openai-only and outside the shared region, so it
cannot affect parity.

The observable change is precisely the point, and is what the CHANGELOG entry
should convey: each event's `metadata().target()` moves from the module path to
the declared `paigasus::…` namespace, and the spurious recorded field named
`target` disappears.

## 4. Regression guard

### 4.1 Shape

A **workspace-wide static check**, as a Rust test in a new non-published
workspace member:

```
tests/workspace-lints/
  Cargo.toml          # publish = false, version = "0.0.0"
  src/lib.rs          # pub fn scan(src: &str) -> Vec<Offense>  + unit tests
  tests/tracing_target_syntax.rs   # walks the repo, asserts no offenses
```

Registered in the root `[workspace] members` list and given a
`[[package]] … release = false` block in `release-plz.toml`, mirroring the
existing `tests/runtime-http-conformance` member exactly.

### 4.2 Why not the facade

The first draft put this in `crates/paigasus-helikon/tests/`. That is wrong:
the facade is a published crate (`version = "0.5.9"`) with no `include`/
`exclude`, so `cargo package` would ship the check to crates.io, where its
`../../crates` path does not exist — and downstream packaging pipelines that
run `cargo test` on the extracted crate would fail. `cargo publish --verify`
compiles lib and bins but not tests, so this would not have been caught before
release. Placing a repo-internal lint there would also republish the public
facade on every edit to it, and add a third crate to this PR's version bumps.

The sibling `openai_litellm_message_parity.rs` lives in the facade for a reason
that does not apply here: it must link *both* providers through the facade's
`openai` + `litellm` feature flags and drive real model types. A file scanner
has no such requirement.

### 4.3 Detection rule

Operating on each `.rs` file's text, in a single left-to-right pass that skips
line comments, block comments, string literals, raw strings, byte strings and
char literals — so a macro appearing inside a comment, doc example or string is
never flagged, and a `)` inside a literal never miscounts paren depth:

1. Find each `!` followed by `(` whose preceding path segment is one of
   `warn`, `info`, `debug`, `error`, `trace`, `event`,
   `span`, `warn_span`, `info_span`, `debug_span`, `error_span`, `trace_span`.
2. Walk that invocation's argument list, tracking paren/bracket/brace depth so
   only **top-level** arguments are considered.
3. At the start of each top-level argument, flag it if it is the bare
   identifier `target` or `parent` followed (modulo whitespace) by `=` that is
   not `==`.

The failure message lists every offending `file:line` with the argument index.

Hand-rolled. `regex` is not a workspace dependency and this does not justify
adding one.

### 4.4 Why all argument positions, not just the first

The first draft checked only the first argument and documented that as an
accepted limitation. Compiling every candidate form against `tracing` 0.1
showed that limitation to be fatal rather than acceptable:

| Form | rustc |
|---|---|
| `warn!(target = "x", "m")` | **compiles** (the SMA-543 bug) |
| `warn!(parent: None, target = "x", "m")` | **compiles** |
| `info_span!("nm", target = "x")` | **compiles** |
| `event!(Level::WARN, target = "x", "m")` | **compiles** |
| `info_span!(target = "x", "nm")` | *rejected* |
| `event!(target = "x", Level::WARN, "m")` | *rejected* |

For the span and event macros the correct syntax puts `target:`/`parent:`
*before* the level or span name, so the erroneous `=` form is only reachable in
a **later** position — a first-argument-only rule covers nothing at all for six
of the twelve macro names while appearing to. And even for `warn!`, a correct
leading `parent:` pushes the bug into second position, which the first-argument
rule would miss.

Skipping comments and string literals (§4.3) already requires a small lexer;
once that exists, tracking paren depth to reach every top-level argument is
nearly free. The limitation was not worth accepting, and does not need to be.

### 4.5 Failability, anti-vacuity and roots

**Failability is proved on every CI run, not once by hand.** `scan()` is a pure
function over a `&str`, unit-tested against an inline table containing every
compiling bad form in §4.4 plus good forms that must *not* trip it:
`target: "x"`, `parent: parent`, `let target = "x";`, `a == b`, a field
legitimately named `count`, the bad pattern inside a `//` comment, inside a
`///` doc comment, and inside a string literal. A CRLF case is included because
`cargo test --workspace --all-features` also runs on `windows-latest`.

Because the lexer skips comments and string literals, the guard scanning its
own source is a non-issue — the bad forms in its test table are string literals.
There is deliberately **no path-based self-exclusion**: that would leave the one
file most likely to accumulate bad examples permanently unchecked.

**Anti-vacuity.** A scan that walks zero files must fail, not pass. The repo
test therefore asserts that the resolved root exists (panicking if not), that
the number of `.rs` files scanned meets a floor, and that both
`providers-openai/src/translate/request.rs` and
`providers-litellm/src/translate/request.rs` were among the files actually
scanned. This mirrors the explicit "Guard against a vacuous pass" assertion in
`openai_litellm_message_parity.rs:226-239`, and the reasoning behind
`HELIKON_REQUIRE_TEMPORAL` / `HELIKON_REQUIRE_SANDBOX` in CLAUDE.md: a check
that silently examines nothing reports identically to one that passes.

**Root resolution.** The repo root is derived from `CARGO_MANIFEST_DIR`, not
the process CWD, so it survives the member being moved. The walk starts at the
repo root — not at `crates/` — so it covers `tests/runtime-http-conformance`,
which is a full workspace member outside `crates/` and would otherwise be
invisible. Any directory named `target` or `.git` is skipped, guarding against
a nested `CARGO_TARGET_DIR`.

### 4.6 One behavioural test as well

The static guard proves the *syntax*. It cannot prove the *semantics* — that
`target:` actually sets `metadata().target()` — and §1.1's probe leaves no
artifact in the repo.

Add one capture-`Layer` test in `providers-openai` asserting that the warn at
`translate/request.rs:205` emits on `paigasus::openai::translate`. It is cheap:
the reachable input already exists in the neighbouring unit test
`assistant_image_content_part_is_dropped_with_warning` (same file, line 456),
and `tracing-subscriber` is already a `[workspace.dependencies]` entry, so the
dev-dep is one line. Roughly 25 lines in one crate.

The first draft rejected a behavioural test at "roughly 160 lines across two
crates plus a dev-dep in each". That costing was wrong — it priced a full
suite covering all sites in both crates, when one test over one already-reachable
site in one crate establishes the semantic property. The two instruments are
complementary, not substitutes: the static guard has the breadth, this has the
meaning. Precedent for the technique is `core/tests/workflow_tracing.rs`.

### 4.7 Accepted cost

A future *legitimate* tracing field named `target` or `parent`, in any
position, would trip the guard and have to be renamed. That is acceptable — and
arguably correct, since such a field shadows the macro keyword and reads as a
bug at every call site.

## 5. Verification

1. **The detector's unit tests** demonstrate it failing on bad input and
   passing on good input, on every CI run — a durable property, not a one-off
   manual demonstration.
2. **Residual diff between the two `request.rs` files is exactly:**
   - the module doc block (openai lines 1–6 vs litellm 1–8);
   - the four shared target strings (`paigasus::openai::translate` vs
     `paigasus::litellm::translate`) at openai 83/172/205/211 ↔ litellm
     85/174/207/213 — three fixed by this change, plus `media_url`'s at openai
     172 / litellm 174, which was already correct. The fix substitutes
     characters in place and adds no lines, so these numbers hold before and
     after;
   - openai-only `to_responses_input` (245–352) and `mod responses_tests`
     (564–630);
   - litellm-only trailing `chat_tests`:
     `plain_text_user_turn_emits_string_content` (457–463) and
     `tool_call_then_result_round_trips` (465–486).

   **The ticket's stated expectation is wrong** — it predicts "the module doc
   block and the four target strings" plus the omitted `to_responses_input`,
   and does not account for litellm's two extra tests. Run literally, that
   check shows unexpected output and reads as a regression. The list above is
   the correct baseline. Note it is a *snapshot with no enforcement*: nothing
   keeps it true, which is what the deferred D6 source-identity ticket (§6)
   would fix.
3. **Full local CI gate set** in the worktree: `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-features --all-targets -- -D warnings`,
   `cargo test --workspace --all-features`, and
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`.

## 6. Non-goals

- **No D6 source-identity check.** Deferred to a separate Linear issue. The
  parity test compares translated output, so drift introduced *identically*
  into both copies — or any source divergence that still yields the same
  `messages` — is structurally invisible to it. A real gap, but SMA-451
  follow-up scope.
- **No observability documentation.** `paigasus::…` targets, `RUST_LOG` and
  `EnvFilter` appear nowhere in `docs/book/src/` or any crate README, so this
  fix makes a namespace filterable that nothing tells operators about. Worth
  fixing, but as its own ticket against
  `docs/book/src/concepts/observability-evaluation.md`, covering the whole
  target namespace rather than the two strings this PR happens to touch. Under
  CLAUDE.md's rule this is a conscious call, not a silent skip.
- **No version bump beyond the two providers.** Both are already released and
  change only non-public code; release-plz handles the patch bumps and
  CHANGELOG from the `fix(providers):` commit. Putting the guard in a
  `publish = false` member (§4.1) keeps the facade out of it.

## 7. Commit and PR

Single commit and PR title:

```
fix(providers): SMA-543 route chat-translator warnings to their declared tracing target
```

- Full Conventional Commits prefix — `pr-title.yml` enforces the type
  independently of the subject pattern.
- Subject starts lowercase after the `SMA-543 ` key, satisfying
  `subjectPattern: ^([A-Z]{2,4}-\d+ )?[^A-Z].+$`.
- `providers` is in `.github/workflows/pr-title.yml`'s `scopes:` list — which
  is what gates the **PR title**, read from `main` under
  `pull_request_target`. `.versionrc`'s `scopeRegex` gates the **commit
  messages** via convco, and also lists `providers`.
- No colon inside the subject, so there is nothing for the parser to
  mis-split on.

**PR body:** cite PR **#199**, not `SMA-451`. Any `SMA-###` token other than
this issue's own trips CodeRabbit's "Linked Issues" pre-merge check, and that
is unfixable after the fact.
