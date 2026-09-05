# SMA-566 openai/chat call_id Canonicalization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `openai/chat`'s stream translator emit exactly one name-carrying
`ToolCallDelta` per `call_id`, by aliasing every wire `index` that resolves a given
`call_id` onto the first index that owned it.

**Architecture:** All correlation maps stay keyed by `u32`. One new map,
`canonical: HashMap<String, u32>`, records `non-blank call_id -> owning wire index`.
On resolving a non-blank `id` for wire index `i`, the owning index is
`*canonical.entry(call_id).or_insert(i)`; if it differs from `i`, `pending[i]` migrates
into `pending[owner]` in buffer-creation order and `owner` keys everything downstream.
Blank ids never enter the map and stay on their wire index.

**Tech Stack:** Rust 1.94, `async-openai` 0.41 (`ChatCompletionMessageToolCallChunk`),
`tracing`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-05-sma-566-openai-chat-call-id-canonicalization-design.md`

## Global Constraints

- **Branch:** `feature/sma-566-openaichat-emits-two-name-carrying-deltas-for-one-call_id`,
  already checked out with the spec committed. Do not create a new branch.
- **Commit format:** `<type>(<scope>): SMA-566 <lowercase message>`. Scopes used here:
  `providers-openai`, `providers`, `spec`, `plan`. The subject after `SMA-566` must
  start lowercase. The `commit-msg` hook runs `convco check --from-stdin` and will
  reject anything else.
- **Every task ends green.** `cargo test -p paigasus-helikon-providers-openai` must
  pass before each commit. The 18 pre-existing tests in `chat.rs`'s `mod tests` must
  pass **unmodified** at every step — including the white-box assertions in
  `late_name_fragment_warns_once` (`t.warned_late_name.contains(&0)`,
  `t.name_emitted.get(&0)`). If one breaks, stop: the change altered well-formed
  behaviour and must be re-examined, not accommodated.
- **The `pre-push` hook** runs `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-features --all-targets -- -D warnings`, and `convco check <merge-base>..HEAD`.
  Run fmt and clippy before pushing, not after.
- **No file outside these four may be touched:**
  `crates/paigasus-helikon-providers-openai/src/backend/chat.rs`,
  `crates/paigasus-helikon-providers-litellm/src/stream.rs` (doc-only),
  `tests/provider-stream-conformance/tests/conformance.rs` (doc-only), and this plan.
  In particular: no `Cargo.toml` version bumps (release-plz owns those), no mdBook, no
  crate READMEs — the spec's §5.2 records why.
- **`tracing` target strings are `"paigasus::openai::chat"`** — matching every other
  `tracing` call in the file. Not `paigasus::litellm::stream`.
- **Follow-up tickets already filed**, cite them by number in comments where the plan
  says to: **SMA-616** (litellm's unguarded flush net), **SMA-617** (responses
  `item_id` injectivity gap).

---

## File Structure

Only one file gains code. The other two gain documentation that would otherwise become
false.

| File | Responsibility | Change |
|---|---|---|
| `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` | The Chat Completions backend and its `ChatTranslator`. Already ~1094 lines and cohesive — one struct, its translation logic, and its unit tests. No split warranted. | Code + 12 new tests |
| `crates/paigasus-helikon-providers-litellm/src/stream.rs` | The litellm translator. Its `two_indexes_with_one_id_merge_into_a_single_call` doc asserts a divergence this change removes. | Doc-only |
| `tests/provider-stream-conformance/tests/conformance.rs` | Cross-provider suite. Its `openai_chat` module needs the counterpart to `litellm`'s "regression coverage lives in the crate's own unit tests" note. | Doc-only |

Task order is layered so each task's tests are made to pass by that task's code and
nothing later: scaffolding, then aliasing, then migration, then the loud-drop path,
then blank-id replacement, then the flush net, then docs.

---

### Task 1: Scaffolding — `seq` on `PendingToolCall`, `ensure_pending`, no `Default`

Pure refactor. No behaviour change, no new test. The 18 existing tests are the proof.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs:215-221` (`PendingToolCall`)
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs:227-249` (`ChatTranslator` struct)
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs:251-262` (`new`)
- Modify: the two `self.pending.entry(index).or_default()` call sites in `handle_tool_call_chunk`

**Interfaces:**
- Consumes: nothing.
- Produces: `PendingToolCall { seq: u64, name: String, args: String }`,
  `PendingToolCall::new(seq: u64) -> Self`,
  `ChatTranslator::ensure_pending(&mut self, index: u32)`, field
  `ChatTranslator::next_seq: u64`.

- [x] **Step 1: Replace the `PendingToolCall` definition**

Find the `#[derive(Default)]` struct at `chat.rs:215-221`. Keep its existing doc
comment paragraphs, and append the two new ones. Replace the derive and body:

```rust
/// Both `name` and `args` use `push_str` concatenation so that fragmented
/// deltas (e.g. `"sea"` + `"rch"` → `"search"`) are assembled correctly.
///
/// **No `Default` impl, deliberately.** Every buffer must carry the `seq` it
/// was created with, so construction goes through [`PendingToolCall::new`] via
/// [`ChatTranslator::ensure_pending`]. Deriving `Default` would let an
/// `.or_default()` call site silently mint a buffer with `seq: 0`, which would
/// corrupt the merge order in [`ChatTranslator::canonicalize`] (SMA-566). The
/// absence of the derive is what makes that a compile error rather than a
/// latent bug.
struct PendingToolCall {
    /// Monotonic creation order across all buffers in one stream.
    ///
    /// Its sole consumer is the merge in [`ChatTranslator::canonicalize`].
    /// It is deliberately **not** the end-of-stream flush order — that stays
    /// keyed on the canonical wire `index`, which is the model's declared
    /// call position. `providers-litellm` uses its `seq` for both because its
    /// `index` is optional and may be absent entirely; here it never is.
    seq: u64,
    name: String,
    args: String,
}

impl PendingToolCall {
    /// An empty buffer stamped with its creation order.
    fn new(seq: u64) -> Self {
        Self {
            seq,
            name: String::new(),
            args: String::new(),
        }
    }
}
```

- [x] **Step 2: Add `next_seq` to the struct and `new`**

In `ChatTranslator`, directly after the `pending` field, add:

```rust
    /// Next value handed out by [`Self::ensure_pending`]; never reused.
    next_seq: u64,
```

In `ChatTranslator::new`, after `pending: HashMap::new(),` add:

```rust
            next_seq: 0,
```

- [x] **Step 3: Add `ensure_pending`**

Insert as the first method in `impl ChatTranslator`, directly after `new`:

```rust
    /// Ensure a buffer exists for `index`, stamping a fresh `seq` on creation.
    ///
    /// Deliberately returns nothing rather than `&mut PendingToolCall`:
    /// callers then reach the buffer through `self.pending.get_mut(..)`, which
    /// borrows one field instead of all of `self` and so leaves the
    /// surrounding disjoint-field borrows of `name_emitted` and `tool_calls`
    /// intact.
    fn ensure_pending(&mut self, index: u32) {
        if !self.pending.contains_key(&index) {
            self.pending
                .insert(index, PendingToolCall::new(self.next_seq));
            self.next_seq += 1;
        }
    }
```

- [x] **Step 4: Replace both `.or_default()` call sites**

There are exactly two in `handle_tool_call_chunk`. The first is in the no-id-yet
branch:

```rust
            // No call_id yet — buffer both fields so neither is dropped.
            let entry = self.pending.entry(index).or_default();
            entry.name.push_str(name_frag);
            entry.args.push_str(args_frag);
            return;
```

becomes:

```rust
            // No call_id yet — buffer both fields so neither is dropped.
            self.ensure_pending(index);
            let entry = self
                .pending
                .get_mut(&index)
                .expect("ensure_pending just inserted this index");
            entry.name.push_str(name_frag);
            entry.args.push_str(args_frag);
            return;
```

The second is after the `already_emitted` binding:

```rust
        let entry = self.pending.entry(index).or_default();
```

becomes:

```rust
        self.ensure_pending(index);
        let entry = self
            .pending
            .get_mut(&index)
            .expect("ensure_pending just inserted this index");
```

- [x] **Step 5: Verify nothing changed behaviourally**

Run: `cargo test -p paigasus-helikon-providers-openai`
Expected: PASS. All 18 `backend::chat::tests::*` green, unmodified.

If `late_name_fragment_warns_once` fails, stop — that is the signal this task was
supposed to preserve.

- [x] **Step 6: Lint and commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs
git commit -m "refactor(providers-openai): SMA-566 stamp tool-call buffers with a creation order"
```

---

### Task 2: The alias map, `canonicalize`, and the acceptance-criterion test

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` (`ChatTranslator` struct, `new`, `handle_tool_call_chunk`, new `canonicalize`)
- Test: same file, `mod tests`

**Interfaces:**
- Consumes: `ensure_pending`, `PendingToolCall::new` (Task 1).
- Produces: fields `canonical: HashMap<String, u32>` and `warned_blank_id: HashSet<u32>`;
  method `ChatTranslator::canonicalize(&mut self, index: u32, call_id: &str) -> u32`;
  test helpers `drive(&mut ChatTranslator, Vec<ChatCompletionMessageToolCallChunk>) -> Vec<ModelEvent>`,
  `named(&[ModelEvent]) -> Vec<(String, String)>`, `args_of(&[ModelEvent], &str) -> String`.

- [x] **Step 1: Write the test helpers and the failing acceptance test**

Add to `mod tests`, directly after the existing `make_chunk` helper:

```rust
    /// Drive every chunk through `handle_tool_call_chunk`, then `finish`,
    /// collecting all events.
    ///
    /// Collecting across both is required: the invariant is per-stream, and an
    /// assertion made only over `finish()` passes vacuously against the
    /// pre-fix translator, whose violation is two *mid-stream* emissions.
    fn drive(
        t: &mut ChatTranslator,
        chunks: Vec<ChatCompletionMessageToolCallChunk>,
    ) -> Vec<ModelEvent> {
        let mut out = Vec::new();
        for c in chunks {
            t.handle_tool_call_chunk(&c, &mut out);
        }
        out.extend(t.finish());
        out
    }

    /// `(call_id, name)` for every delta carrying `Some(name)`, in order.
    fn named(evs: &[ModelEvent]) -> Vec<(String, String)> {
        evs.iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name: Some(n),
                    ..
                } => Some((call_id.clone(), n.clone())),
                _ => None,
            })
            .collect()
    }

    /// Concatenated `args_delta` across every delta carrying `want`, in order.
    fn args_of(evs: &[ModelEvent], want: &str) -> String {
        evs.iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    args_delta,
                    ..
                } if call_id == want => Some(args_delta.as_str()),
                _ => None,
            })
            .collect()
    }
```

Then append the three tests for this task at the end of `mod tests`:

```rust
    /// SMA-566 acceptance criterion: two deltas carrying different `index`
    /// values but the same `id` are one call, and yield one name.
    ///
    /// The merged name `alphabeta` is not "correct" in any deep sense — the
    /// input is malformed, since an `id` identifies a call — but it is one
    /// name for one `call_id`, which is the invariant under test, and it is
    /// character-for-character what `providers-litellm` emits for this input
    /// (`stream.rs`'s `two_indexes_with_one_id_merge_into_a_single_call`).
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("beta")` from `handle_tool_call_chunk` and then `Some("alpha")`
    /// from `finish`.
    #[test]
    fn two_indexes_with_one_id_merge_into_a_single_call() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some("c1"), Some("alpha"), None),
                make_chunk(1, Some("c1"), Some("beta"), Some("{}")),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "alphabeta".to_owned())],
            "exactly one name-carrying delta per call_id, across both entry points"
        );
        assert_eq!(
            args_of(&evs, "c1"),
            "{}",
            "merging the names must not swallow the args"
        );
    }

    /// The same invariant with the flush happening mid-stream rather than at
    /// `finish`, so the test cannot pass for the wrong reason.
    ///
    /// The expected name is `get_`, **not** `get_weather`: the first delta
    /// carries arguments, so the name flushes immediately and is already
    /// downstream when the second index aliases in. The migrating fragment
    /// cannot be recovered (design spec §4 row 1b).
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("get_")` and then `Some("weather")`.
    #[test]
    fn dual_index_call_emits_at_most_one_name_mid_stream() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some("c1"), Some("get_"), Some("{")),
                make_chunk(1, Some("c1"), Some("weather"), Some("}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "get_".to_owned())]);
        assert_eq!(
            args_of(&evs, "c1"),
            "{}",
            "suppressing the second name must not swallow its args"
        );
    }

    /// A blank `id` is not an identity. Canonicalizing on it would collapse
    /// every parallel blank-id call into one slot and all but the first would
    /// vanish — strictly worse than the defect being fixed. Such deltas stay
    /// on their wire index instead.
    ///
    /// Passes against the pre-fix translator too: index keying already keeps
    /// them distinct. This is a regression guard for the alias map, not a
    /// defect proof.
    #[test]
    fn blank_ids_do_not_collapse_distinct_calls() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some(""), Some("alpha"), Some("{}")),
                make_chunk(1, Some(""), Some("beta"), Some("{}")),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![
                (String::new(), "alpha".to_owned()),
                (String::new(), "beta".to_owned()),
            ],
            "two blank-id calls must both emit; an empty id cannot merge them"
        );
    }
```

- [x] **Step 2: Run the tests to verify the two defect proofs fail**

Run: `cargo test -p paigasus-helikon-providers-openai backend::chat::tests`
Expected: FAIL — `two_indexes_with_one_id_merge_into_a_single_call` with
`left: [("c1", "beta"), ("c1", "alpha")]`, and
`dual_index_call_emits_at_most_one_name_mid_stream` with
`left: [("c1", "get_"), ("c1", "weather")]`.
`blank_ids_do_not_collapse_distinct_calls` PASSES already — expected, it is a guard.

Record the two observed `left:` values; they are what the doc comments claim.

- [x] **Step 3: Add the two new fields**

In `ChatTranslator`, after the `tool_calls` field:

```rust
    /// Non-blank call_id → the wire index that owns its correlation state.
    ///
    /// The first index to resolve a given call_id becomes its owner; every
    /// later index resolving the same call_id aliases onto it, so one
    /// `call_id` owns exactly one entry in `pending`, `name_emitted` and
    /// `warned_late_name`. Blank ids are never inserted — see
    /// [`Self::canonicalize`].
    canonical: HashMap<String, u32>,
```

After `warned_late_name`:

```rust
    /// Wire indices for which the blank-id warning has already fired, so a
    /// backend that sends `"id": ""` on every delta warns once per call
    /// rather than once per chunk.
    warned_blank_id: HashSet<u32>,
```

In `new`, add `canonical: HashMap::new(),` and `warned_blank_id: HashSet::new(),`.

- [x] **Step 4: Add `canonicalize` (aliasing only, no migration yet)**

Insert directly before `handle_tool_call_chunk`:

```rust
    /// Resolve the wire `index` to the canonical index owning `call_id`.
    ///
    /// The first index to resolve a given `call_id` owns its state; every
    /// later index resolving the same `call_id` aliases onto it. That is what
    /// makes "exactly one name-carrying `ToolCallDelta` per `call_id`" hold by
    /// construction rather than by guard (SMA-566).
    ///
    /// `providers-litellm` achieves the same property with a `Key { Index, Id }`
    /// enum, because its `index` is optional and one call can arrive under two
    /// different key spaces. Here `ChatCompletionMessageToolCallChunk::index`
    /// is a required `u32`, so there is only ever one key space and
    /// canonicalization is a many-to-one map *within* it. The two crates agree
    /// observably and differ structurally, for that reason.
    fn canonicalize(&mut self, index: u32, call_id: &str) -> u32 {
        // An empty `id` is not an identity. A backend that sends `"id": ""` on
        // every entry would otherwise collapse every one of its parallel calls
        // into a single slot, and all but the first would vanish from the
        // stream entirely — a strictly worse outcome than the dual-index
        // keying this function exists to fix. Leave such deltas on their wire
        // index, which keeps distinct calls distinct.
        if call_id.is_empty() {
            if self.warned_blank_id.insert(index) {
                tracing::warn!(
                    target: "paigasus::openai::chat",
                    index,
                    "tool-call delta carries an empty id; correlating by wire index \
                     instead, since an empty id cannot identify a call"
                );
            }
            return index;
        }

        *self.canonical.entry(call_id.to_owned()).or_insert(index)
    }
```

- [x] **Step 5: Call it from `handle_tool_call_chunk`**

Immediately after the `let call_id = if let Some(id) = self.tool_calls.get(&index) { ... }`
block that resolves `call_id` (the block ending with the `};` before the late-name
warn), insert:

```rust
        // From here on, one call_id owns exactly one state entry.
        let index = self.canonicalize(index, &call_id);
```

The shadowing `let index` is intentional: every use below — the late-name warn, the
`already_emitted` check, `ensure_pending`, the flush condition, `name_emitted` — must
read the canonical index, not the wire one.

- [x] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-providers-openai`
Expected: PASS — all 18 pre-existing plus the 3 new.

- [x] **Step 7: Lint and commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs
git commit -m "fix(providers-openai): SMA-566 alias tool-call indexes onto the owning call_id"
```

---

### Task 3: Buffer migration and merge order

Task 2 fixes the shape where nothing was buffered under the second index. This task
handles the shape where something was.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` (`canonicalize`)
- Test: same file, `mod tests`

**Interfaces:**
- Consumes: `canonicalize`, `ensure_pending`, `PendingToolCall::seq` (Tasks 1-2).
- Produces: no new signatures — `canonicalize` gains a migration body.

- [x] **Step 1: Write the four failing tests**

Append to `mod tests`:

```rust
    /// A fragment buffered under a second index, before that index's `id`
    /// resolved, must survive the alias rather than being stranded.
    ///
    /// This is the *prepend* branch of the merge: the migrating buffer was
    /// created first, so its fragment belongs in front. Its mirror is
    /// `owner_index_buffered_first_appends_on_merge`; a naive unconditional
    /// append yields `"alphabeta"` here.
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("beta")` and then `Some("alpha")`.
    #[test]
    fn fragment_buffered_under_a_second_index_is_not_stranded() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(1, None, Some("beta"), None),
                make_chunk(0, Some("c1"), Some("alpha"), None),
                make_chunk(1, Some("c1"), None, Some("{}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "betaalpha".to_owned())]);
    }

    /// The *append* branch, mirror of the test above: here the canonical slot
    /// was created first, so the migrating fragment belongs behind it. A naive
    /// unconditional prepend yields `"betaalpha"` here.
    ///
    /// Together these two are why `PendingToolCall` carries a `seq`: both
    /// orderings are reachable, and a plain prepend or a plain append is wrong
    /// in exactly one of them.
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("beta")` and then `Some("alpha")`.
    #[test]
    fn owner_index_buffered_first_appends_on_merge() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some("c1"), Some("alpha"), None),
                make_chunk(1, None, Some("beta"), None),
                make_chunk(1, Some("c1"), None, Some("{}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "alphabeta".to_owned())]);
    }

    /// A migrating fragment identical to what the canonical slot already holds
    /// is a whole-name repeat, not a continuation — the same case the SMA-547
    /// wire-path guard handles, for the same reason. Without this, a backend
    /// that resends the complete name under a second index yields
    /// `"searchsearch"`, which resolves to no registered tool.
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("search")` twice.
    #[test]
    fn repeated_whole_name_is_not_doubled_across_the_alias_boundary() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(1, None, Some("search"), None),
                make_chunk(0, Some("c1"), Some("search"), None),
                make_chunk(1, Some("c1"), None, Some("{}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "search".to_owned())]);
    }

    /// Pins `slot.seq = old.seq` in `canonicalize`. When the migrating buffer
    /// is the older one, the canonical slot must inherit its creation order —
    /// otherwise a *third* index aliasing into the same call merges on the
    /// wrong side.
    ///
    /// Without that line the third merge prepends instead of appending and
    /// yields `"GBAx"`.
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits three
    /// separate names.
    #[test]
    fn migrated_buffer_donates_its_creation_order() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(1, None, Some("B"), None),
                make_chunk(2, None, Some("G"), None),
                make_chunk(0, Some("c1"), Some("A"), None),
                make_chunk(1, Some("c1"), Some("x"), None),
                make_chunk(2, Some("c1"), None, Some("{}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "BAxG".to_owned())]);
    }

    /// The accepted residual: when two indexes for one call interleave at
    /// *fragment* level, no buffer-level order reconstructs the wire sequence,
    /// so the merge misorders the fragments — though it loses none of them.
    ///
    /// Wire order is `AA BB CC DD`; the merge yields `AADDBBCC`. This is
    /// pinned deliberately rather than left undefined, matching
    /// `providers-litellm`'s `interleaved_dual_keying_is_lossless_and_misordered`.
    /// It is still strictly better than the pre-fix translator, which emits
    /// two separate names. Do not "fix" the misordering without adding
    /// per-fragment sequencing and re-deciding the trade-off.
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("AADD")` and then `Some("BBCC")`.
    #[test]
    fn interleaved_aliasing_is_lossless_and_misordered() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(1, None, Some("AA"), None),
                make_chunk(0, Some("c1"), Some("BB"), None),
                make_chunk(0, None, Some("CC"), None),
                make_chunk(1, None, Some("DD"), None),
                make_chunk(1, Some("c1"), None, Some("{}")),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "AADDBBCC".to_owned())]);
    }
```

- [x] **Step 2: Run to verify all five fail**

Run: `cargo test -p paigasus-helikon-providers-openai backend::chat::tests`
Expected: FAIL on all five new tests. Record each observed `left:` value and confirm
it matches the doc comment; correct the comment if it does not.

- [x] **Step 3: Add the migration body to `canonicalize`**

Replace the final line of `canonicalize` (`*self.canonical.entry(...).or_insert(index)`)
with:

```rust
        let owner = *self.canonical.entry(call_id.to_owned()).or_insert(index);
        if owner == index {
            // The common path: this index owns the call. Every well-formed
            // stream takes it on every delta, and no migration ever runs.
            return index;
        }

        let Some(old) = self.pending.remove(&index) else {
            return owner;
        };
        self.ensure_pending(owner);
        let slot = self
            .pending
            .get_mut(&owner)
            .expect("ensure_pending just inserted this index");

        // A resolved slot drains `args` on every delta and is removed once
        // both fields are empty, so `slot.args` is always empty here — which
        // makes `insert_str(0, ..)` an assignment in practice rather than a
        // splice. The assert does not guard correctness: `insert_str(0, ..)`
        // stays order-preserving either way. It exists so that a future
        // relaxation of drain-once surfaces here for a deliberate
        // re-decision.
        debug_assert!(
            slot.args.is_empty(),
            "a resolved slot drains its args on every delta"
        );
        slot.args.insert_str(0, &old.args);

        // A migrating fragment identical to what the canonical slot already
        // holds is a whole-name repeat, not a continuation — the same case the
        // wire path guards, for the same reason: a backend that resends the
        // complete name under a second index would otherwise get
        // "search" + "search" -> "searchsearch".
        if !old.name.is_empty() && old.name != slot.name {
            if !slot.name.is_empty() {
                tracing::warn!(
                    target: "paigasus::openai::chat",
                    %call_id,
                    "tool-call name fragments for one call arrived under two wire \
                     indexes; merging in buffer-creation order, which may misorder \
                     them if the two indexes interleaved"
                );
            }
            // Order by creation `seq`, not by which buffer is migrating. Both
            // orderings are reachable, and a plain prepend or a plain append
            // is wrong in exactly one of them.
            if old.seq < slot.seq {
                slot.name.insert_str(0, &old.name);
            } else {
                slot.name.push_str(&old.name);
            }
        }
        // Claimed whenever the migrating buffer is the older one, even if its
        // name was a repeat or empty: `seq` must stay accurate for any
        // subsequent merge into this same slot.
        if old.seq < slot.seq {
            slot.seq = old.seq;
        }
        owner
```

- [x] **Step 4: Run to verify they pass**

Run: `cargo test -p paigasus-helikon-providers-openai`
Expected: PASS — 18 pre-existing + 8 new.

- [x] **Step 5: Lint and commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs
git commit -m "fix(providers-openai): SMA-566 migrate buffered fragments in creation order"
```

---

### Task 4: Report, never strand, a fragment migrating into an emitted slot

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` (`canonicalize`)
- Test: same file, `mod tests`

**Interfaces:**
- Consumes: `canonicalize`'s migration body (Task 3).
- Produces: no new signatures.

- [x] **Step 1: Write the failing test**

Append to `mod tests`:

```rust
    /// A fragment migrating into a slot that has already emitted its name
    /// cannot reach a consumer — the event is downstream and the flush
    /// condition will not fire again for this call. It must be dropped
    /// *loudly* and recorded, never silently: a silent drop here is exactly
    /// the undiagnosed loss SMA-550 existed to eliminate.
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("get_")` and then `Some("beta")`.
    #[test]
    fn fragment_migrating_into_an_emitted_slot_is_recorded_not_stranded() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(1, None, Some("beta"), None),
                make_chunk(0, Some("c1"), Some("get_"), Some("{}")),
                make_chunk(1, Some("c1"), None, None),
            ],
        );
        assert_eq!(named(&evs), vec![("c1".to_owned(), "get_".to_owned())]);
        assert!(
            t.warned_late_name.contains(&0),
            "the unrecoverable fragment must be recorded against the canonical index"
        );
    }
```

- [x] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-providers-openai backend::chat::tests::fragment_migrating_into_an_emitted_slot_is_recorded_not_stranded`
Expected: FAIL with `left: [("c1", "get_"), ("c1", "beta")]`.

- [x] **Step 3: Add the loud-drop branch**

In `canonicalize`, between the `slot.args.insert_str(0, &old.args);` line and the
whole-name-repeat guard, insert:

```rust
        // The canonical slot has already emitted its name, so this fragment
        // can never reach a consumer: the flush condition tests
        // `name_emitted` and will not fire again for this call. It must not be
        // left sitting in `pending` either — `flush_buffered_names` skips
        // entries whose index already emitted. Dropping it silently would
        // recreate the undiagnosed loss SMA-550 exists to eliminate, so drop
        // it loudly and record it the same way a late wire fragment is
        // recorded.
        if !old.name.is_empty() {
            if let Some(emitted) = self.name_emitted.get(&owner) {
                if self.warned_late_name.insert(owner) {
                    tracing::warn!(
                        target: "paigasus::openai::chat",
                        %call_id,
                        fragment = %old.name,
                        emitted = %emitted,
                        "tool-call name fragment buffered under another wire index \
                         arrived after the name was emitted; it cannot be recovered \
                         and is dropped"
                    );
                }
                if old.seq < slot.seq {
                    slot.seq = old.seq;
                }
                return owner;
            }
        }
```

The `slot.seq` claim is repeated here rather than falling through, because this branch
returns early. `slot` borrows `self.pending`; `self.name_emitted` and
`self.warned_late_name` are disjoint fields, so the borrow checker accepts all three
in scope at once — this is why `ensure_pending` returns `()` rather than a reference.

- [x] **Step 4: Run to verify it passes**

Run: `cargo test -p paigasus-helikon-providers-openai`
Expected: PASS — 18 + 9.

- [x] **Step 5: Lint and commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs
git commit -m "fix(providers-openai): SMA-566 report a fragment migrating into an emitted slot"
```

---

### Task 5: A real `id` replaces a blank one on the same wire index

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` (`handle_tool_call_chunk`, id-resolution chain)
- Test: same file, `mod tests`

**Interfaces:**
- Consumes: `canonicalize` (Tasks 2-4), `ensure_pending` (Task 1).
- Produces: no new signatures — the id-resolution chain is restructured in place.

- [x] **Step 1: Write the failing test**

Append to `mod tests`:

```rust
    /// A real `id` arriving after a blank one on the same wire index must win.
    ///
    /// Registration is otherwise first-id-wins, so a backend that changes a
    /// call's id mid-stream cannot re-point an in-flight call. A blank id is
    /// the one exception: `canonicalize` treats it as "no identity yet" rather
    /// than as an identity, so a real id arriving later must be allowed to
    /// replace it — otherwise the blank sticks and the call reaches the
    /// consumer under an empty `call_id` the agent loop cannot submit a
    /// result against.
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `call_id: ""`.
    #[test]
    fn a_real_id_replaces_a_blank_one_on_the_same_wire_index() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some(""), Some("foo"), None),
                make_chunk(0, Some("c1"), None, Some("{}")),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "foo".to_owned())],
            "the real id must replace the blank one, and the buffered name must survive"
        );
    }
```

- [x] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-providers-openai backend::chat::tests::a_real_id_replaces_a_blank_one_on_the_same_wire_index`
Expected: FAIL with `left: [("", "foo")]`.

- [x] **Step 3: Restructure the id-resolution chain**

Replace this block in `handle_tool_call_chunk`:

```rust
        // Resolve or register the call_id.
        let call_id = if let Some(id) = self.tool_calls.get(&index) {
            id.clone()
        } else if let Some(id) = tc.id.as_deref() {
            self.tool_calls.insert(index, id.to_owned());
            id.to_owned()
        } else {
            // No call_id yet — buffer both fields so neither is dropped.
            self.ensure_pending(index);
            let entry = self
                .pending
                .get_mut(&index)
                .expect("ensure_pending just inserted this index");
            entry.name.push_str(name_frag);
            entry.args.push_str(args_frag);
            return;
        };
```

with registration separated from resolution:

```rust
        // Register or replace the call_id for this wire index.
        if let Some(id) = tc.id.as_deref() {
            match self.tool_calls.get_mut(&index) {
                // First id wins, so a backend that changes a call's id
                // mid-stream cannot re-point an in-flight call. The one
                // exception is an id already recorded as empty: `canonicalize`
                // treats a blank id as "no identity yet" rather than as an
                // identity, so a real id arriving later must be allowed to
                // replace it — otherwise the blank sticks and the call reaches
                // the consumer under an empty `call_id` even though the
                // backend eventually supplied a real one.
                Some(existing) if existing.is_empty() && !id.is_empty() => {
                    *existing = id.to_owned();
                }
                Some(_) => {}
                None => {
                    self.tool_calls.insert(index, id.to_owned());
                }
            }
        }

        let Some(call_id) = self.tool_calls.get(&index).cloned() else {
            // No call_id yet — buffer both fields so neither is dropped.
            self.ensure_pending(index);
            let entry = self
                .pending
                .get_mut(&index)
                .expect("ensure_pending just inserted this index");
            entry.name.push_str(name_frag);
            entry.args.push_str(args_frag);
            return;
        };
```

The `let index = self.canonicalize(index, &call_id);` line added in Task 2 stays
immediately below this, unchanged.

- [x] **Step 4: Run to verify it passes**

Run: `cargo test -p paigasus-helikon-providers-openai`
Expected: PASS — 18 + 10.

- [x] **Step 5: Lint and commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs
git commit -m "fix(providers-openai): SMA-566 let a real tool-call id replace a blank one"
```

---

### Task 6: The end-of-stream dedup net, guarded on non-blank ids

**Order matters here.** The guard test is written and passing *before* the net is
added, because an unguarded net breaks it. Writing the net first and the test second
is how the bug ships.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` (`flush_buffered_names`)
- Test: same file, `mod tests`

**Interfaces:**
- Consumes: everything from Tasks 1-5.
- Produces: no new signatures.

- [x] **Step 1: Write both guard tests — they must PASS immediately**

Append to `mod tests`:

```rust
    /// Two parallel **zero-argument** blank-id calls must both emit their name
    /// at end-of-stream.
    ///
    /// Zero-argument is load-bearing: with arguments both calls flush
    /// mid-stream and never reach `flush_buffered_names`, so this is the only
    /// shape that exercises the call_id dedup net there. An unguarded net
    /// claims `""` for the first call and silently drops the second, which is
    /// why the net is written against this test rather than the other way
    /// round. `providers-litellm` carries the unguarded version and loses that
    /// name today (SMA-616).
    ///
    /// Passes both before and after the fix — a guard, not a defect proof.
    #[test]
    fn blank_ids_do_not_collapse_at_end_of_stream() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some(""), Some("alpha"), None),
                make_chunk(1, Some(""), Some("beta"), None),
            ],
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

    /// End-of-stream flush order follows the wire `index` — the model's
    /// declared call position — not the lexicographic order of `call_id`.
    ///
    /// Passes both before and after the fix. It pins the decision to keep
    /// `flush_buffered_names`'s index sort: aliasing could have moved
    /// `pending` onto a synthetic creation counter (as `providers-litellm`
    /// does, because its `index` is optional), which would silently reorder
    /// parallel calls whenever arrival order differed from index order.
    #[test]
    fn flush_order_follows_the_wire_index() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                make_chunk(0, Some("c_z"), Some("zulu"), None),
                make_chunk(1, Some("c_a"), Some("alpha"), None),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![
                ("c_z".to_owned(), "zulu".to_owned()),
                ("c_a".to_owned(), "alpha".to_owned()),
            ],
            "wire order (index 0 then 1), not lexicographic by call_id"
        );
    }
```

- [x] **Step 2: Run to confirm both pass**

Run: `cargo test -p paigasus-helikon-providers-openai backend::chat::tests`
Expected: PASS — 18 + 12. Both new tests green.

- [x] **Step 3: Add the net to `flush_buffered_names`**

After `indices.sort_unstable();` add the seeding:

```rust
        // Seeded from indexes that already emitted, so a call flushed
        // mid-stream cannot be re-emitted here under a second index. Blank
        // call_ids are filtered out for the same reason they are excluded
        // below.
        let mut already: HashSet<String> = self
            .name_emitted
            .keys()
            .filter_map(|i| self.tool_calls.get(i))
            .filter(|c| !c.is_empty())
            .cloned()
            .collect();
```

Then inside the loop, directly after the `if entry.name.is_empty() { continue; }`
check and before `let name = std::mem::take(&mut entry.name);`, insert:

```rust
            // Claimed only once this index is known to have a name to flush —
            // claiming earlier would let an empty-name entry suppress another
            // that does have one.
            //
            // Unreachable for a non-blank call_id since SMA-566: aliasing
            // gives each one exactly one pending index. Kept as a net because
            // it enforces the invariant at the point of emission, independent
            // of the keying discipline upstream — which is precisely what the
            // SMA-533 cross-provider suite asserts. Loud rather than a bare
            // `continue`: if the keying is ever loosened, a silent drop here
            // would recreate the exact undiagnosed loss SMA-550 existed to fix.
            //
            // Blank call_ids are excluded deliberately. They bypass
            // canonicalization (an empty id cannot identify a call), so two
            // parallel blank-id calls both resolve to "". Claiming "" here
            // would drop the second call's name and blame a keying regression
            // that did not happen. The at-most-one invariant is therefore
            // scoped to non-blank call_ids; `providers-litellm` carries the
            // unguarded version of this net and loses that name today
            // (SMA-616). Pinned by `blank_ids_do_not_collapse_at_end_of_stream`.
            if !call_id.is_empty() && !already.insert(call_id.clone()) {
                tracing::error!(
                    target: "paigasus::openai::chat",
                    %call_id,
                    index,
                    "two pending indexes resolved to one call_id after \
                     canonicalization; dropping the second buffered name. This is \
                     a correlation-keying regression, not a backend quirk"
                );
                continue;
            }
```

Note the borrow: `entry` is a `&mut` into `self.pending` and `already` is a local, so
no conflict. If the borrow checker objects, move the `already` check above the
`self.pending.get_mut(&index)` binding and re-read `entry.name.is_empty()` through
`self.pending[&index].name` instead.

- [x] **Step 4: Run to verify everything still passes**

Run: `cargo test -p paigasus-helikon-providers-openai`
Expected: PASS — 18 + 12. In particular
`blank_ids_do_not_collapse_at_end_of_stream` must still be green; if it now fails, the
`!call_id.is_empty() &&` guard was omitted.

- [x] **Step 5: Lint and commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs
git commit -m "fix(providers-openai): SMA-566 net duplicate flushed names at end of stream"
```

---

### Task 7: Retire the divergence documentation at all three sites

Three comments currently assert that `openai/chat` and `providers-litellm` diverge
here. All three become false with Task 6 merged.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` (the `handle_tool_call_chunk` doc comment)
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` (the `two_indexes_with_one_id_merge_into_a_single_call` doc comment)
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (the `openai_chat` module doc)

**Interfaces:**
- Consumes: everything. Produces: nothing — documentation only.

- [x] **Step 1: Confirm there is no fourth site**

Run:

```bash
grep -rn "emits TWO names\|divergence\|SMA-550" \
  crates/paigasus-helikon-providers-openai/src \
  crates/paigasus-helikon-providers-litellm/src \
  tests/provider-stream-conformance/src \
  tests/provider-stream-conformance/tests
```

Expected: hits only in the three files above. If `tests/provider-stream-conformance/src/lib.rs`
or `src/check.rs` also asserts the divergence, add it to this task's file list and
update it the same way.

- [x] **Step 2: Replace the `chat.rs` divergence paragraph**

In `handle_tool_call_chunk`'s doc comment, delete the two paragraphs beginning
*"**Divergence from `providers-litellm` (SMA-550), deliberate.**"* and
*"It is not fully aligned, though, and the gap runs the *other* way."* — everything
through *"closing it needs its own ticket."* Replace with:

```rust
    /// **Aligned with `providers-litellm` since SMA-566; the implementations
    /// differ, the behaviour does not.** Both translators guarantee exactly
    /// one name-carrying `ToolCallDelta` per non-blank `call_id`, and both
    /// merge the malformed shape — two deltas carrying different `index`
    /// values but the same `id` — into a single call rather than emitting two
    /// names.
    ///
    /// They reach it differently, for a reason rooted in the wire. litellm's
    /// `index` is optional, so one call can arrive under two key *spaces*, and
    /// it canonicalizes with a `Key { Index(u32), Id(String) }` enum. Here
    /// `ChatCompletionMessageToolCallChunk::index` is a required `u32`, so
    /// there is exactly one key space and canonicalization is a many-to-one
    /// map within it: [`Self::canonicalize`] aliases every index resolving a
    /// given `call_id` onto the first index that owned it. Keeping the `u32`
    /// key is what lets `flush_buffered_names` go on sorting by the model's
    /// declared call position rather than by a synthetic creation counter.
    ///
    /// The one remaining asymmetry is deliberate and ticketed: this crate's
    /// end-of-stream dedup net excludes blank `call_id`s, litellm's does not
    /// (SMA-616).
```

- [x] **Step 3: Replace the litellm test's doc comment**

In `crates/paigasus-helikon-providers-litellm/src/stream.rs`, find
`two_indexes_with_one_id_merge_into_a_single_call` and replace the sentence
*"`providers-openai` emits TWO names here; see the divergence comment in its
`chat.rs`."* with:

```rust
    /// `providers-openai`'s `chat.rs` emits the same single `alphabeta` here
    /// since SMA-566, via an index alias rather than this crate's `Key` enum —
    /// see the doc comment on its `handle_tool_call_chunk`. The two
    /// translators agree observably and differ structurally, because this
    /// crate's `index` is optional and its `index` is required.
```

Leave the rest of that doc comment, including the pre-fix `Some("beta")` note,
untouched.

- [x] **Step 4: Add the openai_chat counterpart note to the conformance suite**

In `tests/provider-stream-conformance/tests/conformance.rs`, append a section to the
`openai_chat` module doc, directly before `mod openai_chat {`, mirroring the `litellm`
module's existing *"`canonicalize`'s SMA-550 regression coverage lives in the crate's
own unit tests, not here"* section:

```rust
/// # SMA-566's alias regression coverage lives in the crate's own unit tests
///
/// Every tool-call delta this module scripts carries an explicit `"index":0`,
/// and every one of them carries the same `id`. The two-index-one-id shape
/// that `ChatTranslator::canonicalize` exists to merge therefore never arises
/// from these bytes, and assertion 7 passes here without exercising it. Under
/// this suite's fixture-provenance rule the shape has no capture anywhere in
/// the repo and must not be invented, so it is not registered as a scenario —
/// exactly as `litellm` records for its own `canonicalize`.
///
/// Read this subject's `conforms` test as confirming the translator behaves
/// correctly on the wire shapes OpenAI is actually observed to send, **not**
/// as a standing regression guard for the alias fix. That guard is
/// `two_indexes_with_one_id_merge_into_a_single_call` and its eleven siblings
/// in `crates/paigasus-helikon-providers-openai/src/backend/chat.rs`. If a
/// future capture ever shows a backend reusing one `id` across two indexes,
/// that would be the fixture to add here; none currently committed does.
```

- [x] **Step 5: Verify the docs build clean**

Run:

```bash
cargo test -p paigasus-helikon-providers-openai -p paigasus-helikon-providers-litellm
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-openai -p paigasus-helikon-providers-litellm --all-features --no-deps
```

Expected: PASS on both. `-D warnings` catches a broken intra-doc link such as
`[`Self::canonicalize`]` resolving to nothing.

- [x] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs \
        crates/paigasus-helikon-providers-litellm/src/stream.rs \
        tests/provider-stream-conformance/tests/conformance.rs
git commit -m "docs(providers): SMA-566 record the openai/litellm alignment at all three sites"
```

---

### Task 8: Full local CI gate

**Files:** none modified unless a gate fails.

- [x] **Step 1: Run every gate `ci.yml` runs**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: all four PASS. The conformance suite
(`tests/provider-stream-conformance`) is part of `--workspace`, so `openai_chat::conforms`
and `litellm::conforms` both run here — they must stay green, since this change must
not alter behaviour on any captured wire shape.

- [x] **Step 2: Confirm no version bumps or stray files crept in**

```bash
git diff main --stat
```

Expected: exactly four files — the spec, this plan, `chat.rs`, `stream.rs`,
`conformance.rs`. **No `Cargo.toml`, no `CHANGELOG.md`** — release-plz owns those.

- [x] **Step 3: Confirm the test count**

```bash
cargo test -p paigasus-helikon-providers-openai backend::chat::tests 2>&1 | tail -3
```

Expected: 30 tests (18 pre-existing + 12 new), 0 failed.

- [x] **Step 4: Mark the plan complete and commit**

Tick every checkbox in this file, then:

```bash
git add docs/superpowers/plans/2026-09-05-sma-566-openai-chat-call-id-canonicalization.md
git commit -m "docs(plan): SMA-566 mark the implementation plan complete"
```

---

## Self-Review

**Spec coverage.** §3.1 state → Tasks 1-2. §3.2 `seq`/no-`Default` → Task 1, pinned by
Task 3's tests 4 and the prepend/append pair. §3.3 `canonicalize` steps 1-2 → Task 2;
step 3 migration → Task 3; the loud-drop bullet → Task 4. §3.4 id-resolution rewrite →
Task 5 (the `let index = self.canonicalize(...)` line lands earlier, in Task 2, because
Task 2's tests need it). §3.5 replacement rule → Task 5. §3.6 flush net + blank guard →
Task 6. §4's three behaviour rows → Task 2 tests 1-2 (rows 1a, 1b) and Task 5's test
(row 2). §5.1's three doc sites → Task 7. §6's twelve tests → all present: Group A =
Task 2 (×2), Task 3 (×5), Task 4 (×1), Task 5 (×1); Group B = Task 2 (×1), Task 6 (×2).
§7's risks → Task 5 writes the resolution chain out in full; Task 6 orders the guard
before the net; Task 8 step 2 checks for stray version bumps.

**Placeholder scan.** No TBDs. Every code step carries the literal text to insert.
Every test carries its assertion and its expected pre-fix output.

**Type consistency.** `ensure_pending(&mut self, index: u32)` — no return value —
called identically in Tasks 1, 2, 3, 5. `canonicalize(&mut self, index: u32, call_id:
&str) -> u32` — signature fixed in Task 2, body extended in Tasks 3 and 4, never
re-signatured. `PendingToolCall::new(seq: u64)` used only by `ensure_pending`. Test
helpers `drive`/`named`/`args_of` defined once in Task 2 and used unchanged in Tasks
3-6. `make_chunk(index, id, name, arguments)` is pre-existing and unmodified.

**One deliberate deviation from the spec's task ordering:** the spec presents
`canonicalize`'s blank-id guard (§3.3 step 1) and the id replacement rule (§3.5)
together as blank-id handling. The plan splits them — guard in Task 2, replacement in
Task 5 — because the guard must exist the moment `canonical` does, or blank ids
collapse in the window between tasks. The replacement rule has no such urgency.
