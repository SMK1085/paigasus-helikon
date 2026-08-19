# SMA-550 — litellm stream translator emits two name-carrying deltas for one `call_id`

**Status:** approved
**Date:** 2026-08-19
**Ticket:** [SMA-550](https://linear.app/smaschek/issue/SMA-550/litellm-stream-translator-can-emit-two-name-carrying-deltas-for-one)
**Related:** SMA-547 (which surfaced this and deliberately left it unfixed), SMA-533 (which will assert the invariant this violates)

## The defect

`crates/paigasus-helikon-providers-litellm/src/stream.rs:178` correlates
streaming tool calls by a two-variant `Key`:

```rust
let key = match (tc.index, tc.id.as_deref()) {
    (Some(i), _) => Key::Index(i),
    (None, Some(id)) => Key::Id(id.to_owned()),
    ...
};
```

`name_emitted`, `warned_late_name` and `pending` (`:76`, `:79`, `:85`) are all
keyed by `Key`. A single `call_id` reached under **both** variants — one delta
carrying `index` *and* `id`, a later delta carrying only `id` — therefore
produces two independent state entries, and each can independently satisfy the
mid-stream flush condition at `:287`.

### Sequence A — the ticket's example

```json
{"index":0,"id":"c1","function":{"name":"get_","arguments":"{"}}
{"id":"c1","function":{"name":"weather","arguments":"}"}}
```

```
today -> ToolCallDelta{c1, Some("get_"),    "{"}
         ToolCallDelta{c1, Some("weather"), "}"}
```

Two name-carrying deltas for one `call_id`. `ModelTurnAccumulator`
(`core/src/model.rs`) keeps the first `Some(name)`, so the assembled call is
named `get_` and `weather` is lost — but both deltas reach any streaming
consumer, and that is what breaks the contract.

### Sequence B — not in the ticket, found while tracing

Drop the `arguments` fragment from the first delta and the failure changes
shape rather than disappearing:

```json
{"index":0,"id":"c1","function":{"name":"get_"}}
{"id":"c1","function":{"name":"weather","arguments":"{}"}}
```

```
today -> ToolCallDelta{c1, Some("weather"), "{}"}
```

Here `get_` buffers under `Key::Index(0)` and never flushes mid-stream (no args
fragment to complete it); `Key::Id("c1")` is a fresh slot that sees only
`weather`, and its args fragment flushes it. At end-of-stream SMA-547's
`already` guard (`:330`) correctly refuses to emit a second name for `c1`, so
`get_` is **silently discarded**.

Sequence B emits exactly one name-carrying delta, so it does **not** violate
the conformance assertion — it is simply, silently, the wrong name. This is the
finding that decides the approach: a fix built on suppression cannot reach it,
because suppression is already what discards `get_`.

## Second defect: the loss is undiagnosed

The ticket notes that in the dual-key case the fragment buffered under the
losing key is dropped with no diagnostic. The late-fragment `warn!` at `:237`
keys on `Key`, and the losing key never emitted, so it never fires. The ticket
proposes adding a `tracing::debug!` in the `flush_buffered_names` skip branch.

**This design does not add one.** The chosen fix removes the losing key
entirely, which restores the existing `warn!` on its own — see §Consequences.

## Reachability

LiteLLM 1.98.0 emits `index` on every tool-call delta, including continuations
carrying no `id` (container capture during SMA-547 — see that ticket's design
doc, §Evidence). So `Key::Id` is not reached by LiteLLM's own output and this
defect is **latent through the proxy today**.

It is reachable from any OpenAI-compatible backend behind the proxy that omits
`index` on continuation deltas, which is the only reason the `Key::Id` branch
exists. LiteLLM's purpose is fronting arbitrary backends, so the exposure is
"any backend that omits `index`", not "LiteLLM, which does not".

SMA-547's design doc states the position this ticket now reverses:

> The dual-keying hazard noted in §2 is therefore latent rather than reachable
> through this proxy — which is why §2 guards against it rather than
> restructuring the keying.

SMA-550 is that restructuring.

## Decision

Of the ticket's three options, this design takes **re-key on the resolved
`call_id`** (option 1).

### Rejected: guard the mid-stream flush the way the EOS flush is guarded

Small and local (~10 lines), and it does fix Sequence A. It fails on two
counts:

1. **It cannot fix Sequence B.** A `call_id`-keyed suppression set stops the
   *second* emission; it has no mechanism to reunite fragments that landed in
   two slots. Sequence B would still yield `weather`.
2. **It leaves the secondary defect open**, requiring the extra `debug!` the
   ticket describes, because the losing key still exists and still drops its
   buffer unheard.

It also entrenches the dual-key structure that SMA-533's assertion will keep
running into.

### Rejected: document as a known limitation

Ruled out by the ticket's own acceptance criteria, which require a test
verified to FAIL against the current translator. A documentation-only change
cannot produce one.

## Design

### Canonicalize the key on the resolved `call_id`

In `handle_tool_call`, immediately after the `call_id` resolves at `:226`,
rewrite the correlation key to a canonical `Key::Id(call_id)` and migrate any
fragments buffered under the pre-canonical key:

```rust
/// Rewrite `key` to the canonical slot for `call_id`, migrating any
/// fragments buffered under the pre-canonical key.
///
/// Every delta for one call — however it was keyed on the wire — shares one
/// state entry from this point on, which is what makes "at most one
/// name-carrying delta per call_id" hold by construction rather than by
/// guard.
fn canonicalize(&mut self, key: Key, call_id: &str) -> Key {
    let canonical = Key::Id(call_id.to_owned());
    if key == canonical {
        return canonical;
    }
    // Fragments buffered under the pre-canonical key arrived first, so they
    // lead whatever the canonical slot already holds.
    if let Some(old) = self.pending.remove(&key) {
        let slot = self.pending.entry(canonical.clone()).or_default();
        slot.name.insert_str(0, &old.name);
        slot.args.insert_str(0, &old.args);
    }
    // `flush_buffered_names` and `warn_unresolved_pending` both resolve a
    // pending key through `tool_calls`; the canonical key must resolve too,
    // or a canonicalized call would be skipped at flush and then reported as
    // an unresolved loss.
    self.tool_calls
        .entry(canonical.clone())
        .or_insert_with(|| call_id.to_owned());
    canonical
}
```

Call site, replacing nothing and inserting directly after the `let Some(call_id)
= ... else { ... }` block:

```rust
let key = self.canonicalize(key, &call_id);
```

Everything downstream — `name_emitted`, `warned_late_name`, `pending`, the
`already_emitted` capture at `:259`, the flush condition at `:287` — then
operates on the canonical key with **no change to its own type or logic**.

### Why this is complete

- Canonicalization runs *before* any emission on every delta, so `name_emitted`
  only ever receives canonical keys. One `call_id` therefore has exactly one
  entry in every map, and the at-most-one-name invariant holds structurally.
- The original `Key::Index(i) → call_id` entry stays in `tool_calls`, so a
  later index-only continuation delta still resolves.
- **At most one buffer ever migrates per call.** The only unresolved state is
  `Key::Index`-keyed: a `Key::Id` entry is unresolvable by definition, since it
  is keyed *by* the id whose presence is what resolves it. So the prepend is
  unambiguous in wire order — the migrating buffer is always the older one.
- `warn_unresolved_pending` keeps working: it filters on
  `!tool_calls.contains_key(k)`, and canonical keys are registered there, so a
  canonicalized call is never falsely reported as lost, while a genuinely
  id-less `Key::Index` buffer still is.

### Comments that must be rewritten, not left

Two existing comments become false and are corrected in the same change.

1. **`Key`'s `Ord` derive (`:33`)** is currently justified as "`Key::Index`
   sorting before `Key::Id` is what makes the dual-key winner predictable".
   There is no dual-key winner any more. The derive **stays** — `flush_buffered_names`
   still sorts for a deterministic flush order *between different calls* — but
   the stated reason is replaced. After this change all resolved pending keys
   are `Key::Id`, so the sort is lexicographic by `call_id`.

2. **`flush_buffered_names`'s `already` guard (`:330`)** — see below.

### Disposition of SMA-547's `already` guard

Canonicalization makes the guard unreachable: distinct `pending` keys can no
longer map to one `call_id`, so `already.insert(call_id)` can never return
`false`.

**The guard is kept**, with its doc comment rewritten to state that it is now
redundant given canonicalization and why it is retained anyway: it enforces the
at-most-one-name invariant at the point of emission, independent of the keying
discipline upstream of it — which is exactly the property SMA-533 will assert.
Deleting it would remove tested behaviour and the last line of defence if the
keying is ever loosened again.

### `providers-openai` alignment (AC #3)

`providers-openai`'s chat translator is **structurally immune** to Sequence A
and Sequence B: `async-openai` 0.41's `ChatCompletionMessageToolCallChunk.index`
is a required `u32`, not an `Option<u32>` (verified in the vendored source at
`async-openai-0.41.3/src/types/chat/chat_.rs:1124`). It therefore has a single
`u32` key space (`chat.rs:229`, `:403`) and no `Key` enum at all.

It is not *fully* aligned, and this change widens the gap by one shape. After
canonicalization, two deltas carrying different `index` values but the **same**
`id` merge into one call in litellm, whereas openai would keep them as two
indexes and emit a name for each. That shape is malformed — an id identifies a
call — and is not observed from any backend.

**Resolution: document the divergence in a code comment in
`providers-openai/src/backend/chat.rs`**, per the AC's second branch, rather
than changing openai's behaviour. A maintainer reading `chat.rs` and wondering
why it does not mirror litellm's canonicalization should find the answer there.

Accepted consequence: that comment edits a packaged file, so release-plz will
patch-bump `providers-openai` (0.2.22 → 0.2.23) and cascade the facade. This is
expected and harmless — a `docs`-type change touching a packaged crate file
still earns a bump.

## Consequences for behaviour

| Sequence | Today | After |
|---|---|---|
| **A** (args on delta 1) | `Some("get_")` + `Some("weather")` — **contract violation** | `Some("get_")` then `None`; the late-fragment `warn!` at `:237` fires for `weather` |
| **B** (no args on delta 1) | `Some("weather")` — one name, **silently wrong** | `Some("get_weather")` — fragments reunited |

Sequence A still loses `weather`, and that is correct and intended. Once
`arguments` arrive, SMA-547's design treats the name as complete and emits it;
a fragment arriving after that is genuinely unrecoverable because the event is
already downstream. What changes is that the loss is now **loud** rather than
silent — which is precisely the secondary defect the ticket asked to fix, and
it is fixed by removing the losing key rather than by adding a `debug!`.

## Testing

All tests live in `stream.rs`'s `mod tests`, where SMA-547's dual-key tests
already sit.

| Test | Asserts | Fails today? |
|---|---|---|
| `dual_key_call_emits_at_most_one_name_mid_stream` | Sequence A driven through `consume`; exactly one `Some(name)` per `call_id`, **mid-stream** — not merely after `finish()` | **yes** — the AC's required failing test |
| `name_fragments_split_across_the_key_boundary_reassemble` | Sequence B yields `get_weather` | **yes** — today yields `weather` |
| `dual_key_late_fragment_is_reported` | `warned_late_name` records the dropped `weather` in Sequence A, closing the secondary defect | yes |
| `one_call_id_under_two_keys_flushes_a_single_name` | **retargeted**: still one name-carrying delta, now `get_weather` not `get_`; the obsolete "Key::Index sorts before Key::Id" rationale removed from its assertion message | assertion updated |
| `flush_does_not_re_emit_a_name_already_flushed_under_another_key` | unchanged assertion; **docstring corrected** — it passes now because `name_emitted[canonical]` suppresses the second flush, not because of a sibling-key seed | no |
| `buffered_args_survive_a_bare_id_delta` | unchanged — pins that migration preserves drain-once args semantics across canonicalization | no |

**The two `yes` rows must be observed failing against unmodified `stream.rs`
before the fix lands**, and the observed failure output recorded in the
implementation plan. A test that has never been seen to fail proves nothing.

`dual_key_call_emits_at_most_one_name_mid_stream` must collect from the
`consume` return values, not from `finish()`. Asserting on `finish()` alone
would pass against today's code, because today's violation is two *mid-stream*
emissions and `finish()` returns neither.

## Documentation

- **mdBook: no edit.** Conscious call, not a silent skip. `docs/book/src/`
  mentions `ToolCallDelta` only as a `ModelEvent` variant name
  (`concepts/model-providers.md:56`); it documents no per-`call_id` name
  semantics, so nothing in the book becomes stale.
- **READMEs: no edit.** No public API, feature flag, install story, or crate
  roster change. `providers-litellm`'s public surface is untouched — this is
  entirely inside a `pub(crate)` translator.
- **CHANGELOGs: none by hand.** release-plz generates them.

## Release

`providers-litellm` is at **0.1.1** and `providers-openai` at **0.2.22** — both
long since released, so this is release-plz's normal flow. No stub-ascend
ritual, no manual `core` bump (nothing in `core` changes), and no manual facade
bump (nothing here defeats the `dependencies_update` cascade).

## Out of scope

- The four non-fragmenting providers (Responses, Anthropic, Bedrock, Gemini) —
  they receive a complete name in one typed field. Established in SMA-547's
  blast-radius table.
- `providers-openai`'s *behaviour*. Documented divergence only, per AC #3.
- SMA-533's conformance suite itself. This change makes `providers-litellm`
  pass the assertion SMA-533 will add; writing that suite is SMA-533's work.
- The `or_insert` policy when one `index` reports two different `id`s
  (`:220`). Pre-existing, unrelated to dual-keying, and unchanged here.
- Re-capturing fixtures. No wire-format understanding changes; the existing
  captured fixtures stay valid.
