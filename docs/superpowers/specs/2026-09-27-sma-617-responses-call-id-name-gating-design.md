# SMA-617 — Gate `openai/responses` name emission on the call, not the item

**Ticket:** [SMA-617](https://linear.app/smaschek/issue/SMA-617/openairesponses-can-emit-two-name-carrying-deltas-for-one-call-id-via)
**Date:** 2026-09-27
**Related:** SMA-566 (the same defect class in `openai/chat`), SMA-562 (the two
reconciliation sites this must keep correct), SMA-533 (the conformance suite that
does not catch this)

## 1. The defect

`ResponsesTranslator` in
`crates/paigasus-helikon-providers-openai/src/backend/responses.rs` uses one set,
`name_emitted: HashSet<String>`, keyed by `item_id`. The set has two jobs:

1. **Name gating.** The delta path (`function_call_arguments.delta`), the buffered
   flush in `output_item.added`, and `emit_call_if_unseen` put `name: Some(..)` on a
   `ToolCallDelta` only when the `item_id` is not in the set.
2. **Reconciliation dedup (SMA-562).** `emit_call_if_unseen`, called from
   `output_item.done` and from the `response.completed` sweep, emits nothing when
   the `item_id` is in the set.

Two function-call items with different `item.id` but the same `item.call_id` make
two entries. Each item then emits a name-carrying `ToolCallDelta` under the same
`call_id`. The core contract (`ModelEvent::ToolCallDelta::name`, `model.rs`) says
`name` is `Some` "exactly once per non-blank `call_id`". This stream breaks that
contract.

### 1.1 The effect downstream

`ModelTurnAccumulator::observe` keys on `call_id`, keeps the first name, and appends
every `args_delta`. So today the second name is ignored by this crate's own
accumulator, and the turn result does not change. The defect is a contract
violation that a stricter consumer (or the SMA-533 assertion, given a fixture) can
see. It is not a data-loss bug in this crate.

### 1.2 Why the SMA-533 suite is green

The shape is malformed and unobserved. The suite uses only fixtures transcribed from
captured traffic, so it has no fixture for this shape, and the `openai_responses`
subject passes vacuously. The guard belongs in the crate's unit tests.

### 1.3 Atomicity is not injectivity

The Responses wire sends `item.id` and `item.call_id` together on
`output_item.added`. That removes the "id arrives late" sub-problem that SMA-566 had
in Chat Completions. It does not stop two different `item_id`s from mapping to one
`call_id`. The fix is still necessary.

## 2. Goal and non-goals

**Goal.** For every non-blank `call_id`, the translator emits exactly one
`ToolCallDelta` with `name: Some(..)`, whatever the number of items that carry that
`call_id`.

**Non-goals.**

- No change to which argument bytes reach the consumer. Section 4.2 explains why.
- No change to the `id: None` guard in `emit_call_if_unseen`.
- No change to the `response.incomplete` arm (`!item_to_call.is_empty()`).
- No new conformance fixture. The shape has no capture (SMA-533 provenance rule).
- No change to public API, the mdBook, or any crate README. The change is internal
  to a `pub(crate)` translator. This is a deliberate decision, not a skip.

## 3. Design: split the two jobs into two fields

Replace `name_emitted` with two fields, one for each job:

```rust
/// Items whose arguments were delivered in at least one `ToolCallDelta`.
/// Keyed by `item_id`. Reconciliation dedup (SMA-562) and `has_tool_calls`.
emitted_items: HashSet<String>,

/// Non-blank `call_id`s whose name was already emitted.
/// Value: `(item_id, name)` of the item that emitted the name, for the warning.
named_calls: HashMap<String, (String, String)>,
```

### 3.1 The once-per-item invariant

Every emission site emits a name-carrying delta only for an item that is not yet in
`emitted_items`, and inserts the item into `emitted_items` in the same branch:

- The delta path checks `emitted_items` explicitly.
- `emit_call_if_unseen` checks `emitted_items` explicitly (its dedup).
- The `added` flush is safe by construction. `pending_args` gets new data only while
  the item is not in `item_to_call` (`responses.rs:514`, `543-546`), and every item
  in `emitted_items` is already in `item_to_call`.

Thus the name decision runs **at most once per item**. The `emitted_items.insert`
stays visible at each emission site, as `name_emitted.insert` is today. This keeps
the SMA-562 rule "every insertion is paired, in the same branch, with the
`ToolCallDelta`" readable at the call site.

### 3.2 One helper decides the name

```rust
/// Decide the `name` field of an item's first `ToolCallDelta`.
///
/// `call_id` and `name` MUST be exactly the values the emitted `ToolCallDelta`
/// carries. Touches only `named_calls`. The caller inserts into
/// `emitted_items`.
fn claim_name(&mut self, item_id: &str, call_id: &str, name: &str) -> Option<String>
```

The helper:

1. If `call_id` is blank, returns `Some(name)` and records nothing (Section 3.3).
2. If `call_id` is not in `named_calls`, inserts `(item_id, name)` and returns
   `Some(name)`.
3. Otherwise logs one `tracing::warn!` on target `paigasus::openai::responses` with
   the `call_id`, the owner's `item_id` and name, and this `item_id` and name, and
   returns `None`. The warning says that the shape is malformed, and that the
   arguments of both items go to one call.

Step 3 needs no "already warned" state. By Section 3.1 the helper runs once per item,
so the warning fires once per aliasing item. The owner can never reach step 3,
because it reaches the helper only once.

**Rule for the caller.** The helper receives exactly the `call_id` and `name` that
the emitted `ToolCallDelta` carries. The delta path and the `added` flush pass the
values from `item_to_call`. `emit_call_if_unseen` passes `fc.call_id` and `fc.name`,
because those are what it emits. If the helper saw a different `call_id` from the one
emitted, it could suppress the only name of a `call_id`.

**Borrow note.** The delta arm holds `self.item_to_call.get(..)` while it would call
`&mut self`. Clone `call_id` and `name` out of the map first (same constraint as
`chat.rs:326-330`).

### 3.3 A blank `call_id` is not an identity

The core contract says a provider MUST NOT merge two parallel blank-id calls. A blank
`call_id` is therefore never put in `named_calls`, and each item with a blank
`call_id` names itself once. This is today's behaviour for blank ids, and the rule
SMA-566 uses in `ChatTranslator::canonicalize` (an empty id "is not an identity").

An earlier draft used a `CallKey { Call(call_id), Item(item_id) }` enum with the
`item_id` as a key for blank ids. By Section 3.1, an `Item` key would never be found
in the map on any path, so the enum adds a type and no behaviour. It is rejected
(YAGNI). The trade-off: if a future site calls the helper twice for one item with a
blank id, that item names twice. Section 3.1's invariant, which each site states in
a comment, is the guard against that.

### 3.4 The sites

| Site | Today | After |
| --- | --- | --- |
| `output_item.added`, buffered flush | inserts `item_id` into `name_emitted`, emits `Some(name)` | inserts into `emitted_items`, `name = self.claim_name(..)` |
| `function_call_arguments.delta` | `Some` if `item_id` not in `name_emitted` | if the item is not in `emitted_items`: insert it, `name = self.claim_name(..)`; else `None` |
| `emit_call_if_unseen` (dedup) | returns `None` if `item_id` in `name_emitted` | returns `None` if `item_id` in `emitted_items` |
| `emit_call_if_unseen` (emit) | inserts `item_id`, emits `Some(name)` | inserts into `emitted_items`, `name = self.claim_name(..)` |
| `response.completed` `has_tool_calls` | `!name_emitted.is_empty()` | `!emitted_items.is_empty()` |

## 4. Decisions

### 4.1 The two SMA-562 reconciliation sites: name yes, dedup no

The ticket asks whether `output_item.done` and the `response.completed` sweep need
the same treatment. **Their name emission does. Their dedup does not.**

- **Name.** Both sites emit through `emit_call_if_unseen`, which today puts
  `Some(name)` on its delta unconditionally. With two items for one call, the sweep
  can emit a second name. That site now uses `claim_name`.
- **Dedup.** The dedup question is "did this item's arguments already reach the
  consumer?". That is a per-item question, so the key stays `item_id`
  (`emitted_items`). A dedup keyed on the call (or on name ownership) would re-emit
  the arguments of an alias item that already streamed its own deltas. Test 6 pins
  this.

### 4.2 The arguments of every item still reach the call (alias, not drop)

Two items with one `call_id` can each carry arguments. There are two options:

- **Alias (chosen).** Every item delivers its arguments under the shared `call_id`.
  Only the name is gated.
- **First emitted item wins (rejected).** Drop every delta of a later item for an
  owned `call_id`. This needs no buffering: the delta path and reconciliation can
  both drop at once.

The reasons for alias:

1. **Scope.** Alias does not change which argument bytes reach the consumer. The
   ticket is about the name contract, not argument selection.
2. **Consistency with SMA-566.** In `openai/chat`, a second index aliases onto the
   owner, and its arguments merge.
3. **No guess.** When both items carry arguments, the accumulator gets a
   concatenated string such as `{"a":1}{"b":2}`, which is not valid JSON, and the
   turn fails in `ModelTurnAccumulator::finish` (`agent.rs` records the error). This
   is today's behaviour, and it is loud. Drop would silently choose one argument set.

**Why this differs from SMA-562.** SMA-562 drops an `in_progress` or `incomplete`
item in preference to failing the turn (`responses.rs:337-340`). There, the dropped
arguments are known to be truncated, so dropping them removes known-bad data. Here,
neither argument set is known to be bad, so a drop is a guess. If a later capture
shows a backend that resends one item with identical arguments under a new `id`, a
"first emitted wins" rule for identical arguments is a follow-up decision with a
fixture.

**Different names under one `call_id`.** Two items with one `call_id` and different
names are probably two distinct calls that reuse an id. Alias still gives one call
with the first name. A split under a synthetic id is not possible, because `call_id`
is the submission key for `function_call_output`. The warning includes both names,
so this case is visible in the logs.

**Log level.** `warn!` is correct. The shape is a backend defect. SMA-566 uses
`warn!` for malformed backend shapes and `error!` only for internal keying
regressions (`chat.rs:483`).

### 4.3 `has_tool_calls` reads `emitted_items`

The SMA-562 invariant is "`Finish { ToolCalls }` iff at least one `ToolCallDelta` was
emitted". Every insertion into `emitted_items` is paired with an emitted
`ToolCallDelta` for that item. Every emitted `ToolCallDelta` inserts its item. So
`!emitted_items.is_empty()` states the invariant exactly.

### 4.4 The `id: None` guard does not change

`emit_call_if_unseen` refuses an item without an `id`, because the dedup key is the
`item_id`. That key does not change (Section 4.1), so the reason for the guard is
still true.

## 5. Tests

All new tests go in the `tests` module of `responses.rs`. They use the existing
helpers (`added_event`, `delta_event`, `done_event`, `function_item`,
`completed_event`). A shared helper counts name-carrying `ToolCallDelta`s per
`call_id` across all emitted events.

**The new tests must compile against the current translator.** They use only
`consume`, inspection of the returned events, and `crate::test_tracing`. They do not
read `emitted_items` or `named_calls`. So each pre-fix run gives an assertion
failure, not a compile error, as AC (2) requires.

1. **`two_items_one_call_id_emit_one_name` (the acceptance test).** Two
   `output_item.added` events: `fc_1`/`call_A` and `fc_2`/`call_A`. One argument delta
   for each item. Assert exactly one name-carrying `ToolCallDelta` for `call_A`, and
   assert that both argument fragments are forwarded under `call_A`, in wire order.
   Pre-fix: **FAILS** (two names).
2. **`alias_item_reconciled_on_done_carries_no_name`.** `fc_1` streams deltas.
   `fc_2` (same `call_id`, no deltas) arrives only on `output_item.done`. Assert that
   the `done` delta has `name: None` and carries `fc_2`'s arguments.
   Pre-fix: **FAILS**.
3. **`completed_sweep_names_a_shared_call_once`.** No `added`, no deltas. The
   `response.completed` output lists `fc_1` and `fc_2` with the same `call_id`.
   Assert one name for the call, the arguments of both items under `call_A` in output
   order, then `Finish { ToolCalls }`. Pre-fix: **FAILS**.
4. **`buffered_alias_flush_carries_no_name`.** `fc_1` is added and streams. An
   argument delta for `fc_2` arrives before `fc_2`'s `added` (buffered), then `fc_2`'s
   `added` flushes it. Assert the flush has `name: None`. Pre-fix: **FAILS**.
5. **`blank_call_ids_are_not_merged`.** Two items with `call_id: ""` each stream a
   delta. Assert two name-carrying deltas, one for each item (Section 3.3).
   Pre-fix: **PASSES** (a regression guard for the blank-id rule).
6. **`streamed_alias_is_not_re_emitted_by_reconciliation`.** `fc_1` and `fc_2`
   (same `call_id`) both stream deltas. Then `done(fc_2)`, then `response.completed`
   that lists both items. Assert that `done` and the sweep emit no `ToolCallDelta`,
   that there is exactly one name, and `Finish { ToolCalls }`. Pre-fix: **FAILS**
   (two names from the delta path). It kills the mutant "dedup keyed on name
   ownership", which passes tests 1 to 5.
7. **`alias_item_warns_once`.** Use `crate::test_tracing::start()` and `captured()`
   (the required path, `test_tracing.rs:30-34`). Drive test 1's shape with two
   deltas for `fc_2`. Assert exactly one `WARN` on target
   `paigasus::openai::responses` whose text contains both `fc_1` and `fc_2`.
   Pre-fix: **FAILS** (no warning).

**Existing tests.** Two existing tests read the removed field and must change:
`completed_skips_incomplete_output_item` (`responses.rs:1400`) and
`completed_skips_in_progress_output_item` (`responses.rs:1446`). Each asserts
`t.name_emitted.is_empty()`. Replace that with
`t.emitted_items.is_empty() && t.named_calls.is_empty()`. Keep each test's
`Finish { Stop }` assertion unchanged, because it pins `has_tool_calls`. All other
tests in `responses.rs`, the fixture tests, and the SMA-533 conformance subject stay
green without edits.

The plan records the actual pre-fix result of each new test.

## 6. Documentation

**In `responses.rs`.** Update every occurrence of `name_emitted`, including test
docs, and confirm with `grep -n name_emitted responses.rs` that none remain. This
includes the struct doc (the `delta` bullet and the `response.completed` bullet),
the `item_to_call` field doc, the `emit_call_if_unseen` doc, the `response.completed`
arm comment, the `terminal_events` doc, and the test docs. The struct doc states that
the name gate is the non-blank `call_id`, and that a blank `call_id` names once per
item.

**Cross-file, doc-only (the SMA-566 §5.1 precedent).**

- `crates/paigasus-helikon-providers-openai/src/backend/chat.rs:689-692` says the
  `responses` translator "has no blank-id handling whatsoever; its own name-dedup
  defect is open as SMA-617". Both claims become false. Replace them with one
  sentence: the `responses` translator gates names on the non-blank `call_id` and
  does not merge blank ids (SMA-617).
- `tests/provider-stream-conformance/src/check.rs:82-85` says "both first-party chat
  translators" refuse to merge blank ids. Add the `responses` translator.
- `tests/provider-stream-conformance/tests/conformance.rs`, the `openai_responses`
  module doc (starts at line 2767): add a section like the `openai_chat` one
  (lines 573-591). It says that the two-items-one-`call_id` shape has no capture,
  that `conforms` does not exercise it, and that the regression guard is the unit
  tests of Section 5 in `responses.rs`.

## 7. Spec-challenge changelog

The adversarial challenge (Opus) gave **APPROVE WITH CHANGES**, with no blocker.

**Folded in:**

- Two existing tests read `name_emitted`. Section 5 now names them and their
  replacement assertion (was: "no edits").
- The warning had no test and an easy "never fires" ordering bug. Added test 7.
  `claim_name` no longer inserts into `emitted_items`, so the order problem is gone.
- The once-per-item invariant is now stated (Section 3.1). The "already warned" logic
  is removed.
- `claim_name` touches only `named_calls`. The `emitted_items` insert stays visible
  at each site.
- The `call_id` rule for the caller and the borrow note were added (Section 3.2).
- The false "needs buffering" reason against "first wins" was removed. Section 4.2
  now gives the true reasons and answers why this differs from SMA-562, the
  different-names case, and the log level.
- Test 6 kills the "dedup on name ownership" mutant. Test 3 now also checks argument
  order.
- Section 6 now covers every `name_emitted` occurrence and the three cross-file docs.
- The `CallKey` enum is replaced by a map keyed on non-blank `call_id` (Section 3.3).
- The new tests must compile pre-fix. `completed_event` is in the helper list.

**Rejected:** none.
