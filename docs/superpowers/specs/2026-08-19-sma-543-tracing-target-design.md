# SMA-543 — `tracing::warn!` uses `target =` instead of `target:`

**Status:** approved (2026-08-19)
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

### 1.2 Exact inventory

The ticket estimates "four … and the same four, duplicated". The real count is
**seven**, asymmetric:

| File | Lines |
|---|---|
| `crates/paigasus-helikon-providers-openai/src/translate/request.rs` | 83, 205, 211, 345 |
| `crates/paigasus-helikon-providers-litellm/src/translate/request.rs` | 85, 207, 213 |

LiteLLM has **three**, not four: openai's line 345 lives in
`to_responses_input`, which the LiteLLM copy deliberately omits (Chat
Completions only, SMA-451 D10). A workspace-wide scan confirms no other
occurrence anywhere else in the workspace.

### 1.3 Reachability

Three of the seven are unreachable today. Lines openai 83, openai 345 and
litellm 85 sit in `_ =>` arms over `Item`. `Item` is `#[non_exhaustive]`, but
it is defined in `paigasus-helikon-core` — `#[non_exhaustive]` constrains
*downstream* crates, and within a provider crate the match already covers every
existing variant, so the arm is defensive future-proofing that no input can
reach. The four reachable sites match named `ContentPart` variants
(`Image`/`Audio`/`ToolResult`) and do fire.

This matters because it caps what any behavioural test could cover at 4 of 7,
and it is the reason §4 rejects that option.

## 2. Constraint: one commit, both crates (D6)

`paigasus-helikon-providers-litellm` duplicates `to_chat_messages` and its
helpers from `providers-openai` (SMA-451 design decision D6, §13.1). The
duplication's safety story rests on the two copies staying identical in the
shared region apart from the target strings. Fixing one crate alone would
diverge them in a way the cross-crate parity test
(`crates/paigasus-helikon/tests/openai_litellm_message_parity.rs`) cannot
detect, because that test compares *translated `messages`*, not source text.

Both crates therefore change in a single commit.

## 3. The fix

Seven edits, `=` → `:`, at the lines in §1.2. No message text, field, control
flow, or public API changes. Line 345 is openai-only and outside the shared
region, so it cannot affect parity.

## 4. Regression guard

### 4.1 Decision

A **workspace-wide static check**, as a Rust test:
`crates/paigasus-helikon/tests/tracing_target_syntax.rs`.

For every `.rs` file under `crates/`, find each `!(` whose macro path's last
segment is one of `warn`, `info`, `debug`, `error`, `trace`, `event`, `span`,
`warn_span`, `info_span`, `debug_span`, `error_span`, `trace_span`. Fail if the
first argument is the bare identifier `target` or `parent` followed by `=`
(and not `==`). The failure message lists every offending `file:line`.

Hand-rolled scan. `regex` is not a workspace dependency and this does not
justify adding one.

### 4.2 Why this shape

- **Covers all seven**, including the three unreachable arms that no
  behavioural test can reach.
- **Covers every future crate**, not just these two files. The bug is a syntax
  slip, so a syntactic instrument is the proportionate one.
- **Rides an existing required gate.** `cargo test --workspace --all-features`
  is already required on `main`; no new CI job, no new required-check context,
  no `main-protection-checks.json` edit.
- **`parent =` is not hypothetical.** `core/src/agent.rs:543` uses
  `tracing::info_span!(parent: parent, …)`, so the sibling keyword is live in
  this codebase and worth guarding at the same time.
- Placed in the facade beside the existing cross-crate
  `openai_litellm_message_parity.rs`, which sets the precedent for
  workspace-scope tests living there.

### 4.3 Documented limitation

The check inspects the **first** argument only. `warn!(target: "x", parent = p,
…)` would slip through. Detecting that needs balanced-paren and string-literal
aware scanning of the argument list; the added complexity is not worth it for a
form nobody writes, and an over-clever guard test is its own maintenance
liability. This boundary is stated in the test's own doc comment so it is a
known limit rather than an unexamined gap.

### 4.4 Accepted cost

A future *legitimate* tracing field named `target` or `parent` would trip the
guard and have to be renamed. That is acceptable — and arguably correct, since
such a field shadows the macro keyword and reads as a bug at every call site.

## 5. Verification

1. **Guard fails before, passes after.** Run the new test against the
   pre-fix tree and confirm it names all seven sites; then against the fixed
   tree and confirm it passes. A guard demonstrated only in the green
   direction proves nothing about whether it can fail.
2. **Residual diff between the two `request.rs` files is exactly:**
   - the module doc block (lines 1–6 vs 1–8);
   - the four shared target strings (`paigasus::openai::translate` vs
     `paigasus::litellm::translate`) at openai 83/172/205/211 ↔ litellm
     85/174/207/213 — three of them fixed by this change, plus `media_url`'s
     at openai 172 / litellm 174, which was already correct. The fix
     substitutes characters in place and adds no lines, so these line numbers
     are unchanged before and after;
   - openai-only `to_responses_input` and `mod responses_tests`;
   - litellm-only trailing `chat_tests`: `plain_text_user_turn_emits_string_content`
     and `tool_call_then_result_round_trips`.

   **The ticket's stated expectation is wrong** — it predicts a diff of "the
   module doc block and the four target strings" plus the omitted
   `to_responses_input`, and does not account for litellm's two extra tests.
   Run literally, that check shows unexpected output and reads as a
   regression. The list above is the correct baseline.
3. **Full local CI gate set** in the worktree: `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-features --all-targets -- -D warnings`,
   `cargo test --workspace --all-features`, and
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`.

## 6. Non-goals

- **No behavioural (capture-`Layer`) test.** It would reach 4 of 7 sites, cost
  roughly 160 lines across two crates plus a `tracing-subscriber` dev-dep in
  each, and assert a string constant. The static guard is strictly wider
  coverage at a fraction of the cost. Precedent for the technique exists
  (`core/tests/workflow_tracing.rs`); it is simply the wrong instrument here.
- **No book or README edit.** `paigasus::…` target strings appear nowhere in
  `docs/book/src/` or any crate README. They are an undocumented internal
  log-filtering detail, so there is no user-facing documentation to bring into
  line. This is a conscious call under CLAUDE.md's "make it a conscious call,
  not a silent skip" rule, not an oversight.
- **No D6 source-identity check.** Deferred to a separate Linear issue. The
  parity test compares translated output, so a drift introduced *identically*
  into both copies — or any divergence in source text that happens to produce
  the same `messages` — is structurally invisible to it. That is a real gap,
  but it is SMA-451 follow-up scope, not SMA-543.
- **No version bumps.** Both crates are already released and change only
  non-public code; release-plz handles the patch bump and CHANGELOG from the
  `fix(providers):` commit through its normal flow.

## 7. Commit and PR

Single commit and PR title:

```
fix(providers): SMA-543 route chat-translator warnings to their declared tracing target
```

- Full Conventional Commits prefix — `pr-title.yml` enforces the type
  independently of the subject pattern.
- Subject starts lowercase after the `SMA-543 ` key, satisfying
  `subjectPattern: ^([A-Z]{2,4}-\d+ )?[^A-Z].+$`.
- `providers` is already in `.versionrc`'s `scopeRegex`, so the scope resolves
  from `main` — which is what `pull_request_target` reads.
- No colon inside the subject, so there is nothing for the parser to
  mis-split on.

**PR body:** cite PR **#199**, not `SMA-451`. Any `SMA-###` token other than
this issue's own trips CodeRabbit's "Linked Issues" pre-merge check, and that
is unfixable after the fact.
