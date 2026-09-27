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

## 3. Design: split the two jobs into two sets

Replace `name_emitted` with two fields, one for each job:

```rust
/// Items whose arguments were delivered in at least one `ToolCallDelta`.
/// Keyed by `item_id`. Reconciliation dedup (SMA-562) and `has_tool_calls`.
emitted_items: HashSet<String>,

/// Calls whose name was already emitted, keyed by call identity.
/// Value: the `item_id` that emitted the name, and the name, for the warning.
named_calls: HashMap<CallKey, (String, String)>,
```

```rust
/// The identity that name emission is gated on.
#[derive(Clone, PartialEq, Eq, Hash)]
enum CallKey {
    /// A non-blank `call_id`.
    Call(String),
    /// Fallback for a blank `call_id`: the `item_id`.
    Item(String),
}
```

### 3.1 Why a blank `call_id` falls back to the item

The core contract says a provider MUST NOT merge two parallel blank-id calls. If the
key were the raw `call_id`, two parallel items with `call_id: ""` would share one
key, and the second call would lose its name. `CallKey::Item` keeps the current
behaviour for blank ids: one name per item. This is the same rule that SMA-566 uses
in `ChatTranslator::canonicalize` (an empty id "is not an identity").

### 3.2 One helper decides the name

All three emission sites use one private helper:

```rust
/// Returns `Some(name)` when this is the first delta that names the call, and
/// records the item as emitted. Returns `None` when the call already has a name.
fn claim_name(&mut self, item_id: &str, call_id: &str, name: &str) -> Option<String>
```

The helper:

1. Inserts `item_id` into `emitted_items`.
2. Builds the `CallKey` from `call_id` (blank → `Item(item_id)`).
3. If the key is not in `named_calls`, inserts `(item_id, name)` and returns
   `Some(name)`.
4. Otherwise returns `None`. If the recorded `item_id` is a different item, and this
   is the first time this item is suppressed, it logs one `tracing::warn!` with the
   `call_id`, both `item_id`s, and both names. The warning says that the shape is
   malformed, and that the arguments of both items go to one call.

"The first time this item is suppressed" is exactly "the item was not in
`emitted_items` before step 1". No separate "warned" set is necessary.

### 3.3 The sites

| Site | Today | After |
| --- | --- | --- |
| `output_item.added`, buffered flush | inserts `item_id` into `name_emitted`, emits `Some(name)` | `name = self.claim_name(..)` |
| `function_call_arguments.delta` | `Some` if `item_id` not in `name_emitted` | `name = self.claim_name(..)` when the item is not in `emitted_items`, else `None` |
| `emit_call_if_unseen` (dedup) | returns `None` if `item_id` in `name_emitted` | returns `None` if `item_id` in `emitted_items` |
| `emit_call_if_unseen` (emit) | inserts `item_id`, emits `Some(name)` | `name = self.claim_name(..)` |
| `response.completed` `has_tool_calls` | `!name_emitted.is_empty()` | `!emitted_items.is_empty()` |

The delta path calls `claim_name` only on the first delta of an item. Later deltas of
the same item carry `name: None`, as today.

## 4. Decisions

### 4.1 The two SMA-562 reconciliation sites stay keyed on the item

The ticket asks whether `output_item.done` and the `response.completed` sweep need
the same treatment. **Their name emission does. Their dedup does not.**

- **Name.** Both sites emit through `emit_call_if_unseen`, which today puts
  `Some(name)` on its delta unconditionally. With two items for one call, the sweep
  can emit a second name. That site uses `claim_name` (Section 3.3).
- **Dedup.** The dedup question is "did this item's arguments already reach the
  consumer?". That is a per-item question, so the key stays `item_id`
  (`emitted_items`). A per-call key would answer a different question, and
  Section 4.2 explains why that is worse.

### 4.2 The arguments of every item still reach the call (alias, not drop)

Two items with one `call_id` can each carry arguments. There are two options:

- **Alias (chosen).** Every item delivers its arguments under the shared `call_id`.
  Only the name is gated. This is exactly today's argument behaviour. It is also the
  SMA-566 behaviour, where a second index aliases onto the owner and its arguments
  merge.
- **First item wins (rejected).** Drop the arguments of every later item. This makes
  the reconciliation path drop arguments, but the delta path cannot drop them the
  same way without new buffering. The two paths would then disagree. It also changes
  which bytes reach the consumer, which is outside the scope of this ticket.

With the alias rule, two items that both carry arguments give the accumulator a
concatenated string such as `{"a":1}{"b":2}`. That string is not valid JSON, and the
turn fails in `ModelTurnAccumulator::finish`. This is the current behaviour, and it
is loud. The shape is malformed and unobserved, and a loud failure is better than a
silent choice between the two argument sets. The new warning (Section 3.2) names the
cause.

### 4.3 `has_tool_calls` reads `emitted_items`

The SMA-562 invariant is "`Finish { ToolCalls }` iff at least one `ToolCallDelta` was
emitted". Every insertion into `emitted_items` is paired with an emitted
`ToolCallDelta` for that item. Every emitted `ToolCallDelta` inserts its item. So
`!emitted_items.is_empty()` states the invariant exactly. (`!named_calls.is_empty()`
is equal in value, because the first delta of any call names it, but it states the
invariant less directly.)

### 4.4 The `id: None` guard does not change

`emit_call_if_unseen` refuses an item without an `id`, because the dedup key is the
`item_id`. That key does not change (Section 4.1), so the reason for the guard is
still true.

## 5. Tests

All tests go in the `tests` module of `responses.rs` and use the existing helpers
(`added_event`, `delta_event`, `done_event`, `function_item`). A shared helper counts
name-carrying `ToolCallDelta`s per `call_id` across all emitted events.

1. **`two_items_one_call_id_emit_one_name` (the acceptance test).** Two
   `output_item.added` events: `fc_1`/`call_A` and `fc_2`/`call_A`. One argument delta
   for each item. Assert exactly one name-carrying `ToolCallDelta` for `call_A`, and
   assert that both argument fragments are forwarded under `call_A`, in wire order.
   **Must be verified to FAIL against the current translator** before the fix.
2. **`alias_item_reconciled_on_done_carries_no_name`.** `fc_1` streams deltas.
   `fc_2` (same `call_id`, no deltas) arrives only on `output_item.done`. Assert that
   the `done` delta has `name: None` and carries `fc_2`'s arguments.
3. **`completed_sweep_names_a_shared_call_once`.** No `added`, no deltas. The
   `response.completed` output lists `fc_1` and `fc_2` with the same `call_id`.
   Assert one name for the call, then `Finish { ToolCalls }`.
4. **`buffered_alias_flush_carries_no_name`.** `fc_1` is added and streams. An
   argument delta for `fc_2` arrives before `fc_2`'s `added` (buffered), then `fc_2`'s
   `added` flushes it. Assert the flush has `name: None`.
5. **`blank_call_ids_are_not_merged`.** Two items with `call_id: ""` each stream a
   delta. Assert two name-carrying deltas, one for each item (the core non-merge
   rule, Section 3.1).
6. **Existing tests.** All current tests in `responses.rs`, the fixture tests, and the
   SMA-533 conformance subject stay green without edits.

Tests 1 to 4 each must fail against the current translator, and test 5 must pass
against it. The plan records the pre-fix result of each.

## 6. Documentation in code

- Update the `ResponsesTranslator` doc comment. The `delta` bullet says "name emitted
  once per call_id", which is the intent; state that the gate is the call identity,
  with the blank-id fallback.
- Replace the `name_emitted` field docs with docs for the two new fields.
- Update the comments that name `name_emitted`: the `emit_call_if_unseen` doc, the
  `response.completed` arm comment, and the `terminal_events` doc.
