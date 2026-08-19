# SMA-550 — litellm dual-`Key` correlation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `providers-litellm`'s stream translator emit at most one
name-carrying `ToolCallDelta` per `call_id`, and reassemble tool-call names
that fragment across the `Key::Index` / `Key::Id` correlation boundary.

**Architecture:** Once a delta resolves a `call_id`, the correlation key is
rewritten to a canonical `Key::Id(call_id)` and any fragments buffered under
the pre-canonical key migrate into that slot, ordered by a monotonic
buffer-creation sequence. One `call_id` then owns exactly one entry in
`pending` / `name_emitted` / `warned_late_name`, so the at-most-one-name
invariant holds by construction rather than by guard.

**Tech Stack:** Rust 2024, `tracing`, `serde_json` (test-only), no new
dependencies.

## Global Constraints

- **Sole file changed for behaviour:** `crates/paigasus-helikon-providers-litellm/src/stream.rs`. `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` gets a comment only (Task 5).
- **Never use the scope `providers-litellm`** — `.versionrc:18`'s `scopeRegex` does not list it; the local `commit-msg` hook and the `commits` CI job both reject it. Use the parent scope `providers` for work on that crate. **The PR title must also use `providers`**, since `pr-title.yml` runs on `pull_request_target` and reads the allowlist from `main`.
- **Commit message format:** `<type>(<scope>): SMA-550 <lowercase subject>`. Scope is `providers` for every litellm commit; Task 5 touches `providers-openai` instead, and **that scope is allowed** — it is in the allowlist, unlike `providers-litellm`. Doc-only commits on the artifacts in `docs/superpowers/` use `spec` or `plan` (`plan` is singular; `plans` is rejected).
- **Run `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings` before every commit.** The `pre-commit` hook is a deliberate no-op; nothing catches formatting until push.
- **No mdBook edit and no README edit.** Deliberate — `docs/book/src/concepts/agent-loop.md:57-62` and `crates/paigasus-helikon-providers-litellm/README.md:125-131` already describe the post-fix behaviour. Do not "helpfully" add one.
- **No `CHANGELOG` edits by hand** — release-plz generates them.
- **No version bumps.** `providers-litellm` 0.1.1 and `providers-openai` 0.2.22 are both released; release-plz handles bumps.
- Work **synchronously and in the foreground**. Do not background `cargo` runs and do not end a turn before the task reaches a terminal pass/fail.
- MSRV is 1.94; workspace lints include `missing_docs = "warn"`. That lint fires on items reachable from outside the crate, **not** on private ones — the translator here is `pub(crate)`, so its doc comments are a house convention rather than something the compiler demands. Write them anyway; just don't expect the build to catch a missing one.
- **The `| grep` in the verification steps below is a readability filter, not the verdict.** A pipeline's exit status is the *last* command's, so `cargo test … | grep …` reports grep's success even when cargo failed, and `| tail -N` silently truncates the result lines you are counting. Read cargo's own status — run it unpiped, or capture `${PIPESTATUS[0]}`, or `set -o pipefail`. This is not hypothetical: during this plan's execution a backgrounded `cargo test --workspace … | tail -40` returned 0 from `tail` while presenting 6 truncated result lines that aggregated to a nonsense "5 passed", and the gate had to be re-run unpiped to get a real answer.

## File Structure

| File | Responsibility | Tasks |
|---|---|---|
| `crates/paigasus-helikon-providers-litellm/src/stream.rs` | `Key`, `Pending`, `ChatTranslator`; all correlation and buffering logic, plus its `mod tests` | 1–4 |
| `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` | Sibling chat translator; receives a divergence comment only | 5 |

All new tests go in `stream.rs`'s existing `#[cfg(test)] mod tests`, alongside
SMA-547's dual-key tests. `ChatTranslator` is `pub(crate)`, so an integration
test under `tests/` cannot reach it.

## Reference: the five wire shapes

Referenced by name throughout. **Outputs in the "today" column were captured by
running these against unmodified `stream.rs`** — they are measurements, not
predictions.

| # | Deltas (`tool_calls` arrays only) | Today | After |
|---|---|---|---|
| **A** | `{"index":0,"id":"c1","function":{"name":"get_","arguments":"{"}}` then `{"id":"c1","function":{"name":"weather","arguments":"}"}}` | `Some("get_")` **+** `Some("weather")` | `Some("get_")` only |
| **B** | `{"index":0,"id":"c1","function":{"name":"get_"}}` then `{"id":"c1","function":{"name":"weather","arguments":"{}"}}` | `Some("weather")` | `Some("get_weather")` |
| **C** | `{"id":"c1","function":{"name":"get_"}}`, `{"index":0,"function":{"name":"weath"}}`, `{"index":0,"id":"c1","function":{"name":"er","arguments":"{}"}}` | `Some("weather")` | `Some("get_weather")` |
| **B′** | `{"index":0,"function":{"name":"get_"}}`, `{"id":"c1","function":{"name":"weath"}}`, `{"index":0,"id":"c1","function":{"name":"er","arguments":"{}"}}` | `Some("get_er")` | `Some("get_weather")` |
| **D** | one array: `{"index":0,"id":"c1","function":{"name":"alpha"}}`, `{"index":1,"id":"c1","function":{"name":"beta","arguments":"{}"}}` | `Some("beta")` | `Some("alphabeta")` |
| **E** | `{"index":0,…"IDX"}`, `{"id":"c1",…"ID"}`, `{"id":"c1",…"IE"}`, `{"index":0,…"IDX"}`, `{"index":0,"id":"c1","arguments":"{}"}` | `Some("IDXIDX")` | `Some("IDXIDXIDIE")` |

**C and B′ are mirror images and they are why `seq` exists.** A plain
`insert_str(0, …)` prepend gets B′ right and C wrong; a plain `push_str` append
gets C right and B′ wrong. Only ordering by buffer-creation `seq` satisfies both.

**Shape E's third fragment is `IE`, not a second `ID`.** The original fixture
repeated `ID`, and SMA-547's whole-name-repeat guard silently swallowed the
repeat — so the test fed 10 characters, pinned 8, and credited the difference to
the merge rather than to the guard. Making the third fragment distinct is what
lets the row's "lossless" claim actually hold. Keep every fragment in this shape
distinct for the same reason.

---

### Task 1: Give `Pending` a creation sequence

Pure refactor — **no behaviour change**. All 158 existing tests must still pass.

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs:48-61` (`Pending`), `:68-103` (`ChatTranslator` fields + `new`), `:228` and `:261` (the two `.or_default()` sites)

**Interfaces:**
- Produces: `Pending::new(seq: u64) -> Pending`; field `Pending::seq: u64`; `ChatTranslator::next_seq: u64`; method `ChatTranslator::ensure_pending(&mut self, key: &Key)`. Tasks 2 and 3 both consume `ensure_pending` and `Pending::seq`.

- [ ] **Step 1: Replace the `Pending` definition**

Replace `stream.rs:48-61` (the doc comment, `#[derive(Default)]`, and the
struct) in full with:

```rust
/// Buffered name and args fragments for a tool call.
///
/// Both fields start out buffered here regardless of whether the call's
/// `id` is known yet. Once the id is known, `args` is drain-once — taken on
/// the first delta after the id is observed and never re-prepended — while
/// `name` keeps accumulating across every delta and is cleared only when it
/// flushes (SMA-547 §1).
///
/// **No `Default` impl, deliberately.** Every buffer must carry the `seq` it
/// was created with, so construction goes through [`Pending::new`] via
/// [`ChatTranslator::ensure_pending`]. Deriving `Default` would let an
/// `.or_default()` call site silently mint a buffer with `seq: 0`, which
/// would corrupt the merge order in [`ChatTranslator::canonicalize`]
/// (SMA-550). The absence of the derive is what makes that a compile error
/// rather than a latent bug.
struct Pending {
    /// Monotonic creation order across all buffers in one stream.
    ///
    /// Used to merge two buffers for one call in wire order (SMA-550) and to
    /// give `flush_buffered_names` a deterministic end-of-stream order.
    seq: u64,
    /// Accumulated function-name fragments.
    name: String,
    /// Accumulated JSON-arguments fragments.
    args: String,
}

impl Pending {
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

- [ ] **Step 2: Add the `next_seq` field**

In `ChatTranslator` (`stream.rs:68-90`), insert after the `pending` field:

```rust
    /// Next value handed out by [`Self::ensure_pending`]; never reused.
    next_seq: u64,
```

and in `new()` (`stream.rs:94-103`), insert after `pending: HashMap::new(),`:

```rust
            next_seq: 0,
```

- [ ] **Step 3: Add `ensure_pending`**

Add as the first method after `new()` in the `impl ChatTranslator` block:

```rust
    /// Ensure a buffer exists for `key`, stamping a fresh `seq` on creation.
    ///
    /// Deliberately returns nothing rather than `&mut Pending`: callers then
    /// reach the buffer through `self.pending.get_mut(..)`, which borrows one
    /// field instead of all of `self` and so leaves the surrounding
    /// disjoint-field borrows of `name_emitted` and `tool_calls` intact.
    fn ensure_pending(&mut self, key: &Key) {
        if !self.pending.contains_key(key) {
            self.pending.insert(key.clone(), Pending::new(self.next_seq));
            self.next_seq += 1;
        }
    }
```

- [ ] **Step 4: Convert the two `.or_default()` call sites**

At `stream.rs:228` (inside the unresolved-id branch), replace:

```rust
            let slot = self.pending.entry(key).or_default();
```

with:

```rust
            self.ensure_pending(&key);
            let slot = self
                .pending
                .get_mut(&key)
                .expect("ensure_pending just inserted this key");
```

At `stream.rs:261`, replace:

```rust
        let slot = self.pending.entry(key.clone()).or_default();
```

with:

```rust
        self.ensure_pending(&key);
        let slot = self
            .pending
            .get_mut(&key)
            .expect("ensure_pending just inserted this key");
```

Leave every line below `:261` untouched — in particular the flush condition at
`:287` still reads `self.name_emitted.contains_key(&key)`. It compiles because
`get_mut` borrows only `self.pending`.

- [ ] **Step 5: Verify no behaviour changed**

```bash
set -o pipefail
cargo test -p paigasus-helikon-providers-litellm --all-features 2>&1 | grep -E "^test result:|^error"
```

Expected: the same six `test result: ok.` lines as the baseline —
`140`, `1`, `10`, `0 (1 ignored)`, `7`, `0 (1 ignored)` passed, **0 failed**.
If any test fails, the refactor changed behaviour; stop and diagnose rather
than adjusting the test.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
git commit -m "refactor(providers): SMA-550 stamp pending tool-call buffers with a creation seq"
```

---

### Task 2: Order the end-of-stream flush by `seq`

Still **no behaviour change today** — but it must land *before* Task 3, because
canonicalization turns every resolved key into `Key::Id(call_id)` and would
otherwise silently convert this sort from wire order to lexicographic-by-id.

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs:32-36` (`Key`'s derive + comment), `:326-328` (the sort in `flush_buffered_names`)
- Test: same file, `mod tests`

**Interfaces:**
- Consumes: `Pending::seq` from Task 1.

- [ ] **Step 1: Write the failing-guard test**

Add to `mod tests`. This test **passes before and after** — it is a regression
guard for Task 3, not a red test. Add it now so Task 3 cannot silently break
flush ordering.

```rust
    /// Two parallel zero-argument calls flush in wire order, not in
    /// call_id-lexicographic order.
    ///
    /// Pins the `seq` sort in `flush_buffered_names`. After SMA-550's
    /// canonicalization every resolved pending key is `Key::Id(call_id)`, so
    /// a sort by `Key` would silently reorder these two by call_id —
    /// `call_a` before `call_z` — reversing what the wire said. The ids are
    /// chosen so lexicographic and wire order disagree.
    #[test]
    fn parallel_zero_argument_calls_flush_in_wire_order() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![tc_chunk(serde_json::json!([
                {"index": 0, "id": "call_z", "function": {"name": "zulu"}},
                {"index": 1, "id": "call_a", "function": {"name": "alpha"}}
            ]))],
        );
        assert_eq!(
            named(&evs),
            vec![
                ("call_z".to_owned(), "zulu".to_owned()),
                ("call_a".to_owned(), "alpha".to_owned()),
            ],
            "wire order (index 0 then 1), not lexicographic by call_id"
        );
    }
```

This test uses three helpers that do not exist yet. Add them at the top of
`mod tests`, directly after the existing `fn chunk(...)` helper:

```rust
    /// Wrap a `tool_calls` array in the surrounding chunk envelope.
    fn tc_chunk(tool_calls: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"choices": [{"index": 0, "delta": {"tool_calls": tool_calls}}]})
    }

    /// Drive every chunk through `consume`, then `finish`, collecting all
    /// events. Collecting across both is required: the SMA-550 invariant is
    /// per-stream, and a `finish()`-only assertion passes vacuously against
    /// the pre-fix translator, whose violation is two *mid-stream* emissions.
    fn drive(t: &mut ChatTranslator, chunks: Vec<serde_json::Value>) -> Vec<ModelEvent> {
        let mut out = Vec::new();
        for c in chunks {
            out.extend(t.consume(chunk(c)));
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

    /// Concatenated `args_delta` for one `call_id`, in emission order.
    fn args_of(evs: &[ModelEvent], call_id: &str) -> String {
        evs.iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id: c,
                    args_delta,
                    ..
                } if c == call_id => Some(args_delta.as_str()),
                _ => None,
            })
            .collect()
    }
```

- [ ] **Step 2: Run it — it must PASS**

```bash
set -o pipefail
cargo test -p paigasus-helikon-providers-litellm --lib stream::tests::parallel_zero_argument_calls_flush_in_wire_order -- --exact 2>&1 | tail -5
```

Expected: `test result: ok. 1 passed`. If it fails now, the helpers are wrong —
fix them before touching the sort. `args_of` is unused until Task 3; if clippy
objects to a dead helper, add `#[allow(dead_code)]` to `args_of` and remove the
attribute in Task 3.

- [ ] **Step 3: Switch the sort to `seq`**

In `flush_buffered_names` (`stream.rs:326-328`), replace:

```rust
        let mut keys: Vec<Key> = self.pending.keys().cloned().collect();
        keys.sort();
```

with:

```rust
        // Sorted by buffer-creation order, not by `Key`. After SMA-550 every
        // resolved key is `Key::Id(call_id)`, so sorting by `Key` would mean
        // lexicographic-by-call_id — silently reordering parallel calls
        // against the wire. `seq` is unique per buffer, so this is a total,
        // deterministic order that matches first appearance.
        let mut keys: Vec<Key> = self.pending.keys().cloned().collect();
        keys.sort_by_key(|k| self.pending[k].seq);
```

- [ ] **Step 4: Drop `Key`'s now-purposeless `Ord` derive**

Replace `stream.rs:32-36` (doc line, the three `//` comment lines, and the
derive) with:

```rust
/// Correlation key for a streaming tool call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
```

The removed comment justified the derive as making "the dual-key winner
predictable"; `flush_buffered_names` was its only consumer and now sorts by
`seq`. If the crate fails to compile, some other consumer exists — find it and
report rather than restoring the derive silently.

- [ ] **Step 5: Verify the whole suite is still green**

```bash
set -o pipefail
cargo test -p paigasus-helikon-providers-litellm --all-features 2>&1 | grep -E "^test result:|^error"
```

Expected: `141` passed in the lib target (140 baseline + the new test), 0 failed
everywhere.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
git commit -m "refactor(providers): SMA-550 order the eos name flush by buffer creation"
```

---

> **Task 3's `canonicalize` below is SUPERSEDED — do not copy it.** Review of
> the finished branch found three shapes where the merge as written here was
> worse than the code it replaced: it doubled a repeated whole name across the
> key boundary, collapsed distinct calls that both reported a blank `id`, and
> silently stranded a fragment migrating into an already-emitted slot. The
> shipped version guards all three. This task is retained as the record of what
> was planned; for the algorithm that shipped, read
> `crates/paigasus-helikon-providers-litellm/src/stream.rs::canonicalize` and
> §"Three regressions the first implementation introduced" in the design doc.

### Task 3: Canonicalize the correlation key on the resolved `call_id`

The behavioural fix.

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` — insert `canonicalize` in the `impl ChatTranslator` block, and one call line after the `let Some(call_id) = ... else { ... };` block (currently `:226-232`)
- Test: same file, `mod tests`

**Interfaces:**
- Consumes: `ensure_pending`, `Pending::new`, `Pending::seq` (Task 1); the `seq` sort (Task 2).
- Produces: `ChatTranslator::canonicalize(&mut self, key: Key, call_id: &str) -> Key`.

- [ ] **Step 1: Write the failing tests**

Add all eight to `mod tests`.

```rust
    /// SMA-550 acceptance criterion: one `call_id` reached under both `Key`
    /// variants must not produce two name-carrying deltas — mid-stream
    /// included. Shape A.
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("get_")` and then `Some("weather")`, both from `consume`.
    #[test]
    fn dual_key_call_emits_at_most_one_name_mid_stream() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "get_", "arguments": "{"}}
                ])),
                tc_chunk(serde_json::json!([
                    {"id": "c1", "function": {"name": "weather", "arguments": "}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "get_".to_owned())],
            "exactly one name-carrying delta per call_id, across consume and finish"
        );
        assert_eq!(
            args_of(&evs, "c1"),
            "{}",
            "suppressing the second name must not swallow its args"
        );
    }

    /// SMA-550 shape B: a name fragmented across the key boundary reassembles
    /// instead of losing the pre-boundary fragment.
    ///
    /// Confirmed to FAIL against the pre-fix translator, which emits
    /// `Some("weather")` — `get_` is silently discarded by the EOS guard.
    #[test]
    fn name_fragments_split_across_the_key_boundary_reassemble() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "get_"}}
                ])),
                tc_chunk(serde_json::json!([
                    {"id": "c1", "function": {"name": "weather", "arguments": "{}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "get_weather".to_owned())]
        );
    }

    /// SMA-550 shape C: the id-keyed buffer is created FIRST, so the
    /// index-keyed buffer migrating into it must be *appended*.
    ///
    /// Together with `index_keyed_buffer_created_first_merges_in_order` this
    /// is why `Pending` carries a `seq`: a naive `insert_str(0, ..)` prepend
    /// passes that test and yields `Some("weathget_er")` here.
    ///
    /// Confirmed to FAIL against the pre-fix translator (`Some("weather")`).
    #[test]
    fn id_keyed_buffer_created_before_the_index_keyed_one_merges_in_order() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([{"id": "c1", "function": {"name": "get_"}}])),
                tc_chunk(serde_json::json!([{"index": 0, "function": {"name": "weath"}}])),
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "er", "arguments": "{}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "get_weather".to_owned())]
        );
    }

    /// SMA-550 shape B′: the index-keyed buffer is created FIRST, so it must
    /// be *prepended* when it migrates. Mirror of the test above; a naive
    /// `push_str` append yields `Some("weathget_er")` here.
    ///
    /// Confirmed to FAIL against the pre-fix translator (`Some("get_er")`).
    #[test]
    fn index_keyed_buffer_created_first_merges_in_order() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([{"index": 0, "function": {"name": "get_"}}])),
                tc_chunk(serde_json::json!([{"id": "c1", "function": {"name": "weath"}}])),
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "er", "arguments": "{}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "get_weather".to_owned())]
        );
    }

    /// SMA-550 shape D: two entries in one array carrying different `index`
    /// values but the same `id` are one call, and yield one name.
    ///
    /// The merged name `alphabeta` is not "correct" in any deep sense — the
    /// input is malformed, since an `id` identifies a call — but it is one
    /// name for one call_id, which is the invariant under test.
    /// `providers-openai` emits TWO names here; see the divergence comment in
    /// its `chat.rs`.
    ///
    /// Confirmed to FAIL against the pre-fix translator (`Some("beta")`).
    #[test]
    fn two_indexes_with_one_id_merge_into_a_single_call() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![tc_chunk(serde_json::json!([
                {"index": 0, "id": "c1", "function": {"name": "alpha"}},
                {"index": 1, "id": "c1", "function": {"name": "beta", "arguments": "{}"}}
            ]))],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "alphabeta".to_owned())]
        );
    }

    /// SMA-550 shape E, the accepted residual: when the two keys interleave
    /// at *fragment* level, no buffer-level order can reconstruct the wire
    /// sequence, and the merge misorders them.
    ///
    /// This is deliberately pinned rather than left undefined. It is still
    /// strictly better than the pre-fix translator, which emits
    /// `Some("IDXIDX")` and silently discards `ID` entirely: the merge is
    /// lossless and carries a `warn!`. Do not "fix" this to `IDXIDIDX`
    /// without adding per-fragment sequencing and re-deciding the trade-off.
    #[test]
    fn interleaved_dual_keying_is_lossless_and_misordered() {
        let mut t = ChatTranslator::new();
        let evs = drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([{"index": 0, "function": {"name": "IDX"}}])),
                tc_chunk(serde_json::json!([{"id": "c1", "function": {"name": "ID"}}])),
                tc_chunk(serde_json::json!([{"id": "c1", "function": {"name": "IE"}}])),
                tc_chunk(serde_json::json!([{"index": 0, "function": {"name": "IDX"}}])),
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"arguments": "{}"}}
                ])),
            ],
        );
        assert_eq!(
            named(&evs),
            vec![("c1".to_owned(), "IDXIDXIDIE".to_owned())],
            "lossless: every fragment survives, even though the order is not recoverable"
        );
    }

    /// The canonical key must be registered in `tool_calls`.
    ///
    /// Not defensive padding: `flush_buffered_names` resolves a pending key
    /// through `tool_calls` and would skip a canonicalized call entirely,
    /// and `warn_unresolved_pending` would then report that same call as an
    /// unresolved loss on every healthy stream. Pinned so a future "this
    /// self-mapping looks redundant" cleanup fails loudly.
    #[test]
    fn canonical_key_resolves_through_tool_calls() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(tc_chunk(serde_json::json!([
            {"index": 0, "id": "c1", "function": {"name": "get_weather"}}
        ]))));
        assert_eq!(
            t.tool_calls.get(&Key::Id("c1".to_owned())).map(String::as_str),
            Some("c1"),
            "canonicalize must register the canonical key"
        );
    }

    /// SMA-550's secondary defect: the fragment dropped in shape A must be
    /// reported. Pre-fix it was recorded under no key at all, because the
    /// losing key never emitted and so never reached the late-name check.
    #[test]
    fn dual_key_late_fragment_is_reported() {
        let mut t = ChatTranslator::new();
        drive(
            &mut t,
            vec![
                tc_chunk(serde_json::json!([
                    {"index": 0, "id": "c1", "function": {"name": "get_", "arguments": "{"}}
                ])),
                tc_chunk(serde_json::json!([
                    {"id": "c1", "function": {"name": "weather", "arguments": "}"}}
                ])),
            ],
        );
        assert!(
            t.warned_late_name.contains(&Key::Id("c1".to_owned())),
            "the dropped `weather` fragment must be recorded under the canonical key"
        );
        assert_eq!(t.warned_late_name.len(), 1, "at most one warning per call");
    }
```

- [ ] **Step 2: Run them and RECORD the failures**

```bash
set -o pipefail
cargo test -p paigasus-helikon-providers-litellm --lib 2>&1 | grep -E "^test .*(dual_key|reassemble|merges_in_order|two_indexes|interleaved|canonical_key) .*(ok|FAILED)"
```

Expected: **all eight FAIL** (`parallel_zero_argument_calls_flush_in_wire_order`
from Task 2 still passes). Capture the actual `left`/`right` values from the
output — the acceptance criterion requires the test be *verified* to fail, and
the recorded values are the evidence. Cross-check them against the "today"
column of the shape table above; if any disagrees, stop and reconcile before
implementing — a test failing for the wrong reason proves nothing.

- [ ] **Step 3: Add `canonicalize`**

Insert into the `impl ChatTranslator` block, immediately after `ensure_pending`:

```rust
    /// Rewrite `key` to the canonical slot for `call_id`, migrating any
    /// fragments buffered under the pre-canonical key.
    ///
    /// Every delta for one call — however it was keyed on the wire — shares a
    /// single state entry from here on. That is what makes "at most one
    /// name-carrying `ToolCallDelta` per `call_id`" hold by construction
    /// rather than by guard, and it is what lets a name fragmented across the
    /// `Key::Index` / `Key::Id` boundary reassemble instead of losing a
    /// fragment (SMA-550).
    fn canonicalize(&mut self, key: Key, call_id: &str) -> Key {
        // Already canonical — return without allocating. This is the common
        // path: LiteLLM keys every tool-call delta by `index`, so a resolved
        // stream would otherwise pay a `String` allocation per args chunk.
        if matches!(&key, Key::Id(id) if id == call_id) {
            return key;
        }
        let canonical = Key::Id(call_id.to_owned());

        if let Some(old) = self.pending.remove(&key) {
            self.ensure_pending(&canonical);
            let slot = self
                .pending
                .get_mut(&canonical)
                .expect("ensure_pending just inserted this key");

            // A resolved slot drains `args` on every delta and is removed once
            // both fields are empty, so `slot.args` is always empty here and
            // this is an assignment rather than a splice. The assert pins that
            // dependency: if drain-once is ever relaxed, this fails loudly
            // instead of silently mis-ordering JSON.
            debug_assert!(
                slot.args.is_empty(),
                "a resolved slot drains its args on every delta"
            );
            slot.args.insert_str(0, &old.args);

            if !old.name.is_empty() && !slot.name.is_empty() {
                tracing::warn!(
                    target: "paigasus::litellm::stream",
                    %call_id,
                    "tool-call name fragments for one call arrived under two \
                     correlation keys; merging in buffer-creation order, which \
                     may misorder them if the two keys interleaved"
                );
            }
            // Order by creation `seq`, not by which buffer is migrating.
            // Both orderings are reachable, and a plain prepend or a plain
            // append is wrong in exactly one of them (SMA-550 §Merge order).
            if old.seq < slot.seq {
                slot.name.insert_str(0, &old.name);
                slot.seq = old.seq;
            } else {
                slot.name.push_str(&old.name);
            }
        }

        // `flush_buffered_names` and `warn_unresolved_pending` both resolve a
        // pending key through `tool_calls`. Without this the canonical key
        // resolves to nothing: the call is skipped at flush AND then falsely
        // reported as an unresolved loss. Pinned by
        // `canonical_key_resolves_through_tool_calls`.
        self.tool_calls
            .entry(canonical.clone())
            .or_insert_with(|| call_id.to_owned());
        canonical
    }
```

- [ ] **Step 4: Call it**

In `handle_tool_call`, directly after the `let Some(call_id) = ... else { ... };`
block (which ends with `};` at `stream.rs:232`) and before the late-name-warning
comment block, insert:

```rust
        // From here on, one call_id owns exactly one state entry.
        let key = self.canonicalize(key, &call_id);
```

- [ ] **Step 5: Run the full crate suite**

```bash
set -o pipefail
cargo test -p paigasus-helikon-providers-litellm --all-features 2>&1 | grep -E "^test result:|^error|FAILED"
```

Expected: the eight new tests pass. **Two pre-existing tests are expected to
fail here** — `late_name_fragment_warns_once` and
`one_call_id_under_two_keys_flushes_a_single_name`. Both are retargeted in
Task 4; do **not** fix them by weakening the new code. Any *other* failure is a
real regression — diagnose it before continuing.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
git commit -m "fix(providers): SMA-550 canonicalize litellm tool-call keys on the resolved call_id"
```

Committing with two known-failing tests is intentional — Task 4 is the other
half of one logical change and follows immediately. Do not push between the two.

---

### Task 4: Retarget the stale tests, make the guard loud, fix the stale comments

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` — `mod tests` (three tests) and six comment sites

**Interfaces:**
- Consumes: `canonicalize` (Task 3). Produces nothing new.

- [ ] **Step 1: Retarget `late_name_fragment_warns_once`**

At `stream.rs:1093-1097`, replace the second assertion:

```rust
        assert_eq!(
            t.name_emitted.get(&Key::Index(0)).map(String::as_str),
            Some("get_"),
            "the already-emitted name is not retroactively changed"
        );
```

with:

```rust
        assert_eq!(
            t.name_emitted
                .get(&Key::Id("c1".to_owned()))
                .map(String::as_str),
            Some("get_"),
            "the already-emitted name is not retroactively changed"
        );
        assert!(
            t.warned_late_name.contains(&Key::Id("c1".to_owned())),
            "the loss is recorded under the canonical key"
        );
```

The key changed because SMA-550 canonicalizes state onto `Key::Id(call_id)`.
The second assertion mirrors `providers-openai`'s equivalent test
(`chat.rs:1066-1072`), which asserts on the warned set where the litellm test
did not.

- [ ] **Step 2: Retarget `one_call_id_under_two_keys_flushes_a_single_name`**

At `stream.rs:1146-1150`, replace:

```rust
        assert_eq!(
            named[0],
            ("c1".to_owned(), "get_".to_owned()),
            "Key::Index sorts before Key::Id, so the winner is deterministic"
        );
```

with:

```rust
        assert_eq!(
            named[0],
            ("c1".to_owned(), "get_weather".to_owned()),
            "the two keys are one slot after SMA-550, so the fragments reassemble"
        );
```

There is no "winner" any more, and `Key` no longer derives `Ord`. Also update
the `// No `index` -> keys as Key::Id("c1"): a second entry for one call.`
comment at `:1126` to read:

```rust
        // No `index` -> would key as Key::Id("c1"); since SMA-550 that IS the
        // canonical key, so this joins the same slot rather than opening a
        // second entry for one call.
```

- [ ] **Step 3: Correct the docstring on `flush_does_not_re_emit_a_name_already_flushed_under_another_key`**

Its assertion still holds, but its stated reason no longer does. Replace the
doc comment at `stream.rs:1153-1157` with:

```rust
    /// A call_id that already flushed its name mid-stream must not get a
    /// second name at end-of-stream.
    ///
    /// Since SMA-550 this holds because both deltas share one canonical slot,
    /// so `name_emitted` suppresses the second flush directly — not because
    /// of the `already` seed in `flush_buffered_names`, which this sequence no
    /// longer reaches. Kept because the invariant is what matters, not the
    /// mechanism that currently enforces it.
```

- [ ] **Step 4: Make the `already` guard loud**

In `flush_buffered_names`, replace the guard at `stream.rs:350-356`:

```rust
            // Claimed only once we know this key actually has a name to
            // flush — claiming earlier would let an empty-name entry for a
            // resolved call_id block a sibling key that does have one.
            if !already.insert(call_id.clone()) {
                continue;
            }
```

with:

```rust
            // Claimed only once we know this key actually has a name to flush
            // — claiming earlier would let an empty-name entry for a resolved
            // call_id suppress another entry that does have one.
            if !already.insert(call_id.clone()) {
                // Unreachable since SMA-550: canonicalization gives each
                // call_id exactly one pending key, so two keys can no longer
                // resolve to one call_id. Kept as a net because it enforces
                // the at-most-one-name invariant at the point of emission,
                // independent of the keying discipline upstream — which is
                // precisely what a cross-provider conformance suite asserts.
                // Loud rather than a bare `continue`: if the keying is ever
                // loosened again, a silent drop here would recreate the exact
                // undiagnosed loss SMA-550 existed to fix.
                tracing::error!(
                    target: "paigasus::litellm::stream",
                    %call_id,
                    ?key,
                    "two pending keys resolved to one call_id after \
                     canonicalization; dropping the second buffered name. This \
                     is a correlation-keying regression, not a backend quirk"
                );
                continue;
            }
```

Note `key` is moved by `self.name_emitted.insert(key, ...)` further down the
loop; the `?key` here is before that, so it borrows fine.

- [ ] **Step 5: Fix the remaining stale comments**

1. `stream.rs:44-45` — `Key::Id`'s variant doc. Replace:

```rust
    /// Correlated by `delta.tool_calls[].id`, when `index` is absent.
    Id(String),
```

with:

```rust
    /// Correlated by `delta.tool_calls[].id`.
    ///
    /// Reached two ways: as the wire key when `index` is absent, and — since
    /// SMA-550 — as the *canonical* key for every call whose `id` has
    /// resolved, whether or not it also carried an `index`. See
    /// [`ChatTranslator::canonicalize`].
    Id(String),
```

2. `stream.rs:69` — the `tool_calls` field doc. Replace:

```rust
    /// Resolved call ids, keyed by correlation [`Key`].
```

with:

```rust
    /// Resolved call ids, keyed by correlation [`Key`].
    ///
    /// Holds both wire keys (`Key::Index(i)`, so later index-only deltas keep
    /// resolving) and the canonical `Key::Id(call_id) -> call_id` self-mapping
    /// that [`Self::canonicalize`] registers. The self-mapping is load-bearing:
    /// `flush_buffered_names` and `warn_unresolved_pending` both resolve a
    /// pending key through this map.
```

3. `stream.rs:71` and `:80` — the `name_emitted` and `pending` field docs.
   Change the opening words of each from `Key → …` / `Name/args fragments
   buffered per call.` to name the canonical key. For `name_emitted`:

```rust
    /// Canonical key → the tool name already emitted to the consumer.
```

   and insert this line into the `pending` doc after its first line:

```rust
    /// Keyed by the canonical key once the call's `id` resolves, so one
    /// `call_id` never owns two buffers (SMA-550).
```

4. `stream.rs:11-24` — module invariant #2. Append to that paragraph, directly
   before the closing `//!` line of the invariant:

```rust
//!    Correlation itself is canonicalized: once a delta resolves the call's
//!    `id`, the key becomes `Key::Id(call_id)` and any fragments buffered
//!    under the pre-canonical key migrate into that slot in buffer-creation
//!    order. One `call_id` therefore owns exactly one state entry, which is
//!    what makes "at most one name-carrying delta per `call_id`" structural
//!    rather than guarded (SMA-550).
```

5. `stream.rs:322-325` — in the `flush_buffered_names` doc comment, replace:

```rust
    /// Skips entries whose `id` never resolved (nothing to emit under) and
    /// entries whose resolved `call_id` already emitted a name. The latter is
    /// not redundant with `name_emitted`: a call reached under both
    /// `Key::Index` and `Key::Id` has two entries for one `call_id`, and the
    /// contract allows only one name-carrying delta per call.
```

with:

```rust
    /// Skips entries whose `id` never resolved (nothing to emit under) and
    /// entries whose resolved `call_id` already emitted a name. Since SMA-550
    /// the latter check is redundant — canonicalization gives each `call_id`
    /// one key — and is kept as a net; see the comment at its `continue`.
```

- [ ] **Step 6: Run the full crate suite — everything green**

```bash
set -o pipefail
cargo test -p paigasus-helikon-providers-litellm --all-features 2>&1 | grep -E "^test result:|^error|FAILED"
```

Expected: **0 failed** in every target. The lib target should show 149 passed
(140 baseline + 1 from Task 2 + 8 from Task 3).

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
git commit -m "test(providers): SMA-550 retarget dual-key tests and make the eos guard loud"
```

---

### Task 5: Document the `providers-openai` divergence

AC #3 requires openai stay behaviourally aligned **or** the divergence be
documented in a code comment. It is not aligned, so: document.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs:398` (immediately above `fn handle_tool_call_chunk`)

**Interfaces:** none — comment only. No behaviour change, no test.

- [ ] **Step 1: Insert the comment**

Directly above `fn handle_tool_call_chunk(` at `chat.rs:398`, insert:

```rust
    /// Correlate one tool-call delta and emit any completed name/args.
    ///
    /// **Divergence from `providers-litellm` (SMA-550), deliberate.** That
    /// crate canonicalizes its correlation state onto the resolved `call_id`,
    /// because its `index` is optional on the wire and one call can arrive
    /// under two different keys. This translator does not, and does not need
    /// to for that case: `ChatCompletionMessageToolCallChunk::index` is a
    /// required `u32`, so there is exactly one key space here.
    ///
    /// It is not fully aligned, though, and the gap runs the *other* way.
    /// Given two deltas carrying different `index` values but the **same**
    /// `id` — malformed, since an `id` identifies a call — litellm merges them
    /// into one call emitting one name, while this translator keeps two
    /// indexes and emits a name for each: two name-carrying `ToolCallDelta`s
    /// for one `call_id`. `flush_buffered_names` below has no `call_id`-level
    /// dedup, so nothing catches it. That shape is unobserved from any
    /// backend, which is why SMA-550 documented it here rather than changing
    /// this code. A cross-provider conformance suite asserting "at most one
    /// name-carrying delta per `call_id`" would fail here and pass for
    /// litellm; closing it needs its own ticket.
```

If `fn handle_tool_call_chunk` already carries a doc comment, merge this text
into it rather than stacking a second block above it. Consecutive `///` lines
concatenate into one doc comment, so stacking compiles fine — it just reads as
two disjoint openings on one item. (What *is* a compile error, `E0585`, is a
`///` block with no item after it — so watch the placement, not the count.)

- [ ] **Step 2: Verify the crate still builds and documents cleanly**

```bash
set -o pipefail
cargo test -p paigasus-helikon-providers-openai --all-features 2>&1 | grep -E "^test result:|^error"
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-openai --all-features --no-deps 2>&1 | tail -5
```

Expected: all tests pass, docs build with no warnings. `missing_docs` is a
workspace lint, so a malformed doc comment fails here rather than at CI.

- [ ] **Step 3: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs
git commit -m "docs(providers-openai): SMA-550 record the dual-key divergence from litellm"
```

`providers-openai` **is** in `.versionrc`'s scope allowlist, so this one commit
may use the precise scope.

---

### Task 6: Full local CI gate reproduction

**Files:** none — verification only.

- [ ] **Step 1: Run every gate CI runs**

Run each to completion and record the result. Do not background them.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

- [ ] **Step 2: Confirm the diff matches the plan**

```bash
git diff main --stat
```

Expected: exactly two files changed —
`crates/paigasus-helikon-providers-litellm/src/stream.rs` and
`crates/paigasus-helikon-providers-openai/src/backend/chat.rs` — plus the two
`docs/superpowers/` files already committed. **If any other file appears, stop
and report it.** In particular there must be no `Cargo.toml`, `CHANGELOG.md`,
`README.md` or `docs/book/` change.

- [ ] **Step 3: Confirm no debug residue**

```bash
git diff main -- '*.rs' | grep -nE '^\+.*(dbg!|println!|eprintln!|todo!|unimplemented!|#\[ignore\])' || echo "clean"
```

Expected: `clean`.

- [ ] **Step 4: Verify the commit range passes convco**

```bash
convco check "$(git merge-base origin/main HEAD)..HEAD"
```

Expected: pass. The merge-base — not `origin/main`'s tip — is required; a
diverged tip makes convco silently walk the entire history instead.

---

## Self-Review

**Spec coverage.** Every section maps to a task: `Pending.seq` + no-`Default`
→ Task 1; `seq` flush sort + `Ord` removal → Task 2; `canonicalize`, the merge
order, the allocation early-return, the `debug_assert`, the merge `warn!`, and
the `tool_calls` self-mapping → Task 3; the `already` guard's `error!`, all
three test retargets, and all six stale comments → Task 4; the openai
divergence and its SMA-533 note → Task 5; release/scope/docs constraints →
Global Constraints and Task 6. All twelve rows of the spec's test table appear.

**Type consistency.** `Pending::new(seq: u64)`, `Pending::seq: u64`,
`ChatTranslator::next_seq: u64`, `ensure_pending(&mut self, key: &Key)` (returns
`()`), and `canonicalize(&mut self, key: Key, call_id: &str) -> Key` are used
with identical signatures wherever they appear. The four test helpers
(`tc_chunk`, `drive`, `named`, `args_of`) are defined once in Task 2 and used
under the same names in Tasks 2 and 3.

**Known-red window.** Task 3 commits with two pre-existing tests failing, fixed
in Task 4. This is called out in both tasks and is why the plan says not to push
between them. It is deliberate: retargeting those tests *before* the behaviour
change would mean committing assertions that contradict the code.

**Borrow-checker hazard, pre-empted.** `ensure_pending` returns `()` rather than
`&mut Pending` on purpose. Returning a reference would borrow all of `self`,
and the flush condition at `:287` and the insert at `:293` both touch
`self.name_emitted` while the buffer borrow is live — a `&mut self` helper turns
those into compile errors and would force an unnecessary restructure of code
this ticket has no reason to touch. The rationale is in the method's doc
comment so it survives a later "simplify this" pass.
