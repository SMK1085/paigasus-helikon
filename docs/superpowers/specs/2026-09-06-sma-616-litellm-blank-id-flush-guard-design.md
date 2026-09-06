# SMA-616 — Scope litellm's blank-`call_id` handling: wire key and flush net

**Ticket:** [SMA-616](https://linear.app/smaschek/issue/SMA-616/litellm-flush-buffered-names-drops-a-blank-id-calls-name-via-the-call)
**Date:** 2026-09-06
**Related:** SMA-566 (added the guarded net to `openai/chat` and deferred this),
SMA-550 (introduced the net and the blank-id carve-out in `canonicalize`),
SMA-533 (the conformance suite), SMA-617 (`responses.rs`, the third implementation)

## 0. Acceptance criteria

Quoted from the ticket, each mapped to the section that discharges it:

1. *"A test drives two parallel **zero-argument** blank-id calls and asserts both
   names survive."* → §7.1. Note this criterion is **not** satisfiable by the flush
   guard alone: for the index-absent shape it also requires §3.3. See §1.3.
2. *"The test is verified to FAIL against the current translator."* → §7.1.
3. *"`openai/chat` and `litellm` carry the same guard and the same scoping note."* →
   §3.4 (the four litellm sites) and §6 (the four `openai/chat` sites).

## 1. The defect

`ChatTranslator::flush_buffered_names` in
`crates/paigasus-helikon-providers-litellm/src/stream.rs` keeps a `call_id`-level
dedup net over the end-of-stream flush:

```rust
let mut already: HashSet<String> = self
    .name_emitted
    .keys()
    .filter_map(|k| self.tool_calls.get(k).cloned())
    .collect();
// ...
if !already.insert(call_id.clone()) {
    tracing::error!(/* "a correlation-keying regression, not a backend quirk" */);
    continue;
}
```

`canonicalize` deliberately refuses to canonicalize a blank `id` — an empty id is
not an identity, and collapsing on it would make parallel calls vanish, which is
strictly worse than the dual-keying SMA-550 existed to fix (pinned by
`blank_ids_do_not_collapse_distinct_calls`, `stream.rs:1913`). Such deltas therefore
keep their wire `Key`, but every one of them resolves through `tool_calls` to
`call_id == ""`.

So two parallel **zero-argument** blank-id calls both reach the net carrying `""`.
The first claims it; the second is dropped, and the `error!` it fires blames "a
correlation-keying regression" that did not happen — the keying is behaving exactly
as designed.

### 1.1 Why zero-argument is load-bearing

The mid-stream flush condition is
`!self.name_emitted.contains_key(&key) && !slot.name.is_empty() && (!args_frag.is_empty() || name_frag.is_empty())`
(`stream.rs:507-509`). Omitting `arguments` yields `args_frag == ""` via
`unwrap_or("")` (`stream.rs:409-413`), which does **not** trip the flush while a name
fragment is present — confirmed independently by the existing passing test
`zero_argument_call_flushes_name_before_finish` (`stream.rs:1203`), which asserts the
mid-stream events are empty.

With arguments, both calls satisfy `!args_frag.is_empty()` and emit their name from
`handle_tool_call_delta`, never reaching `flush_buffered_names`.
`blank_ids_do_not_collapse_distinct_calls` drives `"arguments": "{}"` and therefore
exercises only that path. The end-of-stream net is uncovered.

### 1.2 What is actually lost — and what is not

**The agent loop's dispatch is unchanged, before and after this fix.**
`ModelTurnAccumulator` keys tool calls in a `BTreeMap<String, ToolCallAccum>` on
`call_id` and is first-name-wins (`crates/paigasus-helikon-core/src/model.rs:555,
585-591`). Two `ToolCallDelta { call_id: "" }` events collapse into **one** map entry,
so the loop gets exactly one `Item::ToolCall` named `"alpha"` either way. The
`"beta"` this fix restores never becomes a dispatchable tool call.

An earlier draft of this spec claimed the drop was "a real behavioural loss, not a
diagnostic one" for the agent loop. That was false, and SMA-566's spec had already
checked and recorded the accumulator's behaviour
(`2026-09-05-sma-566-…-design.md:377-382`). The change is still worth making, on the
benefits it actually has:

1. **The false `error!` stops firing.** Today a well-behaved backend sending blank
   ids triggers a log line asserting an internal keying regression that did not
   happen — actively misleading anyone debugging a stream.
2. **Raw-delta consumers see both names.** `ModelEvent::ToolCallDelta` and
   `AgentEvent::ToolCallDelta` reach streaming UIs and custom consumers that do not
   go through `ModelTurnAccumulator`. For them the second name is real data.
3. **Cross-provider consistency.** `openai/chat` emits both; litellm does not. Two
   first-party providers disagreeing on the same wire shape is the drift SMA-566
   set out to end.

Emitting one delta plus a `warn!` would fix benefit 1 alone. It is rejected: it
keeps litellm diverging from `openai/chat` (benefit 3) and keeps discarding data a
consumer asked for (benefit 2), to no gain.

### 1.3 A second, deeper blank-id collision: the wire key

The premise "blank-id deltas keep their wire `Key`, which keeps distinct calls
distinct" — asserted by the ticket, by `canonicalize`'s own comment
(`stream.rs:179-184`), and by this spec's earlier draft — **is false when `index` is
absent.** The wire key is chosen at `stream.rs:373-400`:

```rust
let key = match (tc.index, tc.id.as_deref()) {
    (Some(i), _)      => Key::Index(i),
    (None, Some(id))  => Key::Id(id.to_owned()),   // ← `id` may be ""
    (None, None) if any_explicit_index => { /* warn, skip */ }
    (None, None)      => Key::Index(pos as u32),
};
```

`ToolCallChunk::index` is `Option<u32>` and `id` is `Option<String>`
(`sse.rs:85-92`), so an array of
`[{"id":"","function":{"name":"alpha"}}, {"id":"","function":{"name":"beta"}}]`
gives **both** entries `Key::Id("")`. They share one `Pending` slot; the SMA-547
whole-name-repeat guard at `stream.rs:498` does not fire (`"alpha" != "beta"`); the
flush emits a single `("", "alphabeta")`.

Neither §3.1's guard nor §3.2's filter touches this — the collision happens at key
construction, before `canonicalize` is ever called. The shape is impossible in
`openai/chat`, whose `index` is a required `u32`, so this is a litellm-only hole that
a pure "mirror SMA-566" framing hides.

This is the same root cause one layer up: *an empty id is not an identity*. It is
fixed here, not deferred — see §2.

## 2. Decision

Two changes, both applications of one rule — **a blank `id` is treated as absent,
never as an identity**:

1. **At wire-key construction**, a blank `id` no longer produces `Key::Id("")`; the
   delta falls through to positional keying, which keeps parallel calls distinct.
2. **At the end-of-stream dedup net**, blank `call_id`s are exempt from the claim,
   and the seeding is filtered the same way — exactly as `openai/chat` already does.

Change 2 alone is what the ticket asks for. Change 1 is brought in because without
it, acceptance criterion 1 is only satisfiable for the index-present shape, and
because §3.4's prescribed doc sentence ("two blank-id calls are not something the
net can fix — it is a property of an id that cannot identify") would otherwise ship
as a false general claim, to be cited by the next ticket exactly as SMA-566's
comments are cited by this one.

**Alternatives considered and rejected:**

- *Making the net blank-aware in a richer way* (keying `already` on `(call_id, Key)`)
  defeats its entire purpose: the net exists precisely to catch two *different* keys
  resolving to one `call_id`.
- *Canonicalizing blank ids after all* is the regression SMA-550 explicitly rejected.
- *Deferring change 1 to its own ticket* would leave AC1 half-met and §3.4's comment
  overstated. The fix is one expression and is provably inert against every existing
  test (§7.3), which is what tips it in rather than out.
- *Emitting one delta plus a `warn!`* — see §1.2.

## 3. The change (`providers-litellm`)

### 3.1 Guard the claim

```rust
if !call_id.is_empty() && !already.insert(call_id.clone()) {
```

Short-circuiting on the blank id means `insert` never runs for it, so `""` is never
claimed and every blank-id call flushes its own name.

### 3.2 Filter the seeding

```rust
let mut already: HashSet<String> = self
    .name_emitted
    .keys()
    .filter_map(|k| self.tool_calls.get(k))
    .filter(|c| !c.is_empty())
    .cloned()
    .collect();
```

This filter is **redundant on its own** — §3.1's short-circuit already prevents a
blank entry in `already` from changing what gets claimed. It is added anyway so the
seed states the same "blank ids are exempt" rule as the guard, in the same place, and
so the two crates' seeds read identically. Moving `.cloned()` after the filter also
drops one clone per blank entry.

### 3.3 Treat a blank `id` as absent when choosing the wire key

```rust
let key = match (tc.index, tc.id.as_deref().filter(|id| !id.is_empty())) {
```

The match arms are otherwise unchanged. A blank id with no `index` now falls to the
existing `(None, None)` arms: positional keying, or the loud skip when another entry
in the same array carries an explicit index. Both are already the documented right
answers for "this delta has no usable identity", and the skip arm's rationale — a
synthesized positional key could collide with a genuine explicit `index` — applies
unchanged.

Every existing blank-id test carries an explicit `index` (`stream.rs:1827`, `1919`,
`1920`), so all take the first arm and are untouched (§7.3).

### 3.4 Scope the invariant in the doc comments — four sites

`openai/chat` already says "exactly one name-carrying `ToolCallDelta` per **non-blank**
`call_id`" (`chat.rs:648-649`). litellm states it unqualified in four places, all of
which gain the qualifier so AC3 is actually met:

| Site | Current text |
|---|---|
| `stream.rs:29-30` (module doc) | *"…what makes 'at most one name-carrying delta per `call_id`' structural rather than guarded (SMA-550)"* |
| `stream.rs:173-176` (`canonicalize` doc) | *"…what makes 'at most one name-carrying `ToolCallDelta` per `call_id`' hold by construction rather than by guard"* |
| `stream.rs:541-544` (`flush_buffered_names` doc) | *"Skips … entries whose resolved `call_id` already emitted a name"* |
| `stream.rs:575-586` (the `continue` comment) | the `error!` rationale |

`canonicalize`'s blank-id comment (`stream.rs:179-184`) additionally has its "which
keeps distinct calls distinct" claim corrected — after §3.3 that is true, but it is
true *because* of §3.3, not automatically, and the comment should say so.

### 3.5 The `error!` message is unchanged

After the guard it only fires for a genuine non-blank collision, where "a
correlation-keying regression, not a backend quirk" is accurate. `openai/chat` kept
the identical wording under the identical guard.

## 4. Conformance-suite impact

**The suite stays green and no case in it changes.** Its `litellm` module's fixtures
all carry real ids; there is no blank-id capture anywhere in the repo, and the
fixture-provenance rule is that envelope shapes are transcribed from captured
traffic, never invented.

Were such a fixture added, `check.rs:73-98` groups name-carrying deltas by `call_id`
and violates on `count != 1`. For **two parallel zero-argument blank-id calls**:

| | names under `""` | `classify` verdict |
|---|---|---|
| litellm, before this change | 1 (second dropped) | passes — *by losing a name* |
| litellm, after this change | 2 | `ToolNameNotExactlyOnce { call_id: "", count: 2 }` |
| `openai/chat`, before and after SMA-566 | 2 | `ToolNameNotExactlyOnce { call_id: "", count: 2 }` |

So this change trades a silent name loss that the checker reads as conformant for a
loud violation on a shape that cannot be conformant either way. That is the right
trade — "exactly once" is a statement about calls that *have* an identity — but it is
a directional change in the hypothetical verdict, not a no-op. The ticket's inherited
phrasing ("violate … both before and after") is correct for `openai/chat` and wrong
for litellm; this spec states the litellm case rather than repeating it.

**With arguments**, both crates already emit two names under `""` and already violate,
before and after. The row above is specific to the zero-argument shape.

### 4.1 Record the exception in `check.rs`

Assertion 7's doc block (`check.rs:75-80`) gains one sentence: blank `call_id`s are a
known, deliberate exception, because an id that cannot identify cannot carry a
per-`call_id` invariant. Doc-only — **the assertion itself is not scoped**. Narrowing
a shared cross-provider gate is a behaviour change to a required CI job that no
fixture currently exercises; it deserves its own decision, not a drive-by.

Without this note, the next person to add a blank-id capture gets a red suite for
behaviour two specs deem correct, with nothing in `check.rs` explaining it — and the
net's own justification comment ("*which is precisely what a cross-provider
conformance suite asserts*", `stream.rs:583-584`) reads as self-contradictory.

## 5. Scope the `paigasus-helikon-core` event contract

`ModelEvent::ToolCallDelta`'s doc states *"`name` is `Some` exactly once per
`call_id`"* twice (`core/src/model.rs:183-188`, `192-196`), and
`AgentEvent::ToolCallDelta` restates it (`core/src/agent.rs:384`). After this change
litellm deliberately emits two `Some(name)` under `call_id: ""`; `openai/chat`
already does. All three are scoped to **non-blank** `call_id`s.

This is the SDK's public event contract, not an internal comment: a third-party
provider implementor would otherwise build to a rule both first-party providers break
by design. **Doc-only** — no API is added, so CLAUDE.md's core-bump and facade-bump
caveats do not fire (§8).

## 6. Aligning `openai/chat`

`grep -n 'SMA-616' crates/paigasus-helikon-providers-openai/src/backend/chat.rs`
returns **four** sites, not the three an earlier draft listed:

| Line | Content | Treatment |
|---|---|---|
| 432 | seed comment — *"…matches `providers-litellm`'s seed once SMA-616 aligns the two (that seed is currently unfiltered)"* | symmetric present tense |
| 475 | guard comment — *"`providers-litellm` carries the unguarded version of this net and loses that name today"* | symmetric present tense |
| 666 | *"The one remaining asymmetry is deliberate and ticketed: this crate's end-of-stream dedup net excludes blank `call_id`s, litellm's does not."* | **retargeted — see below** |
| 1768 | test doc — same sentence as 475 | symmetric present tense |

Three become symmetric present tense: each crate describes the same rule in the same
voice and names the other as carrying it too, so the pair reads as one invariant
implemented twice rather than a migration in progress. Leaving them would leave
`openai/chat` asserting something false.

**Line 666 cannot take that treatment.** It names the dedup-net asymmetry, which this
change removes — but rewriting it to claim *no* remaining asymmetry would itself be
false. `openai/chat` carries `blank_emitted: HashSet<u32>` (`chat.rs:287, 688, 822`)
gating the blank→real `call_id` upgrade; litellm has **zero** occurrences and upgrades
unconditionally (`stream.rs:425-427`). Traced:
`[{index:0,id:"",name:"alpha",args:"{}"}, {index:0,id:"c1",args:"[]"}]` emits
`("", Some("alpha"), "{}")` then `("c1", None, "[]")`, leaving the **non-blank** `"c1"`
with zero name-carrying deltas — the exact trade `chat.rs:706-716` calls "worse than
the stuck blank". litellm's `a_real_id_replaces_a_blank_one_on_the_same_wire_key`
(`stream.rs:1821`) covers only the safe case where nothing has emitted yet.

So line 666 is **retargeted** to name `blank_emitted` as the remaining asymmetry,
citing the follow-up ticket (§8).

This makes `providers-openai`'s half of the PR comment-only.

## 7. Tests

### 7.1 New: `blank_ids_do_not_collapse_at_end_of_stream`

In `stream.rs`'s unit module beside `blank_ids_do_not_collapse_distinct_calls`,
mirroring the `openai/chat` test of the same name. Exact shape, indexes explicit so
it exercises §3.1's guard rather than §3.3's key fix:

```rust
let evs = drive(
    &mut t,
    vec![tc_chunk(serde_json::json!([
        {"index": 0, "id": "", "function": {"name": "alpha"}},
        {"index": 1, "id": "", "function": {"name": "beta"}}
    ]))],
);
assert_eq!(
    named(&evs),
    vec![(String::new(), "alpha".to_owned()), (String::new(), "beta".to_owned())],
);
```

No `arguments` key at all, so both stay buffered to end-of-stream (§1.1) and the
flush net is the only thing between them and the consumer.

Per AC2, verified to **FAIL** against the current translator, with the expected
pre-fix output stated verbatim: `[("", "alpha")]` — the second dropped at
`stream.rs:596` with an `error!`. Unlike the `openai/chat` test of the same name,
which passes before and after and is a guard only, this one is a defect proof.

### 7.2 New: an index-absent companion

The same two calls with **no `index` key**, pinning §3.3:

```rust
tc_chunk(serde_json::json!([
    {"id": "", "function": {"name": "alpha"}},
    {"id": "", "function": {"name": "beta"}}
]))
```

Pre-fix this yields a single `("", "alphabeta")` — the `Key::Id("")` collision of
§1.3, a different failure from §7.1's and one the flush guard alone does not fix.
Post-fix both names survive under `""`.

### 7.3 Unchanged

Every existing test in both crates, and the conformance suite. Verification that
§3.3 is inert: all three existing blank-id test literals carry an explicit `index`
(`stream.rs:1827`, `1919`, `1920`) and so take the unchanged first match arm.

## 8. Out of scope

- **Porting `blank_emitted` to litellm** (§6) — a distinct defect on a distinct shape
  (blank→real upgrade *after* emission) needing its own test matrix. SMA-566 set the
  precedent by splitting the `responses.rs` equivalent out as SMA-617 rather than
  absorbing it. **A follow-up ticket is filed and cited from `chat.rs:666`**; this
  spec does not land the fix.
- **Scoping `check.rs`'s assertion 7** to non-blank `call_id`s — §4.1 adds the doc
  note only.
- **The litellm `README.md` `Limitations` section** (`README.md:119-132`) documents
  wire-shape caveats of exactly this class. It is left alone: this PR *removes* two
  such caveats rather than adding one, and the residual (`blank_emitted`) belongs to
  the follow-up ticket that owns it.
- **mdBook** — no page under `docs/book/src/` documents per-`call_id` name semantics
  beyond the event shape (`concepts/agent-loop.md:57`,
  `concepts/model-providers.md:56`). A conscious skip under CLAUDE.md's rule.
- **Hand version bumps** — no crate gains public API (core's edit is doc-only), so
  neither the core-bump nor the facade-bump caveat fires; release-plz bumps the
  touched crates from the squashed `fix(...)` commit in the normal flow.
