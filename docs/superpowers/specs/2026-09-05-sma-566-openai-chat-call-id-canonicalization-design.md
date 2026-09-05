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
anywhere in the repo, so it is not in the fixture set, and the assertion passes for
`openai_chat` vacuously, without an `#[ignore]`.

The assertion is **"exactly once"**, not "at most one": `check.rs` emits
`Violation::ToolNameNotExactlyOnce { call_id, count }` and fires on `count: 0` as
well as `count: 2`. The ticket's phrasing ("at most one") is the looser property; this
spec asserts the tighter one where it holds, and §3.6 records the one case where it
cannot hold either before or after this change.

This mirrors litellm exactly. `tests/provider-stream-conformance/tests/conformance.rs`
records under *"`canonicalize`'s SMA-550 regression coverage lives in the crate's own
unit tests, not here"* that stubbing `canonicalize` into an identity function left
`litellm::conforms` **green** while failing 11 unit tests in the crate under test.
The regression guard for this class of defect belongs in the crate's own test module,
not in the conformance suite.

## 2. Decision

**Canonicalize `openai/chat`'s correlation state onto the resolved `call_id`**, so
that one `call_id` structurally owns exactly one state entry — matching the
*observable behaviour* SMA-550 gave `providers-litellm`.

The alternative the ticket offers — a `call_id`-level dedup net in
`flush_buffered_names` — was considered and rejected. It satisfies the letter of the
acceptance criterion while leaving the two translators observably different: given the
malformed shape, a dedup net *suppresses* the second name (emitting `"alpha"`) where
litellm *merges* it (emitting `"alphabeta"`). That converts a documented divergence
into a subtler one rather than closing it, and it makes the invariant a guard rather
than a property of the keying.

### 2.1 The canonical key is an index alias, not a `Key` enum

Two earlier sketches were rejected during design review. Both are recorded here
because the reasoning that kills them is the reasoning that justifies what remains.

**Sketch 1 — canonicalize onto the `String` call_id.** Fatal: `openai/chat` records
`"id": ""` into `tool_calls` today and emits `ToolCallDelta { call_id: "" }`.
Canonicalizing on the raw `String` collapses *every* parallel blank-`id` call into a
single `""` slot, and all but the first vanish from the stream entirely — strictly
worse than the bug being fixed. litellm hit this and guards it, pinned by
`blank_ids_do_not_collapse_distinct_calls`.

**Sketch 2 — port litellm's `Key { Index(u32), Id(String) }` enum.** This survives the
blank-`id` hazard by leaving such deltas on their wire key, and it is what litellm
does. But it is *not required* to survive it, and it carries three costs that the
third design does not:

1. `Key::Id` is unreachable from the wire on this backend, because
   `ChatCompletionMessageToolCallChunk::index` is a required `u32`. The variant would
   exist solely as a canonicalization target — a dead branch relative to litellm's.
2. Re-keying `name_emitted` and `warned_late_name` from `u32` to `Key` breaks
   compilation of `late_name_fragment_warns_once` (`chat.rs:1086-1092`), which asserts
   `t.warned_late_name.contains(&0)` and `t.name_emitted.get(&0)`. `HashSet<Key>` has
   no `Borrow<u32>`, so this is a compile error, not a behavioural signal.
3. `flush_buffered_names` currently sorts `Vec<u32>` of indices ascending
   (`chat.rs:351-352`) — the model's *declared* call positions. Under `Key` keying the
   sort must move to a synthetic creation counter, silently changing end-of-stream
   emission order for well-formed streams whenever arrival order differs from index
   order.

**The adopted design: alias the wire index onto the first index that owned the
`call_id`.** Every map stays keyed by `u32`. One new map records the alias:

```rust
canonical: HashMap<String /* non-blank call_id */, u32 /* owning wire index */>,
```

On resolving a non-blank `id` for wire index `i`, the owning index is
`*canonical.entry(call_id).or_insert(i)`; if it differs from `i`, `pending[i]` migrates
into `pending[owner]` and `owner` is the key used for everything downstream.

This is not a weaker version of litellm's fix — it is the same fix expressed in the
key space this backend actually has. Because `index` is required here, the wire has
exactly one key space, and canonicalization is a *many-to-one map within* that space
rather than a *migration between* two spaces. Consequences, each of which is a cost
sketch 2 pays and this does not:

- **Blank ids never enter `canonical`**, so the §2.1 collapse hazard is eliminated
  structurally rather than by a guard.
- **The existing 18 tests compile and pass unmodified**, white-box assertions
  included, because the key type never changes.
- **`flush_buffered_names` keeps its index sort**, so end-of-stream order for
  well-formed streams is byte-for-byte what it is today.
- **No dead branch.** There is no unreachable variant to document.

The cost is that `chat.rs` and `stream.rs` are no longer line-by-line diffable in
this region. That is the honest trade, and it is the right one: the two files
*should* differ here, because the wire shapes they parse differ in exactly the way
that motivated litellm's enum. **Alignment is a claim about observable behaviour, not
about implementation shape**, and §4 is where that claim is made and tested.

## 3. Design

### 3.1 State

```rust
pub(crate) struct ChatTranslator {
    /// Wire index -> call_id. Unchanged in type and meaning.
    tool_calls: HashMap<u32, String>,
    /// Non-blank call_id -> the wire index that owns its state. NEW.
    ///
    /// The first index to resolve a given call_id becomes its owner; every
    /// later index resolving the same call_id aliases onto it. Blank ids are
    /// never inserted -- see `canonicalize`.
    canonical: HashMap<String, u32>,
    /// Canonical index -> the tool name already emitted to the consumer.
    name_emitted: HashMap<u32, String>,
    /// Canonical indices whose late-name warning has already fired.
    warned_late_name: HashSet<u32>,
    /// Wire indices whose blank-id warning has already fired. NEW.
    warned_blank_id: HashSet<u32>,
    /// Canonical index -> buffered name/args.
    pending: HashMap<u32, PendingToolCall>,
    /// Next value handed out by `ensure_pending`; never reused. NEW.
    next_seq: u64,
    finish_reason: Option<FinishReason>,
}
```

### 3.2 `PendingToolCall` gains `seq`, loses `Default`

```rust
struct PendingToolCall {
    /// Monotonic creation order across all buffers in one stream.
    seq: u64,
    name: String,
    args: String,
}
```

**`seq` has exactly one consumer: merge order in `canonicalize`.** It is *not* the
flush sort key — `flush_buffered_names` keeps sorting by the canonical index, which is
the model's declared call position and is what it sorts by today. This is a
deliberate divergence from litellm, where `seq` serves both purposes because its
`index` is optional and may be absent entirely.

Merge order cannot use the index. The index is the *declared* position of a call, not
the arrival order of its fragments, and the two come apart:

```text
chunk 1: {index: 1,           function: {name: "beta"}}       -> buffers under 1
chunk 2: {index: 0, id: "c1", function: {name: "alpha"}}      -> owner of "c1" := 0
chunk 3: {index: 1, id: "c1", function: {name: "gamma", ...}} -> aliases 1 -> 0
```

The wire order of the two buffers is `beta` then `alpha`, but `0 < 1` would append and
yield `"alphabeta"`. Creation order yields `"betaalpha"`, which is faithful.

**Both merge branches are reachable, but only under the malformed shape.** The append
branch requires `pending[owner]` to pre-exist with a lower `seq` than the migrating
buffer, which forces two distinct wire indexes to resolve to one `id` — the malformed
shape itself. For every well-formed stream, migration never runs at all. §6's tests 3
and 4 are the pair that pins both branches; a naive unconditional prepend passes one
and yields `"weathget_er"` on the other, and a naive append does the reverse.

**The `Default` derive is removed deliberately.** Every buffer must carry the `seq` it
was created with, so construction goes through `PendingToolCall::new(seq)` via
`ChatTranslator::ensure_pending`. Deriving `Default` would let an `.or_default()` call
site — of which the current code has two — silently mint a buffer with `seq: 0` and
corrupt the merge order. The absence of the derive makes that a compile error rather
than a latent bug. This mirrors litellm's `Pending`, including the rationale.

### 3.3 `canonicalize`

```rust
/// Resolve the wire `index` to the canonical index owning `call_id`,
/// migrating any fragments buffered under the wire index.
fn canonicalize(&mut self, index: u32, call_id: &str) -> u32
```

1. **Blank-`id` guard.** If `call_id.is_empty()`, warn once per wire index (via
   `warned_blank_id`) and return `index` unchanged. An empty `id` is not an identity;
   leaving such deltas on their wire index keeps distinct calls distinct.
2. **Claim or look up the owner.** `let owner = *self.canonical.entry(call_id.to_owned())
   .or_insert(index);` If `owner == index`, return immediately — the common path for
   every well-formed stream, where no migration ever occurs.
3. **Migrate.** If `pending` holds a buffer under `index`, remove it and merge into
   `pending[owner]` (created via `ensure_pending` if absent):
   - `args` prepends via `insert_str(0, ..)`, guarded by a `debug_assert!` that the
     canonical slot's `args` is empty — a resolved slot drains `args` on every delta,
     so this is an assignment in practice. The assert exists so a future relaxation of
     drain-once surfaces for a deliberate re-decision.
   - A migrating `name` whose canonical slot **already emitted** cannot reach a
     consumer. It is dropped *loudly* — `warn!` once per call via `warned_late_name` —
     never silently, which would recreate the undiagnosed loss SMA-550 exists to
     eliminate.
   - A migrating `name` identical to what the canonical slot already holds is a
     whole-name repeat, not a continuation, and is skipped. Same reasoning as the
     SMA-547 wire-path guard: without it a backend resending the complete name yields
     `"searchsearch"`, which resolves to no registered tool.
   - Otherwise merge by `seq`: `old.seq < slot.seq` prepends, else appends.
   - `slot.seq` claims `old.seq` whenever the migrating buffer is older, **even if its
     name was empty or a repeat** — `seq` must stay accurate for any subsequent
     merge into the same slot.

**Accepted residual.** The owner is the index that resolved the `id` *first*, not the
numerically lowest index carrying it. Given `{index: 5, id: "c1"}` before
`{index: 0, id: "c1"}`, the merged call flushes at position 5. Reachable only under
the malformed shape, where "declared position" is already incoherent; re-keying the
map entry to chase the minimum is not worth the path.

### 3.4 `handle_tool_call_chunk`

The id-resolution chain is the one part that is rewritten rather than re-keyed. Today
it is a single `if let / else if let / else` (`chat.rs:438-449`) that short-circuits on
any recorded value. The target shape separates registration from resolution:

```rust
// Register or replace the id for this wire index.
if let Some(id) = tc.id.as_deref() {
    match self.tool_calls.get_mut(&index) {
        // A real id replaces one already recorded as blank; otherwise first
        // id wins, so a backend that changes a call's id mid-stream cannot
        // re-point an in-flight call.
        Some(existing) if existing.is_empty() && !id.is_empty() => *existing = id.to_owned(),
        Some(_) => {}
        None => { self.tool_calls.insert(index, id.to_owned()); }
    }
}

let Some(call_id) = self.tool_calls.get(&index).cloned() else {
    // No id yet -- buffer both fragments under the wire index.
    self.ensure_pending(index);
    let e = self.pending.get_mut(&index).expect("ensure_pending just inserted");
    e.name.push_str(name_frag);
    e.args.push_str(args_frag);
    return;
};

// From here on, one call_id owns exactly one state entry.
let index = self.canonicalize(index, &call_id);
```

`ensure_pending` returns nothing rather than `&mut PendingToolCall` so callers reach
the buffer through `self.pending.get_mut(..)`, borrowing one field instead of all of
`self` and leaving the disjoint borrows of `name_emitted` and `tool_calls` intact.

Everything downstream — the late-name warn, the `already_emitted` short-circuit, the
whole-name-repeat guard, drain-once `args`, the flush condition, the empty-event
suppression — is unchanged in both logic and key type. **All four SMA-547 invariants
must survive verbatim**, and the existing 18 tests are the proof.

### 3.5 The blank-id replacement rule is a separate, deliberate inclusion

The `existing.is_empty()` arm above is **not** required to fix the two-index defect. A
call stuck at `call_id: ""` simply never enters `canonical` and never aliases; the
two-index shape is fixed without it.

It is included because without it a blank recorded on delta 1 sticks for the life of
the stream, and the call reaches the consumer under an empty `call_id` even though the
backend supplied a real one later — an id the agent loop cannot submit a tool result
against. litellm has the rule (`stream.rs:415-431`); omitting it here would leave a
second, quieter divergence in the same file this change exists to align.

It is called out separately because it is a **real output change on a real field**
(§4, row 2) for a shape the ticket does not name. It is severable: dropping it removes
one arm and one test and changes nothing else in this design.

### 3.6 `flush_buffered_names`

- **The index sort is unchanged** (`indices.sort_unstable()`), so end-of-stream order
  for every well-formed stream is exactly what it is today. This is the concrete
  payoff of §2.1's key choice.
- Adds a `call_id`-level `already: HashSet<String>` net, seeded from `name_emitted`
  resolved through `tool_calls`, claimed only once a key is known to have a name to
  flush. Since aliasing gives each non-blank `call_id` one index this is unreachable;
  it is kept because it enforces the invariant *at the point of emission*,
  independent of the keying discipline upstream — which is precisely what a
  cross-provider conformance suite asserts. It logs at `error!` rather than
  `continue`-ing bare, so a future loosening of the keying is loud rather than a
  silent drop.
- **The net must skip blank `call_id`s.** Blank ids deliberately bypass
  canonicalization (§3.3 step 1), so two parallel blank-id calls both resolve to
  `""`. An unguarded `already.insert(call_id)` would drop the second call's name and
  fire an `error!` blaming a keying regression that did not happen — a net-new loss on
  the exact shape §2.1 exists to protect. The condition is therefore
  `!call_id.is_empty() && !already.insert(call_id.clone())`, with the seeding filtered
  the same way. Pinned by §6 test 8.

  The at-most-one invariant is thus scoped to **non-blank** `call_id`s. Two blank-id
  calls violate the conformance suite's "exactly once" assertion both before and after
  this change; that is a property of an id that cannot identify, not something this
  change can fix. `providers-litellm` carries the identical hole
  (`stream.rs:554-597`) — see §5.3.

## 4. Observable behaviour change

Two changes, both confined to shapes no backend is observed to emit.

| # | Input | Before | After |
|---|---|---|---|
| 1a | `[{index:0,id:"c1",name:"alpha"},`<br>`{index:1,id:"c1",name:"beta",arguments:"{}"}]` | `Some("beta")`, then `Some("alpha")` at EOS — **two names** | `Some("alphabeta")` — one name |
| 1b | `[{index:0,id:"c1",name:"get_",arguments:"{"},`<br>`{index:1,id:"c1",name:"weather",arguments:"}"}]` | `Some("get_")`, then `Some("weather")` — **two names** | `Some("get_")`, and `"weather"` dropped with a `warn!` |
| 2 | `{index:0,id:"",name:"foo"}` then<br>`{index:0,id:"c1",arguments:"{}"}` | `ToolCallDelta { call_id: "" }` | `ToolCallDelta { call_id: "c1" }` |

**Rows 1a and 1b differ, and the difference is not incidental.** Whether the second
index's name is *merged* or *dropped with a warning* hinges on whether the canonical
slot has already emitted. In 1a the first delta carries no arguments, so nothing
flushes and the fragments merge. In 1b it does, so the name is already downstream and
unrecoverable. Both yield one name per `call_id`; only 1a matches litellm's
`"alphabeta"` character-for-character, and 1b matches litellm's behaviour on the same
input for the same reason. An implementer must not write test 2 expecting
`"get_weather"`.

**Row 2 is the §3.5 inclusion** and is severable from the rest.

**For every well-formed shape, output is unchanged — including end-of-stream emission
order.** That is the load-bearing claim of this design, and it rests on two specific
properties: the key type never changes (§2.1), and `flush_buffered_names` keeps its
index sort (§3.6). The existing 18 tests passing unmodified is what establishes it.

**On downstream consumption.** `ModelTurnAccumulator::observe` (`core/src/model.rs:582-590`)
is first-name-wins and stores tool calls in a `BTreeMap<String, _>` emitted in
`call_id`-sorted order, so it discards emission order entirely. For row 1a it
accumulates `"beta"` before and `"alphabeta"` after. Neither is a dispatchable tool
name — the input is contradictory — so this is not an argument for either behaviour,
only a note that no in-tree consumer observes the ordering that §6 test 11 pins.

## 5. Scope

### 5.1 In scope

- `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` — the change and its
  tests.
- `crates/paigasus-helikon-providers-litellm/src/stream.rs` — **doc-only.** The doc
  comment on `two_indexes_with_one_id_merge_into_a_single_call` currently asserts
  *"`providers-openai` emits TWO names here; see the divergence comment in its
  `chat.rs`."* This change makes that false; leaving it would strand a reader at a
  divergence comment that no longer exists. The replacement must record that the two
  translators now agree observably while differing structurally, and why (§2.1).
- `tests/provider-stream-conformance/tests/conformance.rs` — **doc-only.** The
  `openai_chat` module gains the counterpart to `litellm`'s *"regression coverage
  lives in the crate's own unit tests, not here"* note. Before writing it, confirm
  whether `src/lib.rs` or `src/check.rs` also assert the divergence, or whether the
  `openai_chat` module doc is the only site.

This satisfies the ticket's third acceptance criterion by the first of its two
branches: alignment is restored, so the divergence comments are replaced with a record
of the alignment rather than being kept and expanded.

### 5.2 Out of scope — each a conscious call

- **`backend/responses.rs`.** It has the same *class* of gap: `name_emitted` is
  `HashSet<String>` keyed by `item_id` (`responses.rs:263`), so two `output_item.added`
  events with different `item.id` and the same `call_id` would emit two names
  (`responses.rs:445-460`). The Responses wire delivers `item.id` and `item.call_id`
  together, which removes the "id arrives late" sub-problem — but that is *atomicity*,
  not *injectivity*, and it does not close the gap. **The reason this is out of scope
  is scope**: the ticket names `openai/chat` in its title, body, and acceptance
  criteria. The suite has an `openai_responses` subject and the assertion applies to
  it, so this should be ticketed rather than left in a comment.
- **A new conformance-suite scenario.** The shape has no capture, and the suite's
  provenance rule forbids invented fixtures. The litellm precedent explicitly parks
  this guard in the crate's unit tests.
- **`warn_unresolved_pending`.** litellm warns when the stream ends with buffered
  fragments whose `id` never resolved; `openai/chat` silently `continue`s. A real
  diagnostic gap, but a different one, and porting it would put a new warning on
  stream shapes this change does not otherwise alter. The new `warned_blank_id`
  warning (§3.3) is not held to the same standard because this change *does* alter
  what happens on a blank id — it now bypasses canonicalization, and that decision is
  worth surfacing at the point it is taken.
- **Multi-`choice` responses.** `openai/chat`'s `consume` iterates **all** choices
  (`chat.rs:273`), unlike litellm which reads only the first and warns on `n > 1`.
  Tool-call `index` is per-choice, so with `n > 1` two choices' `index: 0` collide in
  one key space. Pre-existing, not made worse here, and untouched — but it means
  §3's "one `call_id` owns exactly one state entry" holds within a single choice.
- **mdBook and crate READMEs.** No public API, feature flag, or crate-roster change;
  the behaviour delta is confined to wire shapes no backend emits. Per CLAUDE.md this
  is a pure-internal correctness change, and the skip is deliberate rather than silent.

### 5.3 The blank-id flush hole in `providers-litellm`

§3.6's `already` net exists in litellm today **without** the blank-id guard
(`stream.rs:554-597`), so two parallel zero-argument blank-id calls lose the second
name there. Fixing it is a two-line change in a file this PR already touches
doc-only — but it is a behaviour change to a second crate for a defect the ticket
does not name, and it would turn a doc-only edit into a code edit with its own
version bump. **Recommendation: ticket it, do not fix it here**, and cite the new
ticket from §3.6's comment so the asymmetry is deliberate and findable.

## 6. Testing

New tests in `chat.rs`'s `mod tests`, in two labelled groups. The distinction matters:
the ticket's second acceptance criterion requires the defect test to be *verified to
fail* against the current translator, and a blanket claim that all new tests fail
pre-fix would be false for the regression guards.

A `drive`-style helper is needed: the invariant is per-stream, so an assertion made
only over `finish()` passes vacuously against the pre-fix translator, whose violation
is two *mid-stream* emissions. Assertions collect across `handle_tool_call_chunk` and
`finish` together, via a `named(&evs) -> Vec<(call_id, name)>` projection.

### Group A — defect proofs (must FAIL pre-fix; record the observed pre-fix output)

1. `two_indexes_with_one_id_merge_into_a_single_call` — **the acceptance-criterion
   test.** `[{index:0,id:"c1",name:"alpha"}, {index:1,id:"c1",name:"beta",arguments:"{}"}]`.
   Asserts `named == [("c1", "alphabeta")]` — the **value**, not merely the count; a
   count-only assertion is also satisfied by the dedup-net design §2 rejects.
   Pre-fix: `[("c1","beta"), ("c1","alpha")]`.
2. `dual_key_call_emits_at_most_one_name_mid_stream` — the same invariant with the
   flush happening mid-stream. `[{index:0,id:"c1",name:"get_",arguments:"{"},
   {index:1,id:"c1",name:"weather",arguments:"}"}]` → `[("c1","get_")]`, args `"{}"`.
   **Expected value is `"get_"`, not `"get_weather"`** — see §4 row 1b.
   Pre-fix: `[("c1","get_"), ("c1","weather")]`.
3. `fragment_buffered_under_a_second_index_is_not_stranded` — the **prepend** branch.
   `{index:1,name:"beta"}` → `{index:0,id:"c1",name:"alpha"}` →
   `{index:1,id:"c1",arguments:"{}"}` → `[("c1","betaalpha")]`.
   Pre-fix: `[("c1","beta"), ("c1","alpha")]`.
4. `owner_index_buffered_first_appends_on_merge` — the **append** branch, mirror of 3.
   `{index:0,id:"c1",name:"alpha"}` → `{index:1,name:"beta"}` →
   `{index:1,id:"c1",arguments:"{}"}` → `[("c1","alphabeta")]`. A naive prepend yields
   `"betaalpha"` here; a naive append yields `"alphabeta"` in test 3. This pair is
   what forces `seq`. Pre-fix: `[("c1","beta"), ("c1","alpha")]`.
5. `repeated_whole_name_is_not_doubled_across_the_alias_boundary` —
   `{index:1,name:"search"}` → `{index:0,id:"c1",name:"search"}` →
   `{index:1,id:"c1",arguments:"{}"}` → `[("c1","search")]`. Without the migration
   repeat guard: `"searchsearch"`. Pre-fix: two names.
6. `fragment_migrating_into_an_emitted_slot_is_reported_not_stranded` — the loud-drop
   path. Asserts one name **and** that `warned_late_name` recorded the canonical index.
7. `a_real_id_replaces_a_blank_one_on_the_same_wire_key` — §3.5 / §4 row 2. The
   emitted `call_id` becomes `"c1"` and the buffered name survives.
   Pre-fix: `call_id: ""`. **Drop this test if §3.5 is descoped.**

### Group B — regression guards (must pass BOTH pre- and post-fix)

8. `blank_ids_do_not_collapse_at_end_of_stream` — two parallel **zero-argument**
   blank-id calls; both names must survive. Zero-argument is load-bearing: with
   arguments both flush mid-stream and never reach `flush_buffered_names`, so the
   §3.6 net is never exercised. This is the guard for the net's blank-id hole.
9. `blank_ids_do_not_collapse_distinct_calls` — the mid-stream counterpart.
10. `migrated_buffer_keeps_its_creation_order` — pins `slot.seq = old.seq`.
    `{index:0,name:"alpha"}` → `{index:1,id:"c2",name:"bravo"}` →
    `{index:0,id:"c1",name:"_x"}` and a subsequent merge into the same slot.
11. `flush_order_follows_the_wire_index` — two parallel calls whose `call_id`s sort
    lexicographically *against* their index order. Pins §3.6's unchanged sort. (Note
    per §4 that no in-tree consumer observes this ordering; it is pinned because the
    conformance suite reads the event sequence directly.)
12. `interleaved_aliasing_is_lossless` — the accepted residual. When two indexes for
    one id interleave at *fragment* level, no buffer-level order reconstructs the wire
    sequence, so the merge misorders fragments while losing none. Pinned deliberately
    rather than left undefined, matching litellm.

### The existing 18 tests

They are the primary regression guard and **must pass unmodified** — including the
white-box assertions in `late_name_fragment_warns_once`, which §2.1's key choice
exists in part to preserve. If any needs editing, that is a signal the change altered
well-formed behaviour and must be re-examined, not accommodated.

Full local gate before the PR, per CLAUDE.md: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-features --all-targets -- -D warnings`,
`cargo test --workspace --all-features`, and
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`.

## 7. Risks

- **The id-resolution chain is rewritten, not re-keyed** (§3.4). It is the one place
  two implementers could diverge — specifically on where the pre-id buffering branch
  sits relative to the replacement rule. §3.4 writes the target shape out for that
  reason.
- **The `already` net is a footgun in the shape of a safety feature.** Added naively
  it *causes* the loss it appears to prevent (§3.6). Test 8 exists solely to catch
  that, and it must be written before the net.
- **Release plumbing.** A squashed `fix(providers-openai): SMA-566 …` PR title that
  also touches `providers-litellm/src/stream.rs` will make release-plz attribute a
  patch bump to litellm as well. Harmless — the edit is doc-only and a new litellm
  patch release is not wrong — but it should be a conscious call rather than a
  surprise on the release PR. No hand-bump here, so no facade or `core` cascade.
