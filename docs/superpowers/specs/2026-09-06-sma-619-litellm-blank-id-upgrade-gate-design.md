# SMA-619 — Gate litellm's blank→real `call_id` upgrade on prior emission

`providers-litellm` upgrades a blank `call_id` to a real one unconditionally.
When a `ToolCallDelta` has already gone out under `""`, that upgrade splits one
logical call across two `call_id`s and leaves the real one with zero
name-carrying deltas. `openai/chat` gates the same upgrade on a `blank_emitted`
set (SMA-566); litellm has no equivalent. This is the last remaining asymmetry
between the two chat translators after SMA-616.

## 0. Acceptance criteria

1. A test drives a blank-id call that emits mid-stream, then a real `id` on the
   same wire key, and asserts the call is **not** split — no `call_id` ends up
   with zero name-carrying deltas.
2. That test is verified to FAIL against the translator as it stands on `main`.
3. `openai/chat` and `litellm` carry the same gate and the same rationale
   comment.
4. `chat.rs`'s "one remaining asymmetry" note is updated to reflect that none
   remains.
5. The existing `a_real_id_replaces_a_blank_one_on_the_same_wire_key` still
   passes — the safe upgrade is preserved, not traded away.

## 1. The defect

Traced against `crates/paigasus-helikon-providers-litellm/src/stream.rs` at
`origin/main` (`0ee9af7`):

```json
[{"index": 0, "id": "",   "function": {"name": "alpha", "arguments": "{}"}},
 {"index": 0, "id": "c1", "function": {"arguments": "[]"}}]
```

**Delta 1.** SMA-616 filters a blank `id` out of the wire key, so the key is
`Key::Index(0)`, not `Key::Id("")`. `tc.id` is `Some("")`, so the registration
block inserts `tool_calls[Key::Index(0)] = ""`. `canonicalize` returns the key
unchanged — an empty id is not an identity. `args_frag` is non-empty, so the
name flushes. Emitted: `("", Some("alpha"), "{}")`.

**Delta 2.** Same wire key. The replacement arm at `stream.rs:425` fires:

```rust
Some(existing) if existing.is_empty() && !id.is_empty() => {
    *existing = id.to_owned();
}
```

`call_id` is now `"c1"`, `canonicalize` migrates to `Key::Id("c1")`, and
`name_emitted` is keyed on the *old* `Key::Index(0)` — so the flush condition
sees no prior emission for `Key::Id("c1")`, but `slot.name` is empty (it was
taken on delta 1) and nothing flushes. Emitted: `("c1", None, "[]")`.

**Result.** Two `call_id`s for one call. `""` carries the name; `"c1"` carries
none. `paigasus_helikon_core::ModelEvent::ToolCallDelta` documents `name` as
`Some` **exactly once per non-blank `call_id`** — `"c1"` violates it at zero.
That is strictly worse than the stuck blank the replacement rule exists to
prevent: a blank id is at least uniformly unidentified and diagnosable, whereas
a split leaves a well-formed-looking call whose name never arrives.

### 1.1 The rule being gated is still right

The replacement arm is not the defect and must not be removed. Registration is
otherwise first-id-wins, so without it a blank id recorded first would stick
and every later delta — including one carrying the real id — would keep
resolving to `""`. The call would reach the agent loop under an empty
`call_id` it cannot submit a result against. `canonicalize` treats a blank id
as "no identity yet"; the replacement arm is the other half of that policy.

The defect is that the rule has no upper bound in time. It is correct right up
until a delta has gone out under `""`, and wrong from that moment on, because
from then on the blank id has *become* the call's public identity whether or
not it can identify anything. The fix is a gate, not a removal.

### 1.2 The existing test covers only the safe half

`a_real_id_replaces_a_blank_one_on_the_same_wire_key` (`stream.rs:1864`) drives
`{"name": "foo"}` with **no** `arguments` on the first delta. `flush` therefore
does not fire (neither signal is present: `args_frag` is empty and `name_frag`
is not), `emit_name` is `None`, `args` is empty, and `handle_tool_call` returns
before pushing anything. Nothing has been emitted when the real id arrives, so
the upgrade is safe and the test passes both before and after this change.

The unsafe half — `arguments` present on the first delta, so the name goes out
under `""` — is uncovered. Adding that coverage is acceptance criterion 1.

## 2. Decision

Port `openai/chat`'s `blank_emitted` gate (SMA-566) to `providers-litellm`,
keyed on `Key` rather than `u32`. Once a wire key has emitted a
`ToolCallDelta` while its `call_id` was still blank, that key's blank→real
upgrade is withheld for the remainder of the stream. Keeping the blank keeps
the call whole.

Both outcomes are imperfect — a whole call under `""` is still a call the agent
loop cannot submit a result against. The tie-break is that a uniformly blank
call is *one* wrong thing, visibly unidentified and already warned about by
`canonicalize`, while a split is *two* things that each look well-formed in
isolation and whose corruption is only visible by correlating them. Consumers
that key on `call_id` — which is every consumer — silently mis-handle the
second shape and loudly fail the first.

## 3. The change (`providers-litellm`)

### 3.1 New field

```rust
/// Wire keys that have already emitted a `ToolCallDelta` while their
/// `call_id` was still blank.
blank_emitted: HashSet<Key>,
```

Added to `ChatTranslator` and initialized in `new()`.

### 3.2 Why `Key`, and why the set is structurally `Index`-only

The ticket specifies `Key` over `u32` because litellm's wire key is an enum. Two
properties make that choice sound rather than merely type-shaped:

**The check reads the wire key; the insert writes the canonical key.** In
`handle_tool_call`, `key` is shadowed by
`let key = self.canonicalize(key, &call_id);` between the two sites. This is not
a skew, because `canonicalize` returns a blank-id key **unchanged** (it
early-returns before minting a canonical `Key::Id`), and the insert is guarded
on `call_id.is_empty()`. In the only case where the insert runs, the pre- and
post-canonicalization keys are the same value.

**A blank `call_id` is only ever reachable under `Key::Index(_)`.** `Key::Id(x)`
is minted in exactly two places: the wire-key match, where it is guarded by
`.filter(|id| !id.is_empty())`, and `canonicalize`, which early-returns on a
blank `call_id` before constructing one. `tool_calls` therefore never maps a
`Key::Id` to an empty string, so `call_id.is_empty()` implies the key is an
`Index`. The set is `HashSet<Key>` for type-fit with `tool_calls`,
`name_emitted`, `pending` and `warned_blank_id` — not because `Id` entries are
expected. A `HashSet<u32>` would work identically and read worse.

### 3.3 The gate

Capture the flag **before** the `tool_calls.get_mut(&key)` borrow, so the match
guard reads a plain `bool` rather than holding an immutable borrow of `self`
across a mutable one — the same reason `chat.rs:690` does it:

```rust
let blank_already_emitted = self.blank_emitted.contains(&key);

if let Some(id) = tc.id.as_deref() {
    match self.tool_calls.get_mut(&key) {
        Some(existing)
            if existing.is_empty() && !id.is_empty() && !blank_already_emitted =>
        {
            *existing = id.to_owned();
        }
        Some(_) => {}
        None => {
            self.tool_calls.insert(key.clone(), id.to_owned());
        }
    }
}
```

The existing comment on the arm gains the second paragraph `chat.rs:704-711`
carries, adapted to litellm's vocabulary (`key` rather than `index`) and citing
SMA-619.

### 3.4 The insert

At the emit site, after the "suppress a wholly empty event" early return and
before the `out.push`:

```rust
// Record that this key has emitted under a blank id, so the replacement
// rule above cannot later split the call in two.
if call_id.is_empty() {
    self.blank_emitted.insert(key.clone());
}
```

Placement after the early return is load-bearing: a delta that emits nothing has
not published the blank id to anyone, so it must not close the upgrade window.
`chat.rs:823` places it identically.

### 3.5 The gate fires on *any* emission, not on a name-carrying one

This looks like a simplification opportunity and is not. The args-only shape
splits just as badly:

```json
[{"index": 0, "id": "",   "function": {"arguments": "{\"a\":"}},
 {"index": 0, "id": "c1", "function": {"name": "alpha", "arguments": "1}"}}]
```

Ungated, `"c1"` *does* receive the name — acceptance criterion 1 is technically
satisfied — but the arguments JSON is torn across two `call_id`s and neither
half parses. Narrowing the gate to name-carrying emissions would trade a
name-loss corruption for an arguments-loss corruption. Both are covered by one
rule: any published delta closes the window. Pinned by a test (§5.2) so the
narrowing is not attempted later as a cleanup.

## 4. The change (`providers-openai`)

Comment only. The closing paragraph of
`ChatTranslator::handle_tool_call_chunk`'s doc comment currently reads "The one
remaining asymmetry is deliberate and ticketed …(SMA-619)". Replaced with:

```rust
/// Both crates gate the blank→real `call_id` upgrade on `blank_emitted`,
/// so a call that has already emitted under `""` keeps the blank rather
/// than splitting across two ids (SMA-566 here, SMA-619 in litellm). The
/// end-of-stream dedup net is likewise symmetric — both exempt blank
/// `call_id`s from it (SMA-616). No asymmetry remains.
```

Rewritten rather than deleted so the alignment claim, and where each half is
recorded, stays discoverable from `chat.rs` — the same treatment SMA-616 gave
the dedup-net sentence.

No behavioural change to `providers-openai`. Its own gate and tests
(`a_real_id_does_not_replace_a_blank_one_after_the_index_emitted`,
`chat.rs:1706`) already hold.

## 5. Tests

All in `crates/paigasus-helikon-providers-litellm/src/stream.rs`'s `mod tests`,
using the existing `tc_chunk` / `drive` / `named` / `args_of` helpers.

### 5.1 New: `a_real_id_does_not_replace_a_blank_one_after_the_key_emitted`

The §1 trace. Named to mirror `chat.rs`'s
`a_real_id_does_not_replace_a_blank_one_after_the_index_emitted`, differing only
in `key`/`index` to match each crate's vocabulary. Asserts three things:

- `named(&evs) == [("", "alpha")]` — the name stays under the blank id it was
  emitted with.
- `args_of(&evs, "c1") == ""` — no delta arrives under the real id at all, which
  is the precise failure being prevented: a delta under `"c1"` could only be one
  carrying no name.
- `args_of(&evs, "") == "{}[]"` — every delta for this call stays under one
  `call_id`.

**Must FAIL against `main`.** Verified before the fix is written: the pre-fix
translator produces `named == [("", "alpha")]` (which passes) but
`args_of(&evs, "c1") == "[]"` and `args_of(&evs, "") == "{}"`, failing
assertions two and three.

### 5.2 New: an args-only companion

Pins §3.5 — that the gate fires on any emission, not only a name-carrying one.
Drives the args-only shape from §3.5 and asserts the whole call, name and both
argument fragments, arrives under `""`: `named(&evs) == [("", "alpha")]` and
`args_of(&evs, "") == "{\"a\":1}"`, with `args_of(&evs, "c1") == ""`.

**Also fails against `main`, on all three assertions** — the pre-fix translator
produces `named == [("c1", "alpha")]`, `args_of("") == "{\"a\":"` and
`args_of("c1") == "1}"`. It is a second defect proof, not merely a guard against
a future narrowing of the gate.

litellm-only. `openai/chat` has no equivalent test; adding one there is outside
this ticket's scope and its gate is already pinned by `chat.rs:1706`.

### 5.3 Unchanged: `a_real_id_replaces_a_blank_one_on_the_same_wire_key`

Must still pass. Its first delta carries no `arguments`, so nothing is emitted,
`blank_emitted` never receives the key, and the upgrade proceeds as before. This
is the test that keeps §1.1 honest — it is what stops the fix from degenerating
into "delete the replacement arm".

## 6. Non-impact

**`paigasus-helikon-core`.** No change. `ModelEvent::ToolCallDelta` already
documents `name` as `Some` "exactly once per non-blank `call_id`" (tightened by
SMA-616). "Exactly once" already forbids zero; this ticket restores conformance
to a contract that is already written correctly.

**The conformance suite.** No change. `tests/provider-stream-conformance` states
in both its `openai_chat` and `litellm` modules that canonicalization regression
coverage lives in each crate's own unit tests, and no fixture drives a blank id.
`check.rs`'s deliberate blank-`call_id` exception (added by SMA-616) is about
*two* blank calls not merging, which this ticket does not touch.

**mdBook and crate READMEs.** No change — a conscious call, not a silent skip.
This is an internal stream-translation fix with no public API, feature, usage,
or crate-roster impact.

**Versions and CHANGELOGs.** No hand edits; release-plz owns both. Matches
SMA-616's file set exactly.

## 7. Out of scope

**The cross-key-space split.** litellm has two key *spaces*, so this shape also
splits:

```json
[{"index": 0, "id": "", "function": {"name": "alpha", "arguments": "{}"}},
 {"id": "c1", "function": {"arguments": "[]"}}]
```

Delta 2 has no `index`, so its wire key is `Key::Id("c1")` — a different key
entirely. The replacement arm is never reached, `blank_emitted` cannot see the
connection, and nothing on the wire asserts the two deltas are one call.
`openai/chat` cannot express the shape at all
(`ChatCompletionMessageToolCallChunk::index` is a required `u32`), so there is
no symmetry argument for handling it either. Left as-is deliberately; if a real
backend ever produces it, that is its own ticket with its own evidence.

**Rescuing a call already split by a released version.** Nothing here is
retroactive; the gate only affects streams translated after it lands.
