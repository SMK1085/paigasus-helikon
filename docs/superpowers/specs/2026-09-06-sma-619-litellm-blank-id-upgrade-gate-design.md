# SMA-619 — Gate litellm's blank→real `call_id` upgrade on prior emission

`providers-litellm` upgrades a blank `call_id` to a real one unconditionally.
When a `ToolCallDelta` has already gone out under `""`, that upgrade splits one
logical call across two `call_id`s and leaves the real one with zero
name-carrying deltas. `openai/chat` gates the same upgrade on a `blank_emitted`
set (SMA-566); litellm has no equivalent. This is the last remaining *gating*
asymmetry between the two chat translators after SMA-616.

All line citations in this spec are against `origin/main` at `0ee9af7`.

## 0. Acceptance criteria

1. A test drives a blank-id call that emits mid-stream, then a real `id` on the
   same wire key, and asserts the call is **not** split — no `call_id` ends up
   with zero name-carrying deltas.
2. That test is verified to FAIL against the translator as it stands on `main`.
3. `openai/chat` and `litellm` carry the same gate and the same rationale
   comment.
4. `chat.rs`'s "one remaining asymmetry" note is updated to reflect what
   actually remains.
5. The existing `a_real_id_replaces_a_blank_one_on_the_same_wire_key` still
   passes — the safe upgrade is preserved, not traded away.
6. Every shape this spec traces has its `ModelTurnAccumulator::finish()`
   outcome stated, pre- and post-fix. No shape changes from `Ok` to `Err`
   without an explicit decision recorded here and an assertion pinning it.
7. A withheld upgrade is logged once per key at `warn`, naming the real
   `call_id` that was discarded. The decision in §2 is a loud-versus-silent
   trade and must not itself be silent.

## 1. The defect

Traced against `crates/paigasus-helikon-providers-litellm/src/stream.rs`:

```rust
drive(&mut t, vec![
    tc_chunk(json!([{"index": 0, "id": "",   "function": {"name": "alpha", "arguments": "{}"}}])),
    tc_chunk(json!([{"index": 0, "id": "c1", "function": {"arguments": "[]"}}])),
])
```

**Delta 1.** The wire-key match at `stream.rs:389-390` arms on `tc.index` alone
— `(Some(i), _) => Key::Index(i)` — so the key is `Key::Index(0)` because the
delta carries `index`, regardless of its id. (SMA-616's blank-id filter on the
id arm matters only when `index` is *absent*; see §1.1.) `tc.id` is `Some("")`,
so the registration block inserts `tool_calls[Key::Index(0)] = ""`.
`canonicalize` returns the key unchanged at `stream.rs:193-203` — an empty id is
not an identity — and warns once, recording `Key::Index(0)` in `warned_blank_id`.
`args_frag` is non-empty, so the name flushes. Emitted:
`("", Some("alpha"), "{}")`. The buffer is then removed entirely at
`stream.rs:536-538`, both fields being empty.

**Delta 2.** Same wire key. The replacement arm at `stream.rs:442` fires:

```rust
Some(existing) if existing.is_empty() && !id.is_empty() => {
    *existing = id.to_owned();
}
```

`call_id` is now `"c1"`, so `canonicalize` mints `Key::Id("c1")`. Its
`self.pending.remove(&key)` at `stream.rs:214` returns `None` — delta 1's buffer
was removed at the emit site — so **no migration runs**, and `ensure_pending`
creates a fresh empty buffer under `Key::Id("c1")`. `name_emitted` is keyed on
the old `Key::Index(0)`, so the flush condition sees no prior emission for
`Key::Id("c1")`, but `slot.name` is empty and `flush` is false. Emitted:
`("c1", None, "[]")`.

**Result.** Two `call_id`s for one call. `""` carries the name; `"c1"` carries
none. `paigasus_helikon_core::ModelEvent::ToolCallDelta` documents `name` as
`Some` **exactly once per non-blank `call_id`** (`core/src/model.rs:183-186`) —
`"c1"` violates it at zero.

### 1.1 Where SMA-616's filter does and does not apply

The filter at `stream.rs:392` — `tc.id.as_deref().filter(|id| !id.is_empty())` —
sits on the `(None, Some(id))` arm, reached only when `index` is absent. It
stops two index-less blank-id entries from sharing one `Key::Id("")` slot
(`blank_ids_without_index_do_not_collapse`, `stream.rs:2021`). It is not what
makes this trace's key an `Index`. Stating it as the cause, as the ticket does,
sends a verifier looking in the wrong place.

### 1.2 The rule being gated is still right

The replacement arm is not the defect and must not be removed. Registration is
otherwise first-id-wins, so without it a blank id recorded first would stick and
every later delta — including one carrying the real id — would keep resolving to
`""`. `canonicalize` treats a blank id as "no identity yet"; the replacement arm
is the other half of that policy.

The defect is that the rule has no upper bound in time. It is correct right up
until a delta has gone out under `""`, and wrong from that moment on, because
from then on the blank id has *become* the call's public identity whether or not
it can identify anything. The fix is a gate, not a removal.

### 1.3 The existing test covers only the safe half

`a_real_id_replaces_a_blank_one_on_the_same_wire_key` (`stream.rs:1864`) drives
`{"name": "foo"}` with **no** `arguments` on the first delta. `flush` therefore
does not fire — neither signal is present, `args_frag` being empty and
`name_frag` not (`stream.rs:524-526`) — `emit_name` is `None`, `args` is empty,
and `handle_tool_call` returns at `stream.rs:540-542` before pushing anything.
Nothing has been emitted when the real id arrives, so the upgrade is safe.

The unsafe half — `arguments` present on the first delta, so the name goes out
under `""` — is uncovered. Adding it is acceptance criterion 1.

### 1.4 What the accumulator does with each shape

Reasoning stops at `ModelEvent` in every prior spec in this series, and for this
ticket that is not sufficient: the fix changes which `call_id`s exist, and
`ModelTurnAccumulator` groups by `call_id` (`core/src/model.rs:594`), joins each
group's `args_delta`s into one string, and parses that string once
(`core/src/model.rs:539-544`). A parse failure propagates out of `finish()` and
discards the **entire turn, assistant text included** (`core/src/model.rs:624`).
Merging two `call_id`s into one therefore concatenates two argument strings that
were never meant to be adjacent.

| Shape | Pre-fix `finish()` | Post-fix `finish()` |
|---|---|---|
| §1 / §5.1: `"{}"` then `"[]"` | `Ok` — `""`→`{}`, `c1`→`[]`, both parse | **`Err`** — `""`→`"{}[]"`, does not parse |
| §3.5 / §5.2: `"{\"a\":"` then `"1}"` | **`Err`** — `""`→`"{\"a\":"`, does not parse | `Ok` — `""`→`"{\"a\":1}"`, parses |

The two realistic-looking outcomes point in opposite directions, and this is the
axis on which the decision has to be made honestly.

**The decision: accept the §1 `Err`.** Real backends fragment arguments as a
partial JSON string, which is §3.5's shape — `"{}"` followed by `"[]"` is not a
fragmentation of anything, it is two complete JSON documents, and no LiteLLM
backend emits it. It is the ticket's chosen marker shape, picked to make the
split visible in `args_of`, not a captured trace. Its pre-fix `Ok` is an
artifact: the split happens to cut where both halves independently parse, and
the turn "succeeds" into two junk items — one named `alpha` under an
unsubmittable `""`, one with an empty `name` under `c1`
(`core/src/model.rs:545-549` defaults a missing name to `""`), which the agent
loop then dispatches unvalidated (`loop_state.rs:325-331`). Turning that into a
loud `Err` is the accumulator refusing to invent structure from a corrupt
argument stream, which is what it is for.

On the shape that actually occurs, the fix moves `Err` → `Ok`. That is the
result that matters. Both are asserted (§5.1, §5.2) so neither can drift
unnoticed.

## 2. Decision

Port `openai/chat`'s `blank_emitted` gate (SMA-566) to `providers-litellm`,
keyed on `Key` rather than `u32`. Once a wire key has emitted a `ToolCallDelta`
while its `call_id` was still blank, that key's blank→real upgrade is withheld
for the remainder of the stream. Keeping the blank keeps the call whole.

### 2.1 The tie-break, and the warn it depends on

Both outcomes are imperfect: a whole call under `""` is still a call the agent
loop cannot submit a `ToolResult` against. The tie-break is that a uniformly
blank call is *one* wrong thing, whereas a split is *two* things that each look
well-formed in isolation and whose corruption is visible only by correlating
them — plus, per §1.4, an argument string silently cut in half.

That argument is usually stated as "the blank case fails loudly". **In this
codebase, as it stands, it does not.** Nothing validates a blank `call_id`:
`build_items` constructs `Item::ToolCall { call_id: "", .. }` unconditionally
(`core/src/model.rs:545-549`) and `loop_state.rs:325-331` turns it into a
`ToolCallRequest` without checking. And the gate itself would be silent: the
only blank-id diagnostic is `canonicalize`'s one-shot `warn!`
(`stream.rs:193-201`), deduped by `warned_blank_id`, which therefore fires on
delta 1 — *before* `c1` was ever seen — and never mentions the real id that gets
discarded.

So the premise is made true rather than assumed. §3.4 adds a one-shot `warn!` at
the withheld-upgrade site naming the discarded id, matching this module's
standing rule, stated at `stream.rs:242-244` and `stream.rs:616-619`, that a
value dropped for a structural reason is dropped **loudly and recorded**. The
same warn goes into `openai/chat`, which has the identical silent gap, because
AC3 requires the two translators to carry the same gate and a diagnostic in one
only would open the asymmetry this ticket closes. See §4.2.

### 2.2 N parallel blank-id calls: the merge is extended, deliberately

The tie-break above is argued for one call. For two it cuts differently:

```rust
drive(&mut t, vec![
    tc_chunk(json!([{"index": 0, "id": "", "function": {"name": "alpha", "arguments": "{}"}},
                    {"index": 1, "id": "", "function": {"name": "beta",  "arguments": "{}"}}])),
    tc_chunk(json!([{"index": 0, "id": "c1", "function": {"arguments": "x"}},
                    {"index": 1, "id": "c2", "function": {"arguments": "y"}}])),
])
```

Post-fix both upgrades are withheld, so all four deltas carry `call_id: ""` and
`ModelTurnAccumulator` folds them into **one** item named `alpha` with
`args_str == "{}{}xy"`. Pre-fix, `x` and `y` reach distinct `c1` and `c2`.

Per AC6, the `finish()` outcome: **the merge fails the turn.** Two parallel
calls' argument objects concatenate into a string that cannot parse, so
post-fix `finish()` returns `Err` on any N>1 shape where both calls carry
arguments. On the isolating fixture used in §5.4 — where the two calls emit
under `""` with no arguments, and only the post-upgrade deltas carry any —
pre-fix yields three parsing items (`("", "alpha", {})` plus a nameless `c1`
and `c2`) and post-fix yields `Err`. This is the same trade §1.4 already
accepted and is accepted on the same grounds: the pre-fix `Ok` is junk that
parses — two nameless calls and one under an unsubmittable `""` — and a loud
failure beats dispatching it.

This extends a merge that `canonicalize`'s comment (`stream.rs:184-192`),
`blank_ids_do_not_collapse_distinct_calls` (`stream.rs:1956`) and core's
contract — "a provider MUST NOT merge two parallel blank-id calls"
(`core/src/model.rs:190-193`) — all exist to prevent at the *event* layer.

**Accepted, with two qualifications.** The translator's obligation is at the
event layer, and it is still met: two name-carrying deltas go out, `alpha` and
`beta`, exactly as SMA-616 requires. The merge happens in
`ModelTurnAccumulator`, which SMA-616 already documented as deliberately not
following that advice (`core/src/model.rs:190-197`: "This crate's own
`ModelTurnAccumulator` does not follow that advice: it deliberately merges
blank-id calls together, first-name-wins"). This ticket does not change that
behaviour; it widens the set of streams that reach it. And `openai/chat` has
carried exactly this trade since SMA-566 (`chat.rs:712-716`), so declining it
here would reopen the asymmetry rather than close it.

Pinned by a test (§5.4) so the next reader knows it was seen and decided, not
missed. The `warn!` from §2.1 fires twice here, once per key, which is what
makes the merge diagnosable at all.

### 2.3 Alternative considered and rejected: fail the call instead

The translator could refuse to emit an unidentifiable call at all — surface a
`ModelError`, or drop the call and record a violation — rather than publishing
one under `""` that no consumer can act on. This is arguably the honest fix, and
it is out of scope: it changes `ModelEvent`'s contract, affects every provider
rather than litellm, and would need `openai/chat` and `openai/responses` moved
in lockstep. It is also a strictly larger decision than "which of two corrupt
shapes do we prefer", which is what this ticket was filed to settle. Named here
so the next person does not have to rediscover that it was considered.

## 3. The change (`providers-litellm`)

### 3.1 New field

Ported from `chat.rs:279-287` with `key` for `index`, so AC3's "same rationale
comment" holds verbatim rather than in spirit:

```rust
/// Wire keys that have already emitted a `ToolCallDelta` while their
/// `call_id` was still blank.
///
/// Gates the blank-id replacement rule below. Once a delta has gone out
/// under `""`, upgrading the key to a real id would split one call across
/// two `call_id`s and leave the real one with zero name-carrying deltas —
/// an "exactly once" violation on a *non-blank* id, which is worse than
/// the stuck blank this rule exists to fix (SMA-619).
blank_emitted: HashSet<Key>,
```

Plus `warned_withheld_upgrade: HashSet<Key>` for §3.4's warn dedup, documented
in the same register as the neighbouring `warned_blank_id` and
`warned_late_name`.

Both are initialized in `new()`.

### 3.2 Why `Key`, and why the set is structurally `Index`-only

The ticket specifies `Key` over `u32` because litellm's wire key is an enum.
Two properties make that sound rather than merely type-shaped:

**The check reads the wire key; the insert writes the canonical key.** In
`handle_tool_call`, `key` is shadowed by
`let key = self.canonicalize(key, &call_id);` at `stream.rs:465` between the two
sites. This is not a skew: `canonicalize` returns a blank-id key **unchanged**
(`stream.rs:193-203`, early-returning before minting a `Key::Id`), and the
insert is guarded on `call_id.is_empty()`. In the only case where the insert
runs, the pre- and post-canonicalization keys are the same value.

**A blank `call_id` is only ever reachable under `Key::Index(_)`.** The argument
is about the *value* stored in `tool_calls`, not about which keys get
constructed. If `key` is `Key::Id(x)`, then the wire-key match took its
`(None, Some(id))` arm, so `tc.index` was `None` and `tc.id` was `Some(x)` with
`x` non-empty (the arm's `.filter` guarantees it). The registration block at
`stream.rs:447` then stores that same non-empty `x`, and `canonicalize`'s
`or_insert_with` at `stream.rs:302-304` likewise stores a non-empty `call_id`.
So no `Key::Id` ever maps to `""`, and `call_id.is_empty()` implies an `Index`
key.

`HashSet<Key>` is chosen for type-fit with `tool_calls`, `name_emitted`,
`pending` and `warned_blank_id`; a `HashSet<u32>` would behave identically and
read worse against its neighbours. Because the Index-only property is a proof
rather than a type guarantee, the insert site carries

```rust
debug_assert!(
    matches!(key, Key::Index(_)),
    "a blank call_id is only reachable under an Index key"
);
```

so a future loosening of either premise surfaces instead of silently making the
set heterogeneous.

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
carries, adapted to litellm's vocabulary and citing SMA-619.

### 3.4 The withheld-upgrade warn

The `Some(_) => {}` arm above now absorbs a withheld upgrade as well as an
ordinary first-id-wins no-op, and the two must not look alike in the log. Split
it:

```rust
Some(existing) if existing.is_empty() && !id.is_empty() => {
    // Reached only when `blank_already_emitted` blocked the arm above.
    if self.warned_withheld_upgrade.insert(key.clone()) {
        tracing::warn!(
            target: "paigasus::litellm::stream",
            ?key,
            discarded_id = %id,
            "a real tool-call id arrived after this key already emitted under a \
             blank id; withholding the upgrade so the call is not split across \
             two call_ids. The call reaches the consumer under an empty call_id"
        );
    }
}
Some(_) => {}
```

Deduped per key, in the same shape as `warned_blank_id` and `warned_late_name`,
so a backend repeating the id on every delta warns once per call rather than
once per chunk.

### 3.5 The `blank_emitted` insert

At the emit site, after the "suppress a wholly empty event" early return at
`stream.rs:540-542` and before the `out.push`:

```rust
// Record that this key has emitted under a blank id, so the replacement
// rule above cannot later split the call in two.
if call_id.is_empty() {
    debug_assert!(
        matches!(key, Key::Index(_)),
        "a blank call_id is only reachable under an Index key"
    );
    self.blank_emitted.insert(key);
}
```

`key` is moved, not cloned: nothing between `stream.rs:540` and the `out.push`
at `544-548` reads it again. Unlike `openai/chat`'s `u32`, `Key` is not `Copy`,
so a `clone()` here would be a real allocation on every blank-emitting delta.

**Placement after the early return is load-bearing, and pinned.** A delta that
emits nothing has published the blank id to nobody, so it must not close the
upgrade window. `chat.rs:823` places it identically. Moving it above the early
return breaks `a_real_id_replaces_a_blank_one_on_the_same_wire_key` (§5.3): that
test's first delta emits nothing, so the key would be marked anyway, the upgrade
blocked, and it would yield `[("", "foo")]` instead of `[("c1", "foo")]`. §5.3
is therefore the pin for this placement as well as for §1.2.

### 3.6 The gate fires on *any* emission, not on a name-carrying one

This looks like a simplification opportunity and is not. The args-only shape
splits just as badly:

```rust
drive(&mut t, vec![
    tc_chunk(json!([{"index": 0, "id": "",   "function": {"arguments": "{\"a\":"}}])),
    tc_chunk(json!([{"index": 0, "id": "c1", "function": {"name": "alpha", "arguments": "1}"}}])),
])
```

Ungated, `"c1"` *does* receive the name — AC1 is technically satisfied — but the
arguments JSON is torn across two `call_id`s and neither half parses, so
`finish()` returns `Err` (§1.4). Narrowing the gate to name-carrying emissions
would trade a name-loss corruption for an arguments-loss corruption. One rule
covers both: any published delta closes the window. Pinned by §5.2 so the
narrowing is not attempted later as a cleanup.

### 3.7 Behaviour deltas beyond the two fixtures

Shapes whose output changes but which are not among the new tests, listed so a
reviewer diffing behaviour is not surprised:

- **A late *name* under the real id is now dropped.** `[{index:0,id:"",name:"alpha",args:"{}"}]`
  then `[{index:0,id:"c1",name:"beta",args:"{}"}]`. Pre-fix, `canonicalize` mints
  `Key::Id("c1")`, `name_emitted` has no entry for it, and `beta` is emitted as
  `("c1", Some("beta"), "{}")`. Post-fix the key stays `Key::Index(0)`,
  `name_emitted[Key::Index(0)] == "alpha"`, so the late-name `warn!` fires
  (`stream.rs:470-484`), `already_emitted` suppresses accumulation
  (`stream.rs:492`, `515`), and `beta` is dropped — emitting `("", None, "{}")`.
  **Accepted, and correct:** two names for one call is the violation this whole
  series exists to prevent, and the loss is already logged by the existing warn.
- **N parallel blank-id calls stay merged at the accumulator** — §2.2, tested.

### 3.8 `flush_buffered_names` does not record into `blank_emitted`

Neither crate records there (`stream.rs:642-646`, `chat.rs:488-492`), and the
omission is deliberate: `finish()` is terminal, so no replacement arm runs after
it. It is inert even under the double-`finish()` that
`finish_is_idempotent_after_draining` (`stream.rs:1286`) exercises, because the
first call drains `pending`. A one-line comment records this so the asymmetry
with the mid-stream emit site does not read as an oversight.

## 4. The change (`providers-openai`)

### 4.1 The doc comment

The closing paragraph of `ChatTranslator::handle_tool_call_chunk`'s doc comment
currently reads "The one remaining asymmetry is deliberate and ticketed
…(SMA-619)". Replaced with:

```rust
/// Both chat translators gate the blank→real `call_id` upgrade on
/// `blank_emitted`, so a call that has already emitted under `""` keeps the
/// blank rather than splitting across two ids (SMA-566 here, SMA-619 in
/// litellm), and both warn once when an upgrade is withheld. The
/// end-of-stream dedup net is likewise symmetric — both exempt blank
/// `call_id`s from it (SMA-616). No *gating* asymmetry remains. litellm's
/// two key spaces still admit a cross-key-space split this crate cannot
/// express at all; unticketed and left as-is (SMA-619 §7).
```

Two corrections against the wording this spec originally proposed. "Both
crates" is wrong — `providers-openai` also contains `responses.rs`, which has no
blank-id handling whatsoever (`rg blank_emitted crates/` returns five hits, all
in `chat.rs`) and whose own name-dedup defect is open as SMA-617. The claim is
scoped to the two *chat* translators. And a flat "No asymmetry remains" would
be believed by the next reader and stop them looking, three paragraphs before
§7 documents a live litellm-only divergence; it is scoped to *gating* and the
survivor is named.

### 4.2 The parity warn

`openai/chat` has the same silent-gate gap diagnosed in §2.1: its withheld
upgrade falls into `Some(_) => {}` at `chat.rs:717` and is logged nowhere. It
gets the same treatment as §3.4 — a `warned_withheld_upgrade: HashSet<u32>` and
the same one-shot `warn!` under `target: "paigasus::openai::chat"`.

This is the one place where SMA-619 changes `providers-openai` behaviour rather
than its comments. It is a diagnostic only: no emitted `ModelEvent` changes, and
no existing `openai/chat` test asserts on log output. It is in scope because
AC3 requires the two translators to carry the same gate, and a gate that is loud
in one crate and silent in the other is precisely the class of drift this ticket
closes. If it is judged out of scope at review, the fallback is to drop §4.2 and
§3.4 together — a silent gate in both crates is defensible; a loud one in only
one is not.

## 5. Tests

All in `crates/paigasus-helikon-providers-litellm/src/stream.rs`'s `mod tests`,
using the existing `tc_chunk` / `drive` / `named` / `args_of` helpers, each with
a doc comment in the module's established register (`stream.rs:1014-1018`,
`1600-1605`, `1734-1748`, `1975-1986`) recording the rationale and the verbatim
pre-fix output.

Every chunk list is written in the literal `drive(&mut t, vec![tc_chunk(..), tc_chunk(..)])`
form. The one-array-versus-two-chunks reading of a bare JSON block is ambiguous
in this module — `stream.rs:1960-1963` uses one chunk with two entries — and
although both readings produce identical output for every shape here, two
engineers would otherwise write two different tests.

### 5.1 New: `a_real_id_does_not_replace_a_blank_one_after_the_key_emitted`

The §1 trace, and AC1/AC2's fixture. Named to mirror `chat.rs:1706`'s
`a_real_id_does_not_replace_a_blank_one_after_the_index_emitted`, differing only
in `key`/`index` to match each crate's vocabulary. Asserts:

- `named(&evs) == [("", "alpha")]` — the name stays under the blank id it was
  emitted with.
- `args_of(&evs, "c1") == ""` — no delta arrives under the real id at all, which
  is the precise failure being prevented: a delta under `"c1"` could only be one
  carrying no name.
- `args_of(&evs, "") == "{}[]"` — every delta for this call stays under one
  `call_id`.

**Must FAIL against `main`,** on the second and third assertions: the pre-fix
translator produces `named == [("", "alpha")]` (which passes), but
`args_of("c1") == "[]"` and `args_of("") == "{}"`.

Per AC6 it also asserts the accumulator outcome decided in §1.4 — that
`finish()` is now `Err` on this synthetic shape, where pre-fix it was `Ok` — so
the trade is pinned rather than merely described. The doc comment carries §1.4's
reasoning for why that is accepted.

### 5.2 New: `the_gate_fires_on_an_args_only_emission`

§3.6's shape: the realistic one, and the one that improves on both axes. Asserts
the whole call, name and both argument fragments, arrives under `""` —
`named(&evs) == [("", "alpha")]`, `args_of(&evs, "") == "{\"a\":1}"`,
`args_of(&evs, "c1") == ""` — and, per AC6, that `finish()` returns `Ok` where
pre-fix it returned `Err`.

**Also fails against `main`, on all three event assertions:** the pre-fix
translator produces `named == [("c1", "alpha")]`, `args_of("") == "{\"a\":"` and
`args_of("c1") == "1}"`. It is a second defect proof, not merely a guard against
a future narrowing of the gate.

litellm-only. `openai/chat`'s gate is already pinned by `chat.rs:1706`, and
adding a second fixture there is outside this ticket.

### 5.3 Unchanged: `a_real_id_replaces_a_blank_one_on_the_same_wire_key`

Must still pass. Its first delta carries no `arguments`, so nothing is emitted,
`blank_emitted` never receives the key, and the upgrade proceeds as before. This
is the test that keeps §1.2 honest — it is what stops the fix from degenerating
into "delete the replacement arm" — and, per §3.5, it is also the pin for the
insert's placement after the early return.

Verified complete as the affected-existing-test set: of the four tests in this
module driving `"id": ""` (`stream.rs:1869`, `1961-1962`, `1993-1994`,
`2026-2027`), only `1869`'s has a follow-on real id on the same wire key.

### 5.4 New: `withheld_upgrades_keep_parallel_blank_calls_merged`

§2.2's shape. Asserts what the translator guarantees and what it does not: two
name-carrying deltas go out, `named(&evs) == [("", "alpha"), ("", "beta")]`,
satisfying SMA-616's event-layer rule; no delta reaches either real id; and
`finish()` returns `Err`, because the merged blank bucket concatenates two
argument objects. Its doc comment records that this is the accepted cost of the
gate, that `openai/chat` has carried the same trade since SMA-566, and that the
two `warn!`s from §3.4 are what make it diagnosable.

The fixture is deliberately three chunks, not two. The calls must emit before
their real ids arrive or the gate never engages — but if that emission carries
arguments, those arguments merge under `""` and fail the turn pre-fix and
post-fix alike, proving nothing about this gate. So chunk 1 buffers two names,
chunk 2 is a bare `[{"index": 0}, {"index": 1}]` completion signal that flushes
both under `""` with no arguments, and only chunk 3 carries arguments. That
isolates the gate's effect: `Ok` with three items pre-fix, `Err` post-fix.

## 6. Documentation

### 6.1 `crates/paigasus-helikon-providers-litellm/README.md` — changed

SMA-616's spec explicitly deferred this to the ticket that owns the residual:
"the litellm `README.md` `Limitations` section (`README.md:119-132`) … the
residual (`blank_emitted`) belongs to the follow-up ticket that owns it"
(`2026-09-06-sma-616-litellm-blank-id-flush-guard-design.md:403-407`). This is
that ticket, so declining silently is not available.

The section already documents an unrecoverable wire shape in exactly this
register (`README.md:126-132`, on a name fragment arriving after arguments
begin). A sibling bullet is added: a backend that sends `"id": ""` before a real
id, and whose first delta carries arguments, has its whole call delivered under
`call_id: ""` — the real id is discarded and logged at `warn`, because upgrading
after the fact would split the call in two. This is user-visible and permanent;
it belongs on the crate's crates.io page.

No change to the facade or root README: no crate-roster or feature-map change.

### 6.2 `paigasus-helikon-core` — unchanged

`ModelEvent::ToolCallDelta` already documents `name` as `Some` "exactly once per
non-blank `call_id`" (tightened by SMA-616, `core/src/model.rs:183-197`).
"Exactly once" already forbids zero; this ticket restores conformance to a
contract that is already written correctly. §2.2's accumulator merge is likewise
already documented there.

### 6.3 The conformance suite — unchanged

`tests/provider-stream-conformance` states in both its `openai_chat` and
`litellm` modules (`conformance.rs:573-583`, `1095-1122`) that canonicalization
regression coverage lives in each crate's own unit tests, and no fixture drives
a blank id. Note that `check.rs` contains no blank-`call_id` *exception*: its
assertion 7 (`check.rs:103-107`) fires for any `call_id` with `count != 1`,
including `""`, and the comment SMA-616 added (`check.rs:82-89`) says so
explicitly — it records that the assertion is deliberately **not** scoped, and
that the first blank-id fixture to arrive will trip it. That is unaffected here:
this ticket adds no fixture.

### 6.4 mdBook — unchanged

No page under `docs/book/src/` documents per-`call_id` name semantics beyond the
event shape (`concepts/agent-loop.md:57`, `concepts/model-providers.md:56`). A
conscious skip under CLAUDE.md's rule, matching SMA-616's.

### 6.5 Versions and CHANGELOGs — unchanged

No hand edits; release-plz owns both.

## 7. Out of scope

**The cross-key-space split.** litellm has two key *spaces*, so this shape also
splits:

```rust
drive(&mut t, vec![
    tc_chunk(json!([{"index": 0, "id": "", "function": {"name": "alpha", "arguments": "{}"}}])),
    tc_chunk(json!([{"id": "c1", "function": {"arguments": "[]"}}])),
])
```

Delta 2 has no `index`, so its wire key is `Key::Id("c1")` at `stream.rs:392` —
a different key entirely. It never touches `tool_calls[Key::Index(0)]`, the
replacement arm at `stream.rs:442` is never reached, and `blank_emitted` is
structurally unable to see the connection. Nothing on the wire asserts the two
deltas are one call, and `openai/chat` cannot express the shape at all
(`ChatCompletionMessageToolCallChunk::index` is a required `u32`), so there is
no symmetry argument for handling it either. Left as-is deliberately and named
in §4.1's doc comment so it stays discoverable; if a real backend produces it,
that is its own ticket with its own evidence.

**Validating blank `call_id`s downstream.** §2.3's rejected alternative.

**Rescuing a call already split by a released version.** Nothing here is
retroactive; the gate only affects streams translated after it lands.
