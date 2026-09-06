# SMA-616 litellm Blank-`call_id` Handling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `providers-litellm` from dropping a blank-id tool call's name, at both
places a blank `id` is wrongly treated as an identity — the end-of-stream dedup net
and the wire-key choice — and align the documented invariant across all four crates
that state it.

**Architecture:** Two small code changes in one file, both applications of one rule:
*a blank `id` is treated as absent, never as an identity.* First a `!call_id.is_empty()`
short-circuit on the `flush_buffered_names` dedup net (mirroring `openai/chat`), then a
`.filter(|id| !id.is_empty())` on the wire-key match scrutinee. Everything else is
doc-comment alignment across `providers-litellm`, `providers-openai`,
`paigasus-helikon-core`, and the conformance suite.

**Tech Stack:** Rust 2024, `cargo test`, `cargo clippy`, `cargo fmt`, `serde_json` in
tests. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-06-sma-616-litellm-blank-id-flush-guard-design.md`

## Global Constraints

- **Task order is load-bearing.** Task 2's test cannot pass until Task 1's guard is in
  place (it needs *both* fixes). Do not reorder Tasks 1 and 2.
- **Commit prefix:** `<type>(<scope>): SMA-### <message>`, subject lowercase after the
  ticket id. `convco check` runs as a `commit-msg` hook and as a required CI job.
- **Valid scopes are an allowlist.** `.versionrc`'s `scopeRegex` and
  `.github/workflows/pr-title.yml`'s `scopes:` list are kept in sync and contain
  `providers`, `providers-openai`, `providers-anthropic` — but **no
  `providers-litellm`**. litellm work uses the generic `providers` scope (as 9 prior
  commits do). Using `providers-litellm` reddens both the `commits` job and, for the
  PR title, `pr-title`.
- **Every commit message ends with:**

  ```text
  Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
  ```

- **No hand version bumps.** No crate gains public API; core's edit is doc-only.
  release-plz handles the bumps. Do not touch any `Cargo.toml` or `CHANGELOG.md`.
- **No mdBook or README edits.** Consciously out of scope per spec §8.
- **`missing_docs` is `warn` workspace-wide and `cargo doc` runs with
  `RUSTDOCFLAGS=-D warnings`.** Doc edits must stay valid rustdoc — intra-doc links in
  backticks only, no bare `[Foo]` unless the item resolves.
- **Line width:** match the surrounding comment wrapping (~80 columns). `cargo fmt`
  does not reflow comments; keep them tidy by hand.
- **Follow-up ticket:** SMA-619 —
  `https://linear.app/smaschek/issue/SMA-619/litellm-upgrades-a-blank-call-id-after-emission-splitting-one-call`

---

### Task 1: Guard the end-of-stream dedup net against blank `call_id`s

Implements spec §3.1, §3.2, §7.1. Discharges acceptance criteria 1 and 2 for the
index-present shape.

**Files:**

- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` — the `already`
  seed and the `continue` guard inside `flush_buffered_names` (around lines 554-597)
- Test: same file, unit-test module at the bottom, beside
  `blank_ids_do_not_collapse_distinct_calls` (around line 1913)

**Interfaces:**

- Consumes: existing test helpers in that module — `tc_chunk(serde_json::Value) -> serde_json::Value`
  (wraps tool calls into a `choices[0].delta.tool_calls` chunk),
  `drive(&mut ChatTranslator, Vec<serde_json::Value>) -> Vec<ModelEvent>` (feeds every
  chunk through `consume`, then `finish`, collecting all events), and
  `named(&[ModelEvent]) -> Vec<(String, String)>` (`(call_id, name)` for every delta
  carrying `Some(name)`, in order).
- Produces: nothing new. Task 2 relies on this guard already being in place.

- [ ] **Step 1: Write the failing test**

Add at the very end of the unit-test module in
`crates/paigasus-helikon-providers-litellm/src/stream.rs`, immediately after
`blank_ids_do_not_collapse_distinct_calls`:

```rust
    /// Two parallel **zero-argument** blank-id calls must both emit their name
    /// at end-of-stream.
    ///
    /// Zero-argument is load-bearing: with arguments both calls flush
    /// mid-stream and never reach `flush_buffered_names`, so this is the only
    /// shape that exercises the `call_id` dedup net there —
    /// `blank_ids_do_not_collapse_distinct_calls` drives `"arguments": "{}"`
    /// and therefore covers the other path only.
    ///
    /// An unguarded net claims `""` for the first call and silently drops the
    /// second under an `error!` blaming a correlation-keying regression that
    /// did not happen. `openai/chat` carries the same guard (SMA-566).
    #[test]
    fn blank_ids_do_not_collapse_at_end_of_stream() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![tc_chunk(serde_json::json!([
                {"index": 0, "id": "", "function": {"name": "alpha"}},
                {"index": 1, "id": "", "function": {"name": "beta"}}
            ]))],
        );
        assert_eq!(
            named(&evs),
            vec![
                (String::new(), "alpha".to_owned()),
                (String::new(), "beta".to_owned()),
            ],
            "an empty id cannot identify a call, so the dedup net must not claim it"
        );
    }
```

Note there is **no `arguments` key at all** — that is what keeps both names buffered
to end-of-stream.

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p paigasus-helikon-providers-litellm blank_ids_do_not_collapse_at_end_of_stream
```

Expected: **FAIL**, with left-hand value `[("", "alpha")]` against a right-hand
`[("", "alpha"), ("", "beta")]`. The `"beta"` delta is dropped at the `continue` in
`flush_buffered_names`. This is the defect proof required by acceptance criterion 2 —
record the observed output before continuing.

- [ ] **Step 3: Filter the `already` seed**

In `flush_buffered_names`, replace:

```rust
        let mut already: HashSet<String> = self
            .name_emitted
            .keys()
            .filter_map(|k| self.tool_calls.get(k).cloned())
            .collect();
```

with:

```rust
        // Seeded from keys that already emitted, so a call flushed mid-stream
        // cannot be re-emitted here. The `.filter(|c| !c.is_empty())` is
        // redundant on its own: the loop guard further down
        // (`!call_id.is_empty() && !already.insert(...)`) already
        // short-circuits before `insert` ever runs for a blank id, so a blank
        // entry in `already` could never change what gets claimed. Kept anyway
        // so this seed states the same "blank ids are exempt" rule as the
        // guard below, in the same place — and so it matches
        // `providers-openai`'s seed, which states it identically (SMA-616).
        let mut already: HashSet<String> = self
            .name_emitted
            .keys()
            .filter_map(|k| self.tool_calls.get(k))
            .filter(|c| !c.is_empty())
            .cloned()
            .collect();
```

- [ ] **Step 4: Guard the claim**

In the same function, replace the guard line:

```rust
            if !already.insert(call_id.clone()) {
```

with:

```rust
            if !call_id.is_empty() && !already.insert(call_id.clone()) {
```

Then replace the comment block immediately above it (the one beginning "Claimed only
once we know this key actually has a name to flush") with:

```rust
            // Claimed only once we know this key actually has a name to flush
            // — claiming earlier would, were two entries for one call_id ever
            // possible again, let an empty-name entry suppress another that
            // does have one.
            //
            // Unreachable for a non-blank call_id since SMA-550:
            // canonicalization gives each one exactly one pending key. Kept as
            // a net because it enforces the invariant at the point of
            // emission, independent of the keying discipline upstream — which
            // is precisely what the SMA-533 cross-provider suite asserts. Loud
            // rather than a bare `continue`: if the keying is ever loosened, a
            // silent drop here would recreate the exact undiagnosed loss
            // SMA-550 existed to fix.
            //
            // Blank call_ids are excluded deliberately. They bypass
            // canonicalization (an empty id cannot identify a call), so two
            // parallel blank-id calls both resolve to "". Claiming "" here
            // would drop the second call's name and blame a keying regression
            // that did not happen. The at-most-one invariant is therefore
            // scoped to non-blank call_ids; `providers-openai` carries the
            // same guard (SMA-566). Pinned by
            // `blank_ids_do_not_collapse_at_end_of_stream`.
```

Leave the `tracing::error!` call itself unchanged — after the guard it only fires for
a genuine non-blank collision, where its wording is accurate.

- [ ] **Step 5: Run the test to verify it passes**

Run:

```bash
cargo test -p paigasus-helikon-providers-litellm blank_ids_do_not_collapse_at_end_of_stream
```

Expected: **PASS**.

- [ ] **Step 6: Run the whole crate's tests**

Run:

```bash
cargo test -p paigasus-helikon-providers-litellm
```

Expected: all pass, no regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
```

Commit with subject
`fix(providers): SMA-616 exempt blank call_ids from the flush dedup net`
plus the two trailer lines from Global Constraints.

---

### Task 2: Treat a blank `id` as absent when choosing the wire key

Implements spec §3.3, §7.2. Closes the `Key::Id("")` collision that the flush guard
alone cannot reach. **Requires Task 1 to be committed first.**

**Files:**

- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` — the `match`
  scrutinee in `handle_tool_call` (around line 373) and `canonicalize`'s blank-id
  comment (around line 179)
- Test: same file, unit-test module, beside the test added in Task 1

**Interfaces:**

- Consumes: Task 1's guard on the flush net. Without it this task's test still fails
  (the second call would flush into a claimed `""`).
- Produces: nothing new.

- [ ] **Step 1: Write the failing test**

Add immediately after `blank_ids_do_not_collapse_at_end_of_stream`:

```rust
    /// Blank-id calls with **no `index`** must not collide on one wire key.
    ///
    /// The wire key is `(tc.index, tc.id)`, and litellm's `index` is optional
    /// — so before SMA-616 an array of blank-id entries with no index gave
    /// every one of them `Key::Id("")`. They shared a single `Pending` slot,
    /// the SMA-547 whole-name-repeat guard did not fire (`"alpha" != "beta"`),
    /// and the two names concatenated into one `("", "alphabeta")` delta.
    ///
    /// This is a strictly deeper collision than the dedup net's: it happens at
    /// key construction, before `canonicalize` is ever called, so the
    /// `!call_id.is_empty()` guard in `flush_buffered_names` cannot reach it.
    /// The shape is impossible in `openai/chat`, whose `index` is a required
    /// `u32`.
    #[test]
    fn blank_ids_without_index_do_not_collapse() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![tc_chunk(serde_json::json!([
                {"id": "", "function": {"name": "alpha"}},
                {"id": "", "function": {"name": "beta"}}
            ]))],
        );
        assert_eq!(
            named(&evs),
            vec![
                (String::new(), "alpha".to_owned()),
                (String::new(), "beta".to_owned()),
            ],
            "a blank id is not an identity, so it must not become the wire key"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p paigasus-helikon-providers-litellm blank_ids_without_index_do_not_collapse
```

Expected: **FAIL**, with left-hand value `[("", "alphabeta")]` — a *single* delta whose
name is the two names concatenated. Note this is a different failure from Task 1's
(one merged name, not one dropped name); record it before continuing.

- [ ] **Step 3: Filter the match scrutinee**

In `handle_tool_call`, change only the scrutinee:

```rust
        let key = match (tc.index, tc.id.as_deref()) {
```

to:

```rust
        // A blank `id` is not an identity, so it must not become `Key::Id("")`
        // — two such entries would share one slot and merge into a single
        // call. Filtering it out here sends the delta to the same arms that
        // handle an absent id: positional keying, or the loud skip when
        // another entry in this array carries an explicit index. Registration
        // into `tool_calls` below still reads `tc.id` directly, so a blank id
        // is still recorded and still resolves to `""` (SMA-616).
        let key = match (tc.index, tc.id.as_deref().filter(|id| !id.is_empty())) {
```

Leave every match arm unchanged. Do **not** touch the
`if let Some(id) = tc.id.as_deref()` registration further down — a blank id must go on
being recorded in `tool_calls`, which is what makes it resolve to `""` and what keeps
`canonicalize`'s blank-id warning firing.

- [ ] **Step 4: Correct `canonicalize`'s blank-id comment**

The comment currently ends by asserting something that only became true in Step 3.
Replace its last sentence — "Leave such deltas on their wire key, which keeps distinct
calls distinct." — with:

```rust
        // wire key. That keeps distinct calls distinct only because a blank id
        // is filtered out when the wire key is chosen (see `handle_tool_call`)
        // — otherwise two blank-id entries with no `index` would both arrive
        // here already sharing one `Key::Id("")` slot (SMA-616).
```

Adjust the preceding line so the sentence reads continuously; keep the rest of the
comment as-is.

- [ ] **Step 5: Run the test to verify it passes**

Run:

```bash
cargo test -p paigasus-helikon-providers-litellm blank_ids_without_index_do_not_collapse
```

Expected: **PASS**.

- [ ] **Step 6: Verify the change is inert against existing blank-id tests**

Run:

```bash
cargo test -p paigasus-helikon-providers-litellm blank
cargo test -p paigasus-helikon-providers-litellm
```

Expected: all pass. Specifically `a_real_id_replaces_a_blank_one_on_the_same_wire_key`
and `blank_ids_do_not_collapse_distinct_calls` must still pass — both carry an explicit
`index` and so take the unchanged first match arm.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
```

Commit with subject
`fix(providers): SMA-616 treat a blank id as absent when choosing the wire key`
plus the two trailer lines.

---

### Task 3: Scope litellm's documented invariant to non-blank `call_id`s

Implements spec §3.4. Doc-only. Discharges acceptance criterion 3 on the litellm side.

**Files:**

- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` — module doc
  (~line 29), `canonicalize` doc (~line 173), `flush_buffered_names` doc (~line 541)

**Interfaces:**

- Consumes: nothing.
- Produces: nothing. Task 4 mirrors this wording into `openai/chat`.

- [ ] **Step 1: Scope the module doc**

Replace:

```rust
//!    order. One `call_id` therefore owns exactly one state entry, which is
//!    what makes "at most one name-carrying delta per `call_id`" structural
//!    rather than guarded (SMA-550).
```

with:

```rust
//!    order. One `call_id` therefore owns exactly one state entry, which is
//!    what makes "at most one name-carrying delta per non-blank `call_id`"
//!    structural rather than guarded (SMA-550). The qualifier is load-bearing:
//!    a blank `id` is not an identity, so it is filtered out of the wire key
//!    and exempted from the end-of-stream dedup net, and two parallel
//!    blank-id calls therefore emit two names under `""` (SMA-616).
```

- [ ] **Step 2: Scope the `canonicalize` doc**

Replace:

```rust
    /// Every delta for one call — however it was keyed on the wire — shares a
    /// single state entry from here on. That is what makes "at most one
    /// name-carrying `ToolCallDelta` per `call_id`" hold by construction
    /// rather than by guard, and it is what lets a name fragmented across the
    /// `Key::Index` / `Key::Id` boundary reassemble instead of losing a
    /// fragment (SMA-550).
```

with:

```rust
    /// Every delta for one call — however it was keyed on the wire — shares a
    /// single state entry from here on. That is what makes "at most one
    /// name-carrying `ToolCallDelta` per non-blank `call_id`" hold by
    /// construction rather than by guard, and it is what lets a name
    /// fragmented across the `Key::Index` / `Key::Id` boundary reassemble
    /// instead of losing a fragment (SMA-550). Blank ids are excluded: this
    /// function returns them unchanged rather than canonicalizing them, so
    /// they carry no per-`call_id` invariant at all (SMA-616).
```

- [ ] **Step 3: Scope the `flush_buffered_names` doc**

Replace:

```rust
    /// Reached by the zero-argument shape, where no `arguments` fragment ever
    /// arrives to signal the name is complete. Correctness, not diagnostics:
    /// the agent loop dispatches on the presence of an `Item::ToolCall` and
    /// reads the tool to run from its `name`.
    ///
    /// Skips entries whose `id` never resolved (nothing to emit under) and
    /// entries whose resolved `call_id` already emitted a name. Since SMA-550
    /// the latter check is redundant — canonicalization gives each `call_id`
    /// one key — and is kept as a net; see the comment at its `continue`.
```

with:

```rust
    /// Reached by the zero-argument shape, where no `arguments` fragment ever
    /// arrives to signal the name is complete. Correctness, not diagnostics:
    /// the agent loop dispatches on the presence of an `Item::ToolCall` and
    /// reads the tool to run from its `name`.
    ///
    /// Skips entries whose `id` never resolved (nothing to emit under) and
    /// entries whose resolved `call_id` already emitted a name. Since SMA-550
    /// the latter check is redundant — canonicalization gives each `call_id`
    /// one key — and is kept as a net; see the comment at its `continue`.
    ///
    /// The at-most-one invariant the net enforces is scoped to **non-blank**
    /// `call_id`s. A blank id cannot identify a call, so it is never claimed;
    /// two parallel blank-id calls each flush their own name under `""`
    /// (SMA-616).
```

- [ ] **Step 4: Verify docs build clean**

Run:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-litellm --all-features --no-deps
cargo fmt --all -- --check
```

Expected: both clean.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
```

Commit with subject
`docs(providers): SMA-616 scope the at-most-one invariant to non-blank call_ids`
plus the two trailer lines.

---

### Task 4: Align `openai/chat`'s four SMA-616 forward-references

Implements spec §6. Doc-only. Discharges acceptance criterion 3 on the `openai/chat`
side.

**Files:**

- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` — lines ~432
  (seed comment), ~475 (guard comment), ~666 (asymmetry note), ~1768 (test doc)

**Interfaces:**

- Consumes: the wording settled in Task 3.
- Produces: nothing.

- [ ] **Step 1: Confirm the four sites**

Run:

```bash
grep -n "SMA-616" crates/paigasus-helikon-providers-openai/src/backend/chat.rs
```

Expected: exactly four hits (432, 475, 666, 1768). If the count differs, stop and
report — the spec's §6 inventory is then wrong.

- [ ] **Step 2: Rewrite the seed comment (~line 432)**

Replace:

```rust
        // Kept anyway so this seed states the same "blank ids are exempt"
        // rule as the guard below, in the same place — and so it matches
        // `providers-litellm`'s seed once SMA-616 aligns the two (that seed
        // is currently unfiltered).
```

with:

```rust
        // Kept anyway so this seed states the same "blank ids are exempt"
        // rule as the guard below, in the same place — and so it matches
        // `providers-litellm`'s seed, which states it identically (SMA-616).
```

- [ ] **Step 3: Rewrite the guard comment (~line 475)**

Replace:

```rust
            // regression that did not happen. The at-most-one invariant is
            // therefore scoped to non-blank call_ids; `providers-litellm`
            // carries the unguarded version of this net and loses that name
            // today (SMA-616). Pinned by `blank_ids_do_not_collapse_at_end_of_stream`.
```

with:

```rust
            // regression that did not happen. The at-most-one invariant is
            // therefore scoped to non-blank call_ids; `providers-litellm`
            // carries the same guard (SMA-616). Pinned by
            // `blank_ids_do_not_collapse_at_end_of_stream`.
```

Match the exact current wrapping when you edit — reflow the replacement to ~80 columns.

- [ ] **Step 4: Retarget the asymmetry note (~line 666)**

This one does **not** become symmetric present tense — after SMA-616 the dedup-net
asymmetry is gone, but a different one remains. Replace:

```rust
    /// The one remaining asymmetry is deliberate and ticketed: this crate's
    /// end-of-stream dedup net excludes blank `call_id`s, litellm's does not
    /// (SMA-616).
```

with:

```rust
    /// The one remaining asymmetry is deliberate and ticketed: this crate
    /// gates the blank→real `call_id` upgrade on `blank_emitted`, so a call
    /// that has already emitted under `""` keeps the blank rather than
    /// splitting across two ids; litellm upgrades unconditionally (SMA-619).
    /// The end-of-stream dedup net is no longer asymmetric — both crates
    /// exempt blank `call_id`s from it (SMA-616).
```

- [ ] **Step 5: Rewrite the test doc (~line 1768)**

Replace:

```rust
    /// why the net is written against this test rather than the other way
    /// round. `providers-litellm` carries the unguarded version and loses that
    /// name today (SMA-616).
```

with:

```rust
    /// why the net is written against this test rather than the other way
    /// round. `providers-litellm` carries the same guard and a test of the
    /// same name (SMA-616).
```

Leave the following line — "Passes both before and after the fix — a guard, not a
defect proof." — unchanged; it describes this crate's own history and is still true.

- [ ] **Step 6: Verify**

Run:

```bash
grep -n "SMA-616\|SMA-619" crates/paigasus-helikon-providers-openai/src/backend/chat.rs
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-openai --all-features --no-deps
cargo test -p paigasus-helikon-providers-openai
```

Expected: three SMA-616 hits plus one SMA-619 hit; docs clean; all tests pass
(comment-only change, so no behaviour moves).

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs
```

Commit with subject
`docs(providers-openai): SMA-616 align the blank-call_id notes with litellm`
plus the two trailer lines.

---

### Task 5: Scope the core event contract and record the conformance exception

Implements spec §5 and §4.1. Doc-only, two crates.

**Files:**

- Modify: `crates/paigasus-helikon-core/src/model.rs` — `ModelEvent::ToolCallDelta`
  variant doc (~line 183) and its `name` field doc (~line 192)
- Modify: `crates/paigasus-helikon-core/src/agent.rs` — `AgentEvent::ToolCallDelta`'s
  `name` field doc (~line 384)
- Modify: `tests/provider-stream-conformance/src/check.rs` — assertion 7's comment
  block (~lines 75-80)

**Interfaces:**

- Consumes: nothing.
- Produces: nothing. **No API is added** — these are doc comments only, so no version
  bump and no `[workspace.dependencies]` pin change is required.

- [ ] **Step 1: Scope `ModelEvent::ToolCallDelta`'s variant doc**

In `crates/paigasus-helikon-core/src/model.rs`, replace:

```rust
    /// A partial tool call. `name` is `Some` exactly once per `call_id`, on
    /// the first delta for which the provider can establish the name is
    /// complete, and `None` on every other delta. When `Some`, the value is
    /// the whole name so far as the provider can determine — a provider
    /// receiving the name in fragments MUST buffer and concatenate them, and
    /// MUST NOT emit a name it can detect is still incomplete.
```

with:

```rust
    /// A partial tool call. `name` is `Some` exactly once per non-blank
    /// `call_id`, on the first delta for which the provider can establish the
    /// name is complete, and `None` on every other delta. When `Some`, the
    /// value is the whole name so far as the provider can determine — a
    /// provider receiving the name in fragments MUST buffer and concatenate
    /// them, and MUST NOT emit a name it can detect is still incomplete.
    ///
    /// The non-blank qualifier is deliberate. A backend may send `"id": ""`,
    /// and an empty id cannot identify a call — so a provider MUST NOT merge
    /// two parallel blank-id calls, and two such calls therefore each carry a
    /// name under `""`. Consumers that need one entry per call should key on
    /// a non-blank `call_id` and treat `""` as "unidentified".
```

- [ ] **Step 2: Scope the `name` field docs (both crates)**

In the same file, replace the `name` field's doc:

```rust
        /// `Some` exactly once per `call_id`, on the first delta for which
```

with:

```rust
        /// `Some` exactly once per non-blank `call_id`, on the first delta for which
```

Then make the identical one-line change in
`crates/paigasus-helikon-core/src/agent.rs` for `AgentEvent::ToolCallDelta`'s `name`
field. Re-wrap both to ~80 columns after editing.

- [ ] **Step 3: Record the conformance exception**

In `tests/provider-stream-conformance/src/check.rs`, replace:

```rust
    // catch. Groups are built preserving first-seen `call_id` order so the
    // reported violation is deterministic.
```

with:

```rust
    // catch. Groups are built preserving first-seen `call_id` order so the
    // reported violation is deterministic.
    //
    // Known, deliberate exception: a blank `call_id`. An empty id cannot
    // identify a call, so both first-party chat translators refuse to merge
    // parallel blank-id calls and each such call carries its own name under
    // "" (SMA-566, SMA-616). Two of them therefore report `count: 2` here.
    // The assertion is deliberately NOT scoped to non-blank ids: no fixture
    // exercises the shape today, and narrowing a shared cross-provider gate
    // deserves its own decision rather than a drive-by. If you are adding the
    // first blank-id capture and this fires, that is the decision to make.
```

- [ ] **Step 4: Verify**

Run:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
cargo test -p paigasus-helikon-core
cargo test -p provider-stream-conformance
```

Expected: docs clean, all tests pass. If the last package name is not
`provider-stream-conformance`, read it from
`tests/provider-stream-conformance/Cargo.toml` and use that.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/model.rs crates/paigasus-helikon-core/src/agent.rs tests/provider-stream-conformance/src/check.rs
```

Commit with subject
`docs(core): SMA-616 scope the tool-call name contract to non-blank call_ids`
plus the two trailer lines.

---

### Task 6: Full verification sweep

No spec section — this reproduces the CI gates locally before the PR.

**Files:** none modified (fix anything this surfaces in the task that owns it).

- [ ] **Step 1: Run every fast gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: both clean. Note `clippy` may flag the Task 1 seed's `filter_map(...).filter(...)`
chain — if it suggests a merge that would remove the deliberate parallelism with
`openai/chat`, keep the shape and confirm `openai/chat`'s identical chain is also
lint-clean today (it is, so a new lint means the suggestion differs; report rather
than silently diverging).

- [ ] **Step 2: Run the full test suite**

```bash
cargo test --workspace --all-features
```

Expected: all pass.

- [ ] **Step 3: Build the docs**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: clean apart from the known, accepted facade/CLI filename-collision warning
described in `CLAUDE.md`.

- [ ] **Step 4: Check commit messages**

```bash
convco check $(git merge-base origin/main HEAD)..HEAD
```

Expected: clean. The baseline **must** be a merge-base — `convco` silently walks the
whole history when given a branch tip that is not an ancestor.

- [ ] **Step 5: Confirm no stray edits**

```bash
git status --short
git diff origin/main --stat
```

Expected: five source files plus two docs files, and **no** `Cargo.toml`,
`Cargo.lock`, `CHANGELOG.md`, `README.md`, or `docs/book/` changes.

---

## Self-Review

**Spec coverage:**

| Spec section | Task |
|---|---|
| §3.1 guard the claim | Task 1 Step 4 |
| §3.2 filter the seeding | Task 1 Step 3 |
| §3.3 wire-key filter | Task 2 Step 3 |
| §3.4 four litellm doc sites | Task 3 (module, `canonicalize`, `flush_buffered_names` docs) + Task 1 Step 4 (the `continue` comment) |
| §3.4 `canonicalize` blank-id comment | Task 2 Step 4 |
| §3.5 `error!` unchanged | Task 1 Step 4, stated explicitly |
| §4.1 `check.rs` note | Task 5 Step 3 |
| §5 core contract | Task 5 Steps 1-2 |
| §6 four `openai/chat` refs | Task 4 Steps 2-5 |
| §7.1 zero-argument test | Task 1 Steps 1-2 |
| §7.2 index-absent test | Task 2 Steps 1-2 |
| §7.3 existing tests unchanged | Task 2 Step 6, Task 6 Step 2 |
| §8 out of scope | Task 6 Step 5 asserts no `Cargo.toml`/README/book edits |

No gaps.

**Placeholder scan:** none — every code step carries the literal before/after text.

**Type consistency:** the three test helpers (`tc_chunk`, `drive`, `named`) are used
with the signatures documented in Task 1's Interfaces block and match the existing
module. `Key`, `Pending`, and `ChatTranslator` are pre-existing and unmodified. No new
types are introduced by any task.

**Ordering:** Task 1 before Task 2 is asserted in Global Constraints and re-stated in
Task 2's header and Interfaces block, because Task 2's test depends on Task 1's guard.
