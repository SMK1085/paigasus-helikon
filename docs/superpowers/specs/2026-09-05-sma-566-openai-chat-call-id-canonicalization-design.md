# SMA-566 — Canonicalize `openai/chat` tool-call correlation on the resolved `call_id`

**Ticket:** [SMA-566](https://linear.app/smaschek/issue/SMA-566/openaichat-emits-two-name-carrying-deltas-for-one-call-id-given-the)
**Date:** 2026-09-05
**Related:** SMA-550 (the litellm fix this aligns with), SMA-547 (the name-buffering
invariants this must preserve), SMA-533 (the conformance suite that does not catch
this)

## 1. The defect

`ChatTranslator` in `crates/paigasus-helikon-providers-openai/src/backend/chat.rs`
keys every piece of tool-call correlation state on the wire `index: u32`:

```rust
tool_calls:       HashMap<u32, String>,           // index -> call_id
name_emitted:     HashMap<u32, String>,           // index -> name already emitted
warned_late_name: HashSet<u32>,
pending:          HashMap<u32, PendingToolCall>,  // index -> buffered name/args
```

Given two tool-call deltas carrying **different `index` values but the same `id`**,
the translator keeps two index-keyed entries, resolves both to the same `call_id`,
and emits a name-carrying `ToolCallDelta` for each. `flush_buffered_names` has no
`call_id`-level dedup, so nothing catches it at end-of-stream either.

The translator has documented this against itself since SMA-550, at
`chat.rs:400-417`, closing with: *"A cross-provider conformance suite asserting 'at
most one name-carrying delta per `call_id`' would fail here and pass for litellm;
closing it needs its own ticket."* This is that ticket.

The shape is malformed — an `id` identifies a call, so two indexes sharing one is
contradictory — and is unobserved from any backend.

### 1.1 Why the SMA-533 conformance suite is green

The suite's fixture-provenance rule is that envelope shapes are transcribed from
traffic captured into committed fixtures, never invented. This shape has no capture
anywhere in the repo, so it is not in the fixture set, and assertion 7 ("at most one
name-carrying `ToolCallDelta` per `call_id`") passes for `openai_chat` vacuously,
without an `#[ignore]`.

This mirrors litellm exactly. `tests/provider-stream-conformance/tests/conformance.rs`
records under *"`canonicalize`'s SMA-550 regression coverage lives in the crate's own
unit tests, not here"* that stubbing `canonicalize` into an identity function left
`litellm::conforms` **green** while failing 11 unit tests in the crate under test.
The regression guard for this class of defect belongs in the crate's own test module,
not in the conformance suite.

## 2. Decision

**Canonicalize `openai/chat`'s correlation state onto the resolved `call_id`, the way
SMA-550 did for `providers-litellm`**, so that one `call_id` structurally owns exactly
one state entry.

The alternative the ticket offers — a `call_id`-level dedup net in
`flush_buffered_names` — was considered and rejected. It satisfies the letter of the
acceptance criterion while leaving the two translators observably different: given the
malformed shape, a dedup net *suppresses* the second name (emitting `"alpha"`) where
litellm *merges* it (emitting `"alphabeta"`). That converts a documented divergence
into a subtler one rather than closing it, and it makes the invariant a guard rather
than a property of the keying.

### 2.1 Correction to the initial sketch

The design was first sketched as two typed maps — `HashMap<u32, _>` for pre-id
staging, `HashMap<String, _>` for canonical post-id state — on the reasoning that
`ChatCompletionMessageToolCallChunk::index` is a required `u32`, so `openai/chat` has
only one wire key space and needs no `Key` enum.

**That is wrong, and the blank-`id` case is why.** Today `openai/chat` records
`"id": ""` into `tool_calls` and emits `ToolCallDelta { call_id: "" }`. Canonicalizing
on the raw `String` would collapse *every* parallel blank-`id` call into a single `""`
slot, and all but the first would vanish from the stream entirely — strictly worse
than the bug being fixed. litellm hit this and guards it in `canonicalize`, pinned by
`blank_ids_do_not_collapse_distinct_calls`.

Keeping such deltas on their wire key while canonicalizing the rest requires a key
that can be *either* shape. The `Key` enum is therefore load-bearing here too.

## 3. Design

### 3.1 Correlation key

```rust
/// Correlation key for a streaming tool call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    /// The wire key. `ChatCompletionMessageToolCallChunk::index` is a required
    /// `u32`, so every delta on this backend arrives under this variant —
    /// unlike `providers-litellm`, where `index` is optional and `Key::Id` is
    /// also reachable from the wire. Stays canonical only for a call whose
    /// `id` never resolved, or resolved blank.
    Index(u32),
    /// The canonical key for every call whose `id` resolved non-blank. Never a
    /// wire key in this crate.
    Id(String),
}
```

All four correlation maps re-key from `u32` to `Key`:

```rust
tool_calls:       HashMap<Key, String>,
name_emitted:     HashMap<Key, String>,
warned_late_name: HashSet<Key>,
pending:          HashMap<Key, PendingToolCall>,
warned_blank_id:  HashSet<Key>,   // new
```

`tool_calls` holds **both** the wire mapping `Index(i) -> call_id` (so later
index-only deltas keep resolving) and the canonical self-mapping `Id(c) -> c`. The
self-mapping is load-bearing: `flush_buffered_names` resolves a pending key through
`tool_calls`, and without it a canonical key resolves to nothing, so the call is
skipped at flush.

### 3.2 `PendingToolCall` gains `seq`, loses `Default`

```rust
struct PendingToolCall {
    /// Monotonic creation order across all buffers in one stream.
    seq: u64,
    name: String,
    args: String,
}
```

`seq` serves two purposes: merge order in `canonicalize`, and a deterministic
end-of-stream order in `flush_buffered_names`.

**The `Default` derive is removed deliberately.** Every buffer must carry the `seq` it
was created with, so construction goes through `PendingToolCall::new(seq)` via
`ChatTranslator::ensure_pending`. Deriving `Default` would let an `.or_default()` call
site — of which the current code has two — silently mint a buffer with `seq: 0` and
corrupt the merge order. The absence of the derive makes that a compile error rather
than a latent bug. This mirrors litellm's `Pending` verbatim, including the rationale.

**`seq`, not the wire `index`, is the sort key.** Sorting by `index` was considered
and rejected: `index` is the *declared* position of a call, not the arrival order of
its fragments. A call whose `index: 1` fragment arrives before its `index: 0` fragment
merges backwards under an index sort. Concretely, given

```text
chunk 1: {index: 1,           function: {name: "beta"}}      -> staged under Index(1)
chunk 2: {index: 0, id: "c1", function: {name: "alpha"}}     -> creates Id("c1")
chunk 3: {index: 1, id: "c1", function: {name: "gamma", ...}} -> migrates Index(1)
```

the wire order of the two buffers is `beta` then `alpha`, but `0 < 1` would append and
yield `"alphabeta"`. Creation order (`seq`) yields `"betaalpha"`, which is faithful.

### 3.3 `canonicalize`

```rust
fn canonicalize(&mut self, key: Key, call_id: &str) -> Key
```

Mirrors `providers-litellm`'s function of the same name:

1. **Blank-`id` guard.** If `call_id.is_empty()`, warn once per key (via
   `warned_blank_id`) and return `key` unchanged. An empty `id` is not an identity;
   leaving such deltas on their wire key keeps distinct calls distinct.
2. **Already canonical.** If `key` is `Key::Id(id)` with `id == call_id`, return
   without allocating. Unreachable from the wire on this backend, since `index` is
   required — kept because it is cheap and makes the function total.
3. **Migrate.** If `pending` holds a buffer under `key`, remove it and merge into
   `pending[Id(call_id)]` (created via `ensure_pending` if absent):
   - `args` prepends via `insert_str(0, ..)`, guarded by a `debug_assert!` that the
     canonical slot's `args` is empty — a resolved slot drains `args` on every delta,
     so this is an assignment in practice. The assert exists so a future relaxation of
     drain-once surfaces for a deliberate re-decision.
   - A migrating `name` whose canonical slot **already emitted** cannot reach a
     consumer. It is dropped *loudly* — `warn!` once per call via `warned_late_name` —
     never silently, which would recreate the undiagnosed loss SMA-550 exists to
     eliminate.
   - A migrating `name` identical to what the canonical slot already holds is a
     whole-name repeat, not a continuation, and is skipped — the same reasoning as the
     SMA-547 wire-path guard.
   - Otherwise merge by `seq`: `old.seq < slot.seq` prepends, else appends. Both
     orderings are reachable; a plain prepend or a plain append is wrong in exactly
     one of them.
   - `slot.seq` claims `old.seq` whenever the migrating buffer is older, **even if its
     name was empty or a repeat** — `seq` carries the call's wire position for flush
     order independently of whether any name text moved.
4. **Register the self-mapping** `Id(call_id) -> call_id` in `tool_calls`.

### 3.4 `handle_tool_call_chunk`

The body changes only where it must:

- Key construction becomes `Key::Index(tc.index)`.
- Id registration gains litellm's replacement rule: **first id wins, except that a
  real id replaces one already recorded as blank.** Without this, a blank recorded
  first sticks forever, the call reaches the consumer under an empty `call_id`, and
  canonicalization can never happen for that call. Today's code has no such rule
  (`self.tool_calls.get(&index)` short-circuits on any recorded value, `""` included).
- After the `call_id` resolves, `let key = self.canonicalize(key, &call_id);`. From
  that line on, one `call_id` owns exactly one state entry.
- Buffer access moves from `self.pending.entry(key).or_default()` to
  `self.ensure_pending(&key)` + `self.pending.get_mut(&key)`. `ensure_pending` returns
  nothing rather than `&mut PendingToolCall` so callers borrow one field instead of
  all of `self`, leaving the disjoint borrows of `name_emitted` and `tool_calls`
  intact.

Everything downstream of that — the late-name warn, the `already_emitted` short-
circuit, the whole-name-repeat guard, drain-once `args`, the flush condition, the
empty-event suppression — is unchanged in logic, only re-keyed. **All four SMA-547
invariants must survive verbatim**, and the existing 18 tests in the module are the
proof.

### 3.5 `flush_buffered_names`

- Sorts by `seq` rather than by key. After canonicalization most resolved keys are
  `Key::Id(call_id)`, so sorting by key would mean lexicographic-by-`call_id` —
  silently reordering parallel calls against the wire.
- Keeps a `call_id`-level `already: HashSet<String>` net, seeded from `name_emitted`,
  claimed only once a key is known to have a name to flush. Since canonicalization
  gives each `call_id` one key this is unreachable; it is kept because it enforces the
  invariant *at the point of emission*, independent of the keying discipline upstream
  — which is precisely what a cross-provider conformance suite asserts. It logs at
  `error!` rather than `continue`-ing bare, so a future loosening of the keying is
  loud rather than a silent drop.

## 4. Observable behaviour change

Only for the malformed shape. Given

```json
[{"index": 0, "id": "c1", "function": {"name": "alpha"}},
 {"index": 1, "id": "c1", "function": {"name": "beta", "arguments": "{}"}}]
```

| | before | after |
|---|---|---|
| `openai/chat` | `Some("alpha")` **and** `Some("beta")` | `Some("alphabeta")` |
| `litellm` | `Some("alphabeta")` | `Some("alphabeta")` (unchanged) |

The merged name is not "correct" in any deep sense — the input is contradictory — but
it is **one name for one `call_id`**, which is the invariant, and it is byte-identical
to what `providers-litellm` already produces.

For every well-formed shape, output is unchanged. That is the load-bearing claim of
this design, and §6's regression suite is what establishes it.

## 5. Scope

### 5.1 In scope

- `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` — the change and its
  tests.
- `crates/paigasus-helikon-providers-litellm/src/stream.rs` — **doc-only.** The doc
  comment on `two_indexes_with_one_id_merge_into_a_single_call` currently asserts
  *"`providers-openai` emits TWO names here; see the divergence comment in its
  `chat.rs`."* This change makes that false; leaving it would strand a reader at a
  divergence comment that no longer exists.
- `tests/provider-stream-conformance/tests/conformance.rs` — **doc-only.** The
  `openai_chat` module gains the counterpart to `litellm`'s *"regression coverage
  lives in the crate's own unit tests, not here"* note, so a future reader who
  mutates `canonicalize` and finds `openai_chat::conforms` still green knows where
  the real guard is.

This satisfies the ticket's third acceptance criterion by the first of its two
branches: alignment is restored, so the divergence comments are replaced with a record
of the alignment rather than being kept and expanded.

### 5.2 Out of scope — each a conscious call

- **`backend/responses.rs`.** It has the same *class* of gap: `name_emitted` is keyed
  by `item_id`, so two `item_id`s sharing one `call_id` would emit two names. But the
  Responses wire delivers `item.id` and `item.call_id` **together** on
  `output_item.added`, making the mapping 1:1 by construction rather than resolved
  over time. There is no analogue of "the id arrives late". The ticket names
  `openai/chat`, in its title, body, and acceptance criteria.
- **A new conformance-suite scenario.** The shape has no capture, and the suite's
  provenance rule forbids invented fixtures. The litellm precedent explicitly parks
  this guard in the crate's unit tests.
- **`warn_unresolved_pending`.** litellm warns when the stream ends with buffered
  fragments whose `id` never resolved; `openai/chat` silently `continue`s. A real
  diagnostic gap, but a different one, and porting it would put a new warning on every
  stream shape this ticket does not touch.
- **mdBook and crate READMEs.** No public API, feature flag, or crate-roster change;
  the behaviour delta is confined to a wire shape no backend emits. Per CLAUDE.md this
  is a pure-internal correctness change, and the skip is deliberate rather than silent.

## 6. Testing

New tests in `chat.rs`'s `mod tests`, mirroring litellm's SMA-550 set and adapted to
`openai/chat`'s required `index`. Each is to be **verified to fail against the current
translator**, with the observed pre-fix output recorded in its doc comment — the
ticket's second acceptance criterion, and the same discipline litellm's suite follows.

A `drive`-style helper is needed: the invariant is per-stream, so an assertion made
only over `finish()` passes vacuously against the pre-fix translator, whose violation
is two *mid-stream* emissions. Assertions collect across `handle_tool_call_chunk` and
`finish` together.

1. `two_indexes_with_one_id_merge_into_a_single_call` — **the acceptance-criterion
   test.** Two deltas, different `index`, same `id`; asserts exactly one
   name-carrying `ToolCallDelta` for that `call_id`.
2. `dual_key_call_emits_at_most_one_name_mid_stream` — the same invariant with the
   flush happening mid-stream rather than at `finish`, so the test cannot pass for the
   wrong reason.
3. `name_fragments_split_across_the_key_boundary_reassemble` — the fragment buffered
   under the pre-canonical key must survive, not be discarded.
4. `id_keyed_buffer_created_before_the_index_keyed_one_merges_in_order` and
   `index_keyed_buffer_created_first_merges_in_order` — the pair that forces `seq`.
   A naive prepend passes one and yields `"weathget_er"` on the other; a naive append
   does the reverse.
5. `blank_ids_do_not_collapse_distinct_calls` — two distinct calls both carrying
   `"id": ""` must stay distinct. Guards the §2.1 regression.
6. `a_real_id_replaces_a_blank_one_on_the_same_wire_key` — and the buffered name
   survives the replacement.
7. `canonical_key_resolves_through_tool_calls` — pins the `Id(c) -> c` self-mapping,
   without which a canonical key is skipped at flush.
8. `flush_order_follows_the_wire_not_the_call_id` — two parallel calls whose
   `call_id`s sort lexicographically against their wire order; pins the `seq` sort in
   `flush_buffered_names`.
9. `interleaved_dual_keying_is_lossless` — the accepted residual. When the two keys
   interleave at *fragment* level, no buffer-level order reconstructs the wire
   sequence, so the merge misorders fragments while losing none. Pinned deliberately
   rather than left undefined, matching litellm.

**The existing 18 tests in the module are the primary regression guard** and must pass
unmodified. If any needs editing to accommodate the re-keying, that is a signal the
change altered well-formed behaviour and must be re-examined, not accommodated.

Full local gate before the PR, per CLAUDE.md: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-features --all-targets -- -D warnings`,
`cargo test --workspace --all-features`, and
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`.

## 7. Risks

- **Re-keying touches every correlation site in the file.** The SMA-547 invariants are
  subtle and their guards read as redundant in isolation. Mitigation: the change is
  mechanical below `canonicalize`, and the existing 18 tests must pass unmodified.
- **`Key::Id` is unreachable from the wire on this backend**, which makes part of
  `canonicalize` dead relative to litellm's version. Mitigation: document it as such
  at the enum rather than deleting it — the branch is what keeps the two files
  diffable, and it costs nothing.
- **Version-bump discipline.** This touches `providers-openai` and `providers-litellm`
  (doc-only). Per CLAUDE.md the normal flow is to let release-plz do the bumping — no
  hand-bump here, so no facade or `core` cascade to manage.
