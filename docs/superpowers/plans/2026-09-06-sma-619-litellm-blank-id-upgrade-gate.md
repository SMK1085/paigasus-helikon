# SMA-619 litellm Blank-`call_id` Upgrade Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `providers-litellm` from upgrading a blank tool-call `call_id` to a real one after a `ToolCallDelta` has already been emitted under the blank id, which splits one logical call across two `call_id`s and leaves the real one with zero name-carrying deltas.

**Architecture:** Port the `blank_emitted: HashSet<Key>` gate that `openai/chat` has carried since SMA-566. A wire key that has published a delta under `""` is recorded; the blank→real replacement arm is gated on that record; a withheld upgrade is logged once per key at `warn`. The same warn is added to `openai/chat`, which has the identical silent gap, so the two chat translators stay aligned.

**Tech Stack:** Rust 2024, `tracing` for diagnostics, `serde_json` for test fixtures, plain `#[test]` unit tests inside each translator module.

**Spec:** `docs/superpowers/specs/2026-09-06-sma-619-litellm-blank-id-upgrade-gate-design.md`

## Global Constraints

- All line citations in the spec are against `origin/main` at `0ee9af7`. Verify against the working tree before relying on one; the ticket's own `stream.rs:425` citation was stale.
- **MSRV is `1.94`**; workspace inheritance is mandatory. Touch no `Cargo.toml`.
- **No version bumps, no CHANGELOG edits.** release-plz owns both. A hand-bump here would deadlock the publish.
- **Commit format:** `<type>(<scope>): SMA-619 <lowercase message>`. Every commit message ends with the two attribution lines shown in Task 1 Step 5.
- **`missing_docs` is `warn` workspace-wide and `RUSTDOCFLAGS=-D warnings` in CI.** Every new struct field needs a `///` doc comment.
- **Clippy runs as `-D warnings`** with `--all-features --all-targets`. `cargo clippy` must be clean before any commit.
- The `warn!` messages use `target: "paigasus::litellm::stream"` in litellm and `target: "paigasus::openai::chat"` in openai — match each module's existing calls exactly.
- Do not touch `tests/provider-stream-conformance/`, `crates/paigasus-helikon-core/`, or `docs/book/`. Spec §6.2–§6.4 record why each is unchanged.

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/paigasus-helikon-providers-litellm/src/stream.rs` | The `ChatTranslator` being fixed, and its unit tests | Two new fields, one gated match arm, one new match arm with a warn, one `blank_emitted` insert, one test helper, three new tests |
| `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` | The reference translator | One new field, one new match arm with a warn, one doc-comment paragraph rewritten |
| `crates/paigasus-helikon-providers-litellm/README.md` | The crate's crates.io page | One `Limitations` bullet |

Both translator files are large (2039 and 1820 lines) and already organized as one struct plus one `mod tests`. That is the established pattern; do not restructure.

---

### Task 1: Pin the defect with two failing tests

Both new tests must FAIL before any production code changes, proving they test the defect rather than the fix. Spec AC2.

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` (test module, after `blank_ids_without_index_do_not_collapse`, currently ending at line 2038)

**Interfaces:**
- Consumes: existing test helpers `tc_chunk`, `drive`, `named`, `args_of` (`stream.rs:711-760`); `ChatTranslator::new`.
- Produces: `fn accumulated(evs: &[ModelEvent]) -> Result<Vec<(String, String, serde_json::Value)>, String>` — a test helper used by Tasks 1 and 4. Returns each reassembled `Item::ToolCall` as `(call_id, name, args)` in the accumulator's `call_id`-sorted order, or the error string the turn failed with.

- [ ] **Step 1: Add the `accumulated` test helper**

Insert immediately after `args_of` (which ends at `stream.rs:760`), before `fn texts`:

```rust
    /// Reassemble the events through `ModelTurnAccumulator` and return each
    /// resulting `Item::ToolCall` as `(call_id, name, args)`, or the error
    /// string the turn failed with.
    ///
    /// Several assertions in this module decide a trade-off that is only
    /// visible one layer up. The accumulator groups by `call_id` and parses
    /// each group's joined `args_delta`s exactly once, so a translator that
    /// merges two `call_id`s concatenates two argument strings that were
    /// never adjacent — and a parse failure there discards the whole turn,
    /// assistant text included. Asserting only on `named`/`args_of` hides
    /// that entirely (SMA-619 §1.4).
    fn accumulated(
        evs: &[ModelEvent],
    ) -> Result<Vec<(String, String, serde_json::Value)>, String> {
        let mut acc = paigasus_helikon_core::ModelTurnAccumulator::new("test-agent");
        for e in evs {
            acc.observe(e);
        }
        Ok(acc
            .finish()?
            .items
            .into_iter()
            .filter_map(|i| match i {
                paigasus_helikon_core::Item::ToolCall {
                    call_id,
                    name,
                    args,
                } => Some((call_id, name, args)),
                _ => None,
            })
            .collect())
    }
```

- [ ] **Step 2: Write the first failing test**

Append at the end of `mod tests`, after `blank_ids_without_index_do_not_collapse`'s closing brace and before the module's final `}`:

```rust
    /// A real `id` arriving after this wire key already emitted under a blank
    /// one must NOT win — the call would be split across two `call_id`s.
    ///
    /// `canonicalize` treats a blank id as "no identity yet", so the
    /// registration arm lets a real id replace it
    /// (`a_real_id_replaces_a_blank_one_on_the_same_wire_key`). That rule is
    /// right up until a delta has gone out under `""`, and wrong from that
    /// moment on: the name has already been published under the blank id, so
    /// upgrading leaves the real id carrying zero name-carrying deltas. That
    /// is an "exactly once" violation on an id that *can* identify, which is
    /// strictly worse than the stuck blank the rule exists to fix.
    ///
    /// Confirmed to FAIL against the translator as it stood on `main` before
    /// SMA-619, which emits `("", Some("alpha"), "{}")` then
    /// `("c1", None, "[]")` — so `named` passes while `args_of("c1") == "[]"`
    /// and `args_of("") == "{}"` both fail.
    ///
    /// The accumulator assertion records a deliberate trade (SMA-619 §1.4).
    /// `"{}"` followed by `"[]"` is not a fragmentation of anything — it is
    /// two complete JSON documents, and no LiteLLM backend emits it; it is
    /// the marker shape this ticket chose to make the split visible. Keeping
    /// the call whole necessarily joins them into `"{}[]"`, which does not
    /// parse, so the turn now fails loudly where it previously "succeeded"
    /// into two junk items — one named `alpha` under an unsubmittable `""`,
    /// one with an empty name under `c1`. On the shape backends actually send
    /// the fix runs the other way; see `the_gate_fires_on_an_args_only_emission`.
    #[test]
    fn a_real_id_does_not_replace_a_blank_one_after_the_key_emitted() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "", "function": {"name": "alpha", "arguments": "{}"}}
                ])),
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"arguments": "[]"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![(String::new(), "alpha".to_owned())],
            "the name stays under the blank id it was emitted with"
        );
        assert_eq!(
            args_of(&evs, "c1"),
            "",
            "no delta may arrive under the real id, or it would carry no name"
        );
        assert_eq!(
            args_of(&evs, ""),
            "{}[]",
            "every delta for this call stays under the one call_id"
        );
        assert!(
            accumulated(&evs).is_err(),
            "joining two complete JSON documents cannot parse; the turn fails \
             loudly rather than splitting into two junk items"
        );
    }
```

- [ ] **Step 3: Write the second failing test**

Append immediately after the test from Step 2:

```rust
    /// The gate fires on ANY emission under a blank id, not only a
    /// name-carrying one.
    ///
    /// This looks like a simplification opportunity and is not. Here the
    /// first delta carries only `arguments`, so nothing name-carrying has
    /// been published when the real id arrives — yet an ungated upgrade still
    /// tears the arguments JSON across two `call_id`s, leaving `"{\"a\":"`
    /// under `""` and `"1}"` under `"c1"`, neither of which parses. Narrowing
    /// the gate to name-carrying emissions would trade a name-loss corruption
    /// for an arguments-loss corruption. One rule covers both: any published
    /// delta closes the upgrade window.
    ///
    /// This is the shape real backends actually send — arguments fragment as
    /// a partial JSON string — and it is the one where the fix improves the
    /// end-to-end outcome, taking `ModelTurnAccumulator::finish()` from `Err`
    /// to `Ok`.
    ///
    /// Confirmed to FAIL against the translator as it stood on `main` before
    /// SMA-619, on all three event assertions: it emits
    /// `("", None, "{\"a\":")` then `("c1", Some("alpha"), "1}")`.
    #[test]
    fn the_gate_fires_on_an_args_only_emission() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "", "function": {"arguments": "{\"a\":"}}
                ])),
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "alpha", "arguments": "1}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![(String::new(), "alpha".to_owned())],
            "the name arrives under the blank id the call was published with"
        );
        assert_eq!(
            args_of(&evs, "c1"),
            "",
            "no delta may arrive under the real id"
        );
        assert_eq!(
            args_of(&evs, ""),
            "{\"a\":1}",
            "both argument fragments stay under the one call_id"
        );
        assert_eq!(
            accumulated(&evs),
            Ok(vec![(
                String::new(),
                "alpha".to_owned(),
                serde_json::json!({"a": 1})
            )]),
            "keeping the call whole is what makes the arguments parse at all"
        );
    }
```

- [ ] **Step 4: Run both tests and verify they FAIL**

`cargo test` accepts only ONE positional filter, so run them separately:

```bash
cargo test -p paigasus-helikon-providers-litellm --lib after_the_key_emitted
cargo test -p paigasus-helikon-providers-litellm --lib args_only_emission
```

Expected: both FAIL. Read the assertion output and confirm it matches the doc comments — `a_real_id_does_not_replace_a_blank_one_after_the_key_emitted` fails on `args_of(&evs, "c1")` with `left: "[]"`, and `the_gate_fires_on_an_args_only_emission` fails on `named` with `left: [("c1", "alpha")]`.

**If either test PASSES, stop.** A passing test here means it does not exercise the defect, and the rest of the plan is unverifiable. Re-read spec §1 and §3.6.

- [ ] **Step 5: Commit the failing tests**

```bash
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
git commit -F - <<'MSG'
test(providers): SMA-619 pin the blank-id upgrade split

Both tests fail against the current translator: a blank->real call_id
upgrade after emission splits one call across two ids, leaving the real
one with zero name-carrying deltas.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
MSG
```

Committing red is deliberate here — the failing run is the evidence for AC2, and it is only reproducible while the production code is unchanged.

---

### Task 2: Add the gate and make the tests pass

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` — `ChatTranslator` fields (~line 137), `new()` (~line 145), `handle_tool_call`'s registration block (~line 430-452), the emit site (~line 540)

**Interfaces:**
- Consumes: `Key` (`stream.rs:42-62`), `ChatTranslator::tool_calls`.
- Produces: `ChatTranslator::blank_emitted: HashSet<Key>` and `ChatTranslator::warned_withheld_upgrade: HashSet<Key>`, both consumed by Task 3.

- [ ] **Step 1: Add the two fields**

In the `ChatTranslator` struct, immediately after the `warned_blank_id` field (which ends at `stream.rs:137`):

```rust
    /// Wire keys that have already emitted a `ToolCallDelta` while their
    /// `call_id` was still blank.
    ///
    /// Gates the blank-id replacement rule in [`Self::handle_tool_call`].
    /// Once a delta has gone out under `""`, upgrading the key to a real id
    /// would split one call across two `call_id`s and leave the real one with
    /// zero name-carrying deltas — an "exactly once" violation on a
    /// *non-blank* id, which is worse than the stuck blank that rule exists
    /// to fix (SMA-619). `providers-openai`'s chat translator carries the
    /// same gate (SMA-566).
    ///
    /// Structurally `Key::Index`-only: a `Key::Id` is minted only from a
    /// non-blank id, so no `Key::Id` ever resolves to a blank `call_id`.
    /// `Key` is used anyway for type-fit with the maps beside it.
    blank_emitted: HashSet<Key>,
    /// Keys for which the withheld-upgrade warning has already fired, so a
    /// backend that repeats the real id on every delta warns once per call
    /// rather than once per chunk.
    warned_withheld_upgrade: HashSet<Key>,
```

- [ ] **Step 2: Initialize both in `new()`**

In `ChatTranslator::new`, immediately after `warned_blank_id: HashSet::new(),`:

```rust
            blank_emitted: HashSet::new(),
            warned_withheld_upgrade: HashSet::new(),
```

- [ ] **Step 3: Gate the replacement arm**

Replace the whole `if let Some(id) = tc.id.as_deref() { ... }` block in `handle_tool_call` (the one containing `Some(existing) if existing.is_empty() && !id.is_empty()`, at roughly `stream.rs:430-452`) with:

```rust
        // Captured before the match so the guard below reads a plain `bool`
        // rather than borrowing `self` while `tool_calls` is borrowed mutably.
        let blank_already_emitted = self.blank_emitted.contains(&key);

        if let Some(id) = tc.id.as_deref() {
            match self.tool_calls.get_mut(&key) {
                // First id wins, so a backend that changes a call's id
                // mid-stream cannot re-point an in-flight call. The one
                // exception is an id already recorded as empty: `canonicalize`
                // treats a blank id as "no identity yet" rather than as an
                // identity, so a real id arriving later must be allowed to
                // replace it — otherwise the blank sticks and the call reaches
                // the consumer under an empty `call_id` even though the backend
                // eventually supplied a real one.
                //
                // The upgrade is withheld once this key has already emitted a
                // delta under the blank id. Replacing then would split one call
                // across two `call_id`s: the name would have gone out under
                // `""` and every later delta under the real id, leaving the
                // real id with zero name-carrying deltas. That is an "exactly
                // once" violation on a *non-blank* `call_id` — worse than the
                // stuck blank, and one the pre-SMA-619 translator did not have.
                // Keeping the blank keeps the call whole (SMA-619).
                Some(existing)
                    if existing.is_empty() && !id.is_empty() && !blank_already_emitted =>
                {
                    *existing = id.to_owned();
                }
                // Reached only when `blank_already_emitted` blocked the arm
                // above. Warned rather than absorbed into the no-op arm below:
                // this discards a real `call_id` the backend supplied, and the
                // decision to keep the blank is only defensible if it is
                // visible. `canonicalize`'s blank-id warning does not cover it
                // — that one fires on the first delta, before this id was ever
                // seen, and never names it.
                Some(existing) if existing.is_empty() && !id.is_empty() => {
                    if self.warned_withheld_upgrade.insert(key.clone()) {
                        tracing::warn!(
                            target: "paigasus::litellm::stream",
                            ?key,
                            discarded_id = %id,
                            "a real tool-call id arrived after this key already emitted \
                             under a blank id; withholding the upgrade so the call is \
                             not split across two call_ids. The call reaches the \
                             consumer under an empty call_id"
                        );
                    }
                }
                Some(_) => {}
                None => {
                    self.tool_calls.insert(key.clone(), id.to_owned());
                }
            }
        }
```

- [ ] **Step 4: Record the emission**

In `handle_tool_call`, between the "suppress a wholly empty event" early return (`stream.rs:540-542`) and the `out.push(ModelEvent::ToolCallDelta { ... })`:

```rust
        // Record that this key has emitted under a blank id, so the
        // replacement rule above cannot later split the call in two.
        //
        // Placement after the early return is load-bearing: a delta that
        // emits nothing has published the blank id to nobody, so it must not
        // close the upgrade window. Pinned by
        // `a_real_id_replaces_a_blank_one_on_the_same_wire_key`, whose first
        // delta emits nothing and whose upgrade must still be allowed.
        if call_id.is_empty() {
            debug_assert!(
                matches!(key, Key::Index(_)),
                "a blank call_id is only reachable under an Index key"
            );
            self.blank_emitted.insert(key);
        }
```

`key` is moved, not cloned — nothing reads it again before the `out.push`. Unlike `openai/chat`'s `u32`, `Key` is not `Copy`, so a `clone()` here would allocate on every blank-emitting delta. If the compiler rejects the move, the `out.push` below is still reading `key`; check that first rather than reaching for `.clone()`.

- [ ] **Step 5: Run the two new tests and verify they PASS**

```bash
cargo test -p paigasus-helikon-providers-litellm --lib after_the_key_emitted
cargo test -p paigasus-helikon-providers-litellm --lib args_only_emission
```

Expected: both PASS.

- [ ] **Step 6: Run the whole crate and verify nothing regressed**

```bash
cargo test -p paigasus-helikon-providers-litellm
```

Expected: all pass. The baseline before this plan was 157 passing in `--lib`; expect 159 now.

Pay particular attention to `a_real_id_replaces_a_blank_one_on_the_same_wire_key` — spec §5.3. If it fails, the `blank_emitted` insert from Step 4 is on the wrong side of the early return.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
git commit -F - <<'MSG'
fix(providers): SMA-619 gate the blank call_id upgrade on emission

Once a wire key has published a ToolCallDelta under "", the blank->real
upgrade is withheld and warned about once per key, so the call reaches
the consumer whole rather than split across two call_ids with the real
one carrying no name.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
MSG
```

---

### Task 3: Record the `flush_buffered_names` omission

A one-line comment. Folded in here rather than given its own task because it is a note about Task 2's field and cannot be reviewed apart from it.

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` — `flush_buffered_names`'s emission site (~line 642-646)

**Interfaces:**
- Consumes: `blank_emitted` from Task 2. Produces: nothing.

- [ ] **Step 1: Add the comment**

Immediately above the `out.push(ModelEvent::ToolCallDelta { ... })` at the end of `flush_buffered_names`'s loop:

```rust
            // Deliberately does not record into `blank_emitted`, unlike the
            // mid-stream emit site: `finish` is terminal, so no replacement
            // arm can run after this point. Inert even under the double
            // `finish()` that `finish_is_idempotent_after_draining` drives,
            // because the first call drains `pending`.
```

- [ ] **Step 2: Verify it still compiles and passes**

```bash
cargo test -p paigasus-helikon-providers-litellm --lib
```

Expected: 159 pass.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
git commit -F - <<'MSG'
docs(providers): SMA-619 note why the flush site records no emission

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
MSG
```

---

### Task 4: Pin the N>1 parallel blank-call trade

The gate extends a merge that `ModelTurnAccumulator` performs on blank `call_id`s. Spec §2.2 accepts this; the test records that it was seen and decided rather than missed.

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` (test module, after `the_gate_fires_on_an_args_only_emission`)

**Interfaces:**
- Consumes: `accumulated` from Task 1; the gate from Task 2. Produces: nothing.

- [ ] **Step 1: Write the test**

Append after `the_gate_fires_on_an_args_only_emission`:

```rust
    /// Two parallel blank-id calls whose real ids arrive after both emitted
    /// stay merged at the accumulator. Accepted, not overlooked.
    ///
    /// The tie-break in SMA-619 is argued for one call: keeping the blank
    /// keeps the call whole. For two it cuts the other way — both upgrades
    /// are withheld, so all four deltas carry `""` and `ModelTurnAccumulator`
    /// folds them into a single item, where ungated they would have separated
    /// into `c1` and `c2`.
    ///
    /// This is accepted for three reasons. The translator's obligation is at
    /// the event layer and is still met: two name-carrying deltas go out,
    /// `alpha` and `beta`, exactly as SMA-616 requires and as
    /// `blank_ids_do_not_collapse_distinct_calls` asserts. The merge happens
    /// in `ModelTurnAccumulator`, which core documents as deliberately
    /// merging blank-id calls first-name-wins — behaviour this ticket does
    /// not change, only reaches more often. And `openai/chat` has carried the
    /// identical trade since SMA-566, so declining it here would reopen the
    /// asymmetry SMA-619 exists to close.
    ///
    /// The withheld-upgrade `warn!` fires twice here, once per key, which is
    /// what makes the merge diagnosable at all.
    ///
    /// The fixture is three chunks so that the trade is isolated rather than
    /// conflated. The two calls must emit *before* their real ids arrive, or
    /// the gate never engages — but if they emit arguments, those arguments
    /// merge under `""` and fail the turn on their own, pre-fix and post-fix
    /// alike, which would prove nothing about this gate. So chunk 1 buffers
    /// two names, chunk 2 is a bare completion signal that flushes both under
    /// `""` with no arguments at all, and only chunk 3 carries arguments.
    /// Pre-fix that yields three parsing items — `("", "alpha", {})` plus a
    /// nameless `c1` and `c2`; post-fix the gate keeps everything under `""`,
    /// the two argument objects concatenate into `{"p":1}{"q":2}`, and the
    /// turn fails.
    ///
    /// That regression is accepted on the same grounds as
    /// `a_real_id_does_not_replace_a_blank_one_after_the_key_emitted`: the
    /// pre-fix `Ok` is three junk items, two of them nameless and one under
    /// an unsubmittable `""`, and a loud failure beats dispatching those.
    #[test]
    fn withheld_upgrades_keep_parallel_blank_calls_merged() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                // Buffers two names; emits nothing (no completion signal yet).
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "", "function": {"name": "alpha"}},
                    {"index": 1, "id": "", "function": {"name": "beta"}}
                ])),
                // No name fragment = the name is complete. Flushes both under
                // "" with empty args, which is what arms the gate.
                tc_chunk(serde_json::json!([{"index": 0}, {"index": 1}])),
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"arguments": "{\"p\":1}"}},
                    {"index": 1, "id": "c2", "function": {"arguments": "{\"q\":2}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![
                (String::new(), "alpha".to_owned()),
                (String::new(), "beta".to_owned()),
            ],
            "the event-layer rule holds: two blank-id calls, two names"
        );
        assert_eq!(
            args_of(&evs, "c1"),
            "",
            "the withheld upgrade keeps every delta off the real id"
        );
        assert_eq!(
            args_of(&evs, "c2"),
            "",
            "and off the second real id"
        );
        assert!(
            accumulated(&evs).is_err(),
            "the accumulator merges blank call_ids, so two parallel calls' \
             arguments concatenate into invalid JSON and the turn fails — the \
             accepted cost of the gate for N>1 (SMA-619 §2.2)"
        );
    }
```

- [ ] **Step 2: Run it**

```bash
cargo test -p paigasus-helikon-providers-litellm --lib withheld_upgrades_keep_parallel
```

Expected: PASS.

If `named` comes back empty, chunk 2 did not flush — check that its entries carry `index` but no `function` key at all, so both `name_frag` and `args_frag` are `""` and the "no name fragment means the name is complete" signal fires.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
git commit -F - <<'MSG'
test(providers): SMA-619 pin the parallel blank-call merge

The gate withholds both upgrades, so two parallel blank-id calls stay
merged at the accumulator. Accepted per the spec; asserted so the trade
is on the record.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
MSG
```

---

### Task 5: Align `openai/chat` — the parity warn and the doc comment

`openai/chat` has the same silent gate. Spec §4.2: a gate loud in one crate and silent in the other is the drift this ticket closes.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` — fields (~line 287), `new()` (~line 310), the registration match (~line 712-717), the doc comment (~line 662-668)

**Interfaces:**
- Consumes: existing `blank_emitted: HashSet<u32>` (`chat.rs:287`). Produces: `warned_withheld_upgrade: HashSet<u32>`.

- [ ] **Step 1: Add the field**

Immediately after the `blank_emitted` field declaration (ending `chat.rs:287`):

```rust
    /// Wire indices for which the withheld-upgrade warning has already fired,
    /// so a backend that repeats the real id on every delta warns once per
    /// call rather than once per chunk.
    warned_withheld_upgrade: HashSet<u32>,
```

- [ ] **Step 2: Initialize it in `new()`**

Immediately after `blank_emitted: HashSet::new(),`:

```rust
            warned_withheld_upgrade: HashSet::new(),
```

- [ ] **Step 3: Add the warn arm**

In `handle_tool_call_chunk`'s registration match, insert between the gated `Some(existing) if ... && !blank_already_emitted` arm and the bare `Some(_) => {}` arm (i.e. immediately after the closing `}` of the replacement arm at `chat.rs:716`):

```rust
                // Reached only when `blank_already_emitted` blocked the arm
                // above. Warned rather than absorbed into the no-op arm below:
                // this discards a real `call_id` the backend supplied, and the
                // decision to keep the blank is only defensible if it is
                // visible. `canonicalize`'s blank-id warning does not cover it
                // — that one fires on the first delta, before this id was ever
                // seen, and never names it (SMA-619).
                Some(existing) if existing.is_empty() && !id.is_empty() => {
                    if self.warned_withheld_upgrade.insert(index) {
                        tracing::warn!(
                            target: "paigasus::openai::chat",
                            index,
                            discarded_id = %id,
                            "a real tool-call id arrived after this index already emitted \
                             under a blank id; withholding the upgrade so the call is \
                             not split across two call_ids. The call reaches the \
                             consumer under an empty call_id"
                        );
                    }
                }
```

Note this arm reads `index` — the wire index captured at the top of the function, before `canonicalize` shadows it. That is the same value `blank_already_emitted` was read with at `chat.rs:690`, which is what makes the dedup key correct.

- [ ] **Step 4: Rewrite the asymmetry paragraph**

Replace the final paragraph of `handle_tool_call_chunk`'s doc comment — the one beginning "The one remaining asymmetry is deliberate and ticketed" and ending "…exempt blank `call_id`s from it (SMA-616)." — with:

```rust
    /// Both chat translators gate the blank→real `call_id` upgrade on
    /// `blank_emitted`, so a call that has already emitted under `""` keeps
    /// the blank rather than splitting across two ids (SMA-566 here, SMA-619
    /// in litellm), and both warn once when an upgrade is withheld. The
    /// end-of-stream dedup net is likewise symmetric — both exempt blank
    /// `call_id`s from it (SMA-616). No *gating* asymmetry remains.
    ///
    /// Two divergences survive and are deliberate. litellm's `index` is
    /// optional, so its two key spaces admit a cross-key-space split — a
    /// blank-id delta keyed by index followed by an index-less delta keyed by
    /// id — that this crate cannot express at all, `index` being a required
    /// `u32`; unticketed and left as-is. And this crate's sibling `responses`
    /// translator has no blank-id handling whatsoever; its own name-dedup
    /// defect is open as SMA-617.
```

"Both crates" would be wrong: `providers-openai` also contains `responses.rs`, which has no `blank_emitted` at all. Scope the claim to the two *chat* translators, and name the survivors so the next reader does not stop looking.

- [ ] **Step 5: Verify `openai` still passes**

```bash
cargo test -p paigasus-helikon-providers-openai
```

Expected: all pass. No `ModelEvent` changes, and no existing test asserts on log output, so this should be behaviour-neutral.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs
git commit -F - <<'MSG'
fix(providers-openai): SMA-619 warn when a blank call_id upgrade is withheld

chat.rs had the gate since SMA-566 but logged nothing when it fired, so
a discarded real call_id was invisible. Adds the same one-shot warn
litellm now carries, and retargets the asymmetry note now that the
gating difference is closed.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
MSG
```

---

### Task 6: Document the limitation and verify every CI gate

SMA-616's spec deferred this README bullet to "the follow-up ticket that owns it" (`2026-09-06-sma-616-litellm-blank-id-flush-guard-design.md:403-407`). This is that ticket.

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/README.md` — the `Limitations` section, after the "Tool-call names are buffered until complete" bullet ending at line 132

**Interfaces:**
- Consumes: the behaviour from Task 2. Produces: nothing.

- [ ] **Step 1: Add the Limitations bullet**

Append as the last bullet of `## Limitations`, after the existing bullet ending "…already been yielded downstream.":

```markdown
- **A blank tool-call `id` becomes the call's identity once anything is
  emitted under it.** LiteLLM backends may send `"id": ""` on a call's first
  delta and a real id later. If nothing has been emitted yet, the real id
  replaces the blank one and the call arrives under it. If the first delta
  also carried `arguments`, a `ToolCallDelta` has already gone out under
  `""` — the real id is then discarded and logged at `warn`, and the whole
  call is delivered under `call_id: ""`. Upgrading after the fact would split
  one call across two ids and leave the real one with no name, which is worse:
  an agent loop can see that `""` is unusable, but cannot see that two ids are
  really one call.
```

- [ ] **Step 2: Lint the markdown**

```bash
npx markdownlint-cli2
```

Expected: `0 issues`. This needs Node ≥ 20; if it dies with `SyntaxError: Invalid regular expression flags`, the shell's default Node is too old — prefix with a newer one, e.g. `PATH="$HOME/.nvm/versions/node/v24.20.0/bin:$PATH" npx markdownlint-cli2`.

- [ ] **Step 3: Run every local CI gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
bash scripts/check-markdownlint-config.sh
```

All must pass. `cargo doc` matters here: every field added in Tasks 2 and 5 needs its `///` comment, and `-D warnings` is what catches a missing one.

- [ ] **Step 4: Verify the spec's acceptance criteria one by one**

Re-read spec §0 and confirm each of the seven, citing the test or line that satisfies it. AC2 is satisfied by Task 1 Step 4's recorded failure, not by anything in the final tree — say so explicitly rather than re-running it.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-litellm/README.md
git commit -F - <<'MSG'
docs(providers): SMA-619 document the blank call_id limitation

Records the user-visible consequence of the upgrade gate on the crate's
crates.io page, closing the handoff SMA-616's spec made to this ticket.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01LYH3d66YxK8k7wLjRZ8oCY
MSG
```
