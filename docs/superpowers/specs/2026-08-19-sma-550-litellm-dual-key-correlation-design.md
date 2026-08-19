# SMA-550 — litellm stream translator emits two name-carrying deltas for one `call_id`

**Status:** approved
**Date:** 2026-08-19
**Ticket:** [SMA-550](https://linear.app/smaschek/issue/SMA-550/litellm-stream-translator-can-emit-two-name-carrying-deltas-for-one)
**Related:** SMA-547 (which surfaced this and deliberately left it unfixed), SMA-533 (which will assert the invariant this violates)

## Acceptance criteria (quoted verbatim from the ticket)

> * A test drives the two-key sequence above through `consume` and asserts at
>   most one `ToolCallDelta` per `call_id` carries `Some(name)`, mid-stream
>   included.
> * The test is verified to FAIL against the current translator.
> * Whatever is chosen, `providers-openai`'s chat translator stays
>   behaviourally aligned or the divergence is documented in a code comment —
>   that alignment is an explicit constraint carried from SMA-547.

And the three options it offers:

> * **Re-key on the resolved** `call_id`**.** Once an id is known, migrate
>   `pending` / `name_emitted` / `warned_late_name` entries from the `Key` to
>   the `call_id`. Removes the hazard at the root; touches the most code.
> * **Guard the mid-stream flush the same way the EOS flush is guarded** —
>   check a `call_id`-keyed set before emitting `Some(name)`. Small and local,
>   mirrors what SMA-547 already established.
> * **Document it as a known limitation** and scope SMA-533's assertion
>   accordingly.

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
keyed by `Key`. A single `call_id` reached under **both** variants therefore
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

`get_` buffers under `Key::Index(0)` and never flushes mid-stream (no args
fragment to complete it); `Key::Id("c1")` is a fresh slot that sees only
`weather`, and its args fragment flushes it. At end-of-stream SMA-547's
`already` guard (`:330`) correctly refuses to emit a second name for `c1`, so
`get_` is **silently discarded**.

Sequence B emits exactly one name-carrying delta, so it does **not** violate
the conformance assertion — it is simply, silently, the wrong name. This is the
finding that decides the approach: a fix built on suppression cannot reach it,
because suppression is already what discards `get_`.

## The defect is broader than two sequences

The adversarial review of this spec's first draft produced three further shapes.
All three were **run against unmodified `stream.rs`** rather than reasoned
about; these are captured outputs, not predictions.

| # | Shape | Today | Silently lost |
|---|---|---|---|
| **C** | `{"id":"c1","name":"get_"}` → `{"index":0,"name":"weath"}` → `{"index":0,"id":"c1","name":"er","arguments":"{}"}` | `Some("weather")` | `get_` |
| **D** | one array: `{"index":0,"id":"c1","name":"alpha"}`, `{"index":1,"id":"c1","name":"beta","arguments":"{}"}` | `Some("beta")` | `alpha` |
| **E** | index-only, id-only, id-only, index-only, then both | `Some("IDXIDX")` | `ID` |

Shape C matters most: it is the same "backend keys inconsistently" premise the
whole ticket rests on, but with the id-keyed delta arriving **first**. It is the
shape that decides the merge order below.

## Second defect: the loss is undiagnosed

The ticket notes that in the dual-key case the fragment buffered under the
losing key is dropped with no diagnostic. The late-fragment `warn!` at `:237`
keys on `Key`, and the losing key never emitted, so it never fires. The ticket
proposes adding a `tracing::debug!` in the `flush_buffered_names` skip branch.

**This design does not add one there.** The chosen fix removes the losing key,
which restores the existing `warn!` on its own for Sequence A. It adds a
*different* diagnostic instead, on the new merge path — see §Design.

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

SMA-550 is that restructuring. Consequently both committed fixtures
(`tests/fixtures/tool_call_stream*.txt`) carry `index: 0` throughout and are
**unaffected**; every shape in this design is synthetic by necessity, and no
re-capture is planned or needed.

## Decision

Of the ticket's three options, this design takes **re-key on the resolved
`call_id`** (option 1).

### Rejected: guard the mid-stream flush the way the EOS flush is guarded

Small and local (~10 lines), and it does fix Sequence A. It fails on two counts:

1. **It cannot fix Sequence B, C, D or E.** A `call_id`-keyed suppression set
   stops the *second* emission; it has no mechanism to reunite fragments that
   landed in two slots. All four would still lose a fragment silently.
2. **It leaves the secondary defect open**, requiring the extra `debug!` the
   ticket describes, because the losing key still exists and still drops its
   buffer unheard.

### Rejected: document as a known limitation

Ruled out by the ticket's own acceptance criteria, which require a test
verified to FAIL against the current translator.

### Rejected: split the maps by resolution state

A structurally cleaner variant: keep `tool_calls: HashMap<Key, String>` as a
pure wire→`call_id` resolver, and key all mutable state by `String`
(`name_emitted: HashMap<String, String>`, `warned_late_name: HashSet<String>`,
`unresolved: HashMap<Key, Pending>`, `resolved: HashMap<String, Pending>`).
That makes the invariant true *by type* and needs no `tool_calls[Id(c)] → c`
self-mapping.

Rejected for this ticket on diff size: it rewrites every state access in
`handle_tool_call`, `flush_buffered_names` and `warn_unresolved_pending`, and
re-keys three fields, for a behavioural result identical to the chosen design —
including the merge-ordering problem below, which it does **not** solve. It is
worth revisiting if the keying grows a third variant. **It does not resolve the
ambiguity in §Merge order**, which is intrinsic to the wire format rather than
to the data structure.

## Design

> **The Rust snippets in this section are the original design sketch, and the
> implementation diverged from them.** They are kept as the record of what was
> designed; they are **not** the algorithm that shipped. Three things the
> sketches below do not have — an empty-`id` early return, whole-name-repeat
> suppression during the merge, and a warning when a fragment migrates into an
> already-emitted slot — were added after review found each one to be a
> regression against `main`. See §Three regressions the first implementation
> introduced. The authoritative algorithm is
> `crates/paigasus-helikon-providers-litellm/src/stream.rs::canonicalize`.

### `Pending` carries a creation sequence

```rust
/// Buffered name and args fragments for a tool call.
struct Pending {
    /// Monotonic creation order, used to merge two buffers for one call in
    /// wire order (§Merge order) and to give `flush_buffered_names` a
    /// deterministic order.
    seq: u64,
    name: String,
    args: String,
}
```

**The `#[derive(Default)]` on `Pending` is removed** and replaced with
`Pending::new(seq)`. This is deliberate and load-bearing: without `Default`,
every existing `.or_default()` call site fails to compile, so the compiler —
not review discipline — forces each one through the seq-assigning helper.

```rust
/// Get or create the buffer for `key`, assigning a creation `seq` on first use.
fn pending_mut(&mut self, key: Key) -> &mut Pending {
    let next = self.next_seq;
    if !self.pending.contains_key(&key) {
        self.next_seq += 1;
    }
    self.pending.entry(key).or_insert_with(|| Pending::new(next))
}
```

`ChatTranslator` gains `next_seq: u64`, initialised to `0` in `new()`.

### Canonicalize the key on the resolved `call_id`

Inserted immediately after the `let Some(call_id) = ... else { ... }` block at
`:226`:

```rust
let key = self.canonicalize(key, &call_id);
```

```rust
/// Rewrite `key` to the canonical slot for `call_id`, migrating any fragments
/// buffered under the pre-canonical key.
///
/// Every delta for one call — however it was keyed on the wire — shares one
/// state entry from here on, which is what makes "at most one name-carrying
/// delta per call_id" hold by construction rather than by guard.
fn canonicalize(&mut self, key: Key, call_id: &str) -> Key {
    // Already canonical: return without allocating. This is the common path —
    // LiteLLM itself keys every delta by `index`, so a stream that resolves
    // once would otherwise pay a `String` allocation per args chunk.
    if matches!(&key, Key::Id(id) if id == call_id) {
        return key;
    }
    let canonical = Key::Id(call_id.to_owned());
    if let Some(old) = self.pending.remove(&key) {
        let slot = self.pending_mut(canonical.clone());
        // A resolved slot drains `args` on every delta (`:282`) and is removed
        // when both fields empty (`:299`), so `slot.args` is always empty here
        // and this is an assignment, not a splice. The assert pins that
        // dependency: if drain-once is ever relaxed, this fails loudly instead
        // of silently mis-ordering JSON.
        debug_assert!(slot.args.is_empty(), "a resolved slot drains args each delta");
        slot.args.insert_str(0, &old.args);

        if !old.name.is_empty() && !slot.name.is_empty() {
            // Two buffers for one call both hold name fragments. Order by
            // creation seq — see §Merge order for why neither a plain prepend
            // nor a plain append is correct.
            tracing::warn!(
                target: "paigasus::litellm::stream",
                %call_id,
                "tool-call name fragments for one call arrived under two \
                 correlation keys; merging in buffer-creation order, which \
                 may misorder them if the keys interleaved"
            );
        }
        if old.seq < slot.seq {
            slot.name.insert_str(0, &old.name);
            slot.seq = old.seq;
        } else {
            slot.name.push_str(&old.name);
        }
    }
    // `flush_buffered_names` and `warn_unresolved_pending` both resolve a
    // pending key through `tool_calls`; the canonical key must resolve too, or
    // a canonicalized call is skipped at flush AND then falsely reported as an
    // unresolved loss. Pinned by `canonical_key_resolves_through_tool_calls`.
    self.tool_calls
        .entry(canonical.clone())
        .or_insert_with(|| call_id.to_owned());
    canonical
}
```

Everything downstream — `name_emitted`, `warned_late_name`, `pending`, the
`already_emitted` capture at `:259`, the flush condition at `:287` — then
operates on the canonical key with no change to its own logic.

### Merge order

The first draft of this spec claimed the migrating buffer "is always the older
one", and merged with an unconditional `insert_str(0, ..)` prepend. **That claim
was false**, and the adversarial review caught it. Both orderings are reachable:

| | First buffer created | Second | Correct result |
|---|---|---|---|
| **Shape C** | `Key::Id("c1")` = `get_` | `Key::Index(0)` = `weath` | `get_weather` |
| **Shape B′** | `Key::Index(0)` = `get_` | `Key::Id("c1")` = `weath` | `get_weather` |

A plain prepend yields `weathget_er` on C; a plain append yields `weathget_er`
on B′. **Only ordering by creation `seq` gets both right**, which is why
`Pending` carries one.

**Residual, stated rather than hidden.** `seq` orders *buffers*, not individual
fragments. In shape E the two buffers interleave at fragment level (index-only,
id-only, id-only, index-only), and no buffer-level order can reconstruct the
true sequence; the merge yields `IDXIDXIDIE`. That is still strictly better
than today's `IDXIDX`, which *silently discards* the `Id`-keyed fragments
entirely — the merge is lossless and now carries a `warn!`. Reconstructing
fragment order would require per-fragment sequencing, which is not worth it
for a shape no conforming backend emits.

### `flush_buffered_names` sorts by `seq`, not by `Key`

Today the sort is by `Key` (`:327`), which for resolved entries is
`Key::Index(i)` — numeric, i.e. wire order. After canonicalization every
resolved key is `Key::Id(call_id)`, so an unchanged sort silently becomes
**lexicographic by call_id**: two parallel zero-argument calls with ids
`call_z` (index 0) and `call_a` (index 1) would flip emission order.

This is user-visible in the raw `ToolCallDelta` stream that `runtime-axum` /
`runtime-actix` consumers see. (The *assembled* turn is unaffected —
`ModelTurnAccumulator` uses a `BTreeMap<String, ToolCallAccum>`,
`core/src/model.rs:494`, so it is already call_id-ordered.) Sorting by `seq`
instead preserves today's order exactly:

```rust
keys.sort_by_key(|k| self.pending[k].seq);
```

`seq` is unique per buffer, so the order is total and deterministic.

**Consequently `Key`'s `PartialOrd`/`Ord` derive (`:33-35`) is removed**, along
with its comment — that sort was its only consumer, and its stated rationale
("`Key::Index` sorting before `Key::Id` is what makes the dual-key winner
predictable") describes a dual-key winner that no longer exists. The
implementation must confirm by compilation that nothing else required it.

### Disposition of SMA-547's `already` guard

Canonicalization makes the guard unreachable: distinct `pending` keys can no
longer map to one `call_id`, so `already.insert(call_id)` can never return
`false`. The adversarial review independently confirmed this by attempting four
routes to reach it, and it held in all of them.

**The guard is kept**, with its doc comment rewritten to say it is now redundant
and why it is retained: it enforces the at-most-one-name invariant at the point
of emission, independent of the keying discipline upstream — exactly the
property SMA-533 will assert.

**Its `continue` gains a `tracing::error!`.** A silent defence is the wrong
shape here: if a future change reintroduces dual keying, a bare `continue`
drops a name without a word, which is *precisely* the "loss is undiagnosed"
defect this ticket exists to fix. It must be loud.

### `providers-openai` alignment (AC #3)

`providers-openai`'s chat translator cannot reach Sequences A–C or E:
`ChatCompletionMessageToolCallChunk.index` is a required `u32`, not an
`Option<u32>`. In-repo evidence: `chat.rs:229` declares
`tool_calls: HashMap<u32, String>`, `chat.rs:403` binds `let index = tc.index;`
with no `Option` handling, and the test helper at `chat.rs:528-537` constructs
`index` as a bare `u32`.

It is **not** fully aligned, and two things must be recorded honestly:

1. **This change widens the gap on shape D.** After canonicalization litellm
   emits `Some("alphabeta")` with both args streams merged into one buffer,
   where today it emits `Some("beta")`. openai keeps two indexes. So the
   divergence is "one lossy name → one merged name", not "two calls → one call"
   as the first draft claimed.
2. **openai *violates* SMA-533's assertion on shape D, and litellm does not.**
   openai emits `Some("beta")` mid-stream for index 1 and then `Some("alpha")`
   from `flush_buffered_names` for index 0 — **two name-carrying deltas for one
   `call_id`**. `chat.rs:350-377` has no `already` equivalent and no
   `call_id`-level dedup at all. This inverts the framing: after this change
   litellm is the *stricter* translator.

**Resolution: document both in a code comment in
`providers-openai/src/backend/chat.rs`**, per the AC's second branch, rather
than changing openai's behaviour here — shape D is malformed (an id identifies
a call) and unobserved, and fixing openai is a separate change with its own
blast radius. The comment is placed in `chat.rs` precisely *because* of point 2:
a maintainer reading that file needs to find the known gap there. **SMA-533 must
decide whether its suite covers shape D**; if it does, `providers-openai` goes
red and needs its own ticket. This is called out in the handoff.

Accepted consequence: the comment edits a packaged file, so release-plz will
patch-bump `providers-openai` (0.2.22 → 0.2.23) and cascade the facade.

### Comments that must be rewritten, not left

The first draft named two; the review found four more. All are false or
misleading after this change:

1. **`:33-35`** — `Key`'s `Ord` rationale. Removed with the derive.
2. **`:44-45`** — `Key::Id`'s variant doc, "Correlated by `delta.tool_calls[].id`,
   **when `index` is absent**". Post-fix `Key::Id` is the canonical state key
   *even when index was present*. The most misleading one left.
3. **`:69`, `:71`, `:80-85`** — `ChatTranslator` field docs ("keyed by
   correlation `Key`"); `tool_calls` now also holds `Id(c) → c` self-mappings.
4. **`:11-24`** — module invariant #2 documents the keying discipline in detail
   and must gain canonicalization, which is now the load-bearing rule.
5. **`:322-325`** — the whole "a call reached under both `Key::Index` and
   `Key::Id` has two entries for one `call_id`" paragraph, not just the guard
   sentence.
6. **`:350-352`** — "claiming earlier would let an empty-name entry … block a
   **sibling key**". There are no sibling keys post-fix.

## Three regressions the first implementation introduced

A high-effort local review of the finished branch found three shapes where the
merge in `canonicalize` was **worse than the code it replaced**. All three were
confirmed by running the same input against both this branch and `main`; they
are measurements, not predictions. This section records them because the design
above did not anticipate any of them — the omission was in this document, not
in the implementation of it.

| Shape | `main` | First implementation | Now |
|---|---|---|---|
| Repeated whole name across the key boundary: `{"id":"c1","name":"search"}`, `{"index":0,"name":"search"}`, `{"index":0,"id":"c1","arguments":"{}"}` | `Some("search")` | `Some("searchsearch")` | `Some("search")` |
| Blank id on two parallel calls: one array, both entries `"id": ""` | `alpha` **and** `beta` | `alpha` only — `beta` lost | `alpha` **and** `beta` |
| Migrate into an already-emitted slot: `{"index":0,"name":"foo"}`, `{"id":"c1","name":"bar","arguments":"{}"}`, `{"index":0,"id":"c1","arguments":"x"}` | `bar` + `foo` (two names — the original defect) | `bar`, `foo` silently stranded | `bar`, `foo` reported |

**1. The merge bypassed SMA-547's whole-name-repeat guard.** The wire path
guards `slot.name != name_frag` precisely so a backend that resends the
complete name on every delta gets `search`, not `searchsearch` — and the
existing comment there says appending unconditionally "would be a regression".
The migration path did exactly that. `canonicalize` now applies the same guard
to the migrating buffer. This design never mentioned the repeat guard, so the
interaction was never considered.

**2. An empty `id` was treated as an identity.** A backend sending `"id": ""`
on every entry collapsed all of its parallel calls into one `Key::Id("")` slot,
and every call but the first vanished from the stream — strictly worse than the
dual-keying this ticket targets. `canonicalize` now leaves a blank-id delta on
its wire key and warns once per key. Losing a whole call is worse than emitting
two name-carrying deltas for a `call_id` that is not an identity in the first
place.

**3. A fragment migrating into an already-emitted slot vanished silently.** It
was left in `pending` under the canonical key, where `flush_buffered_names`
skips it (the call_id has emitted) and `warn_unresolved_pending` ignores it (the
key now resolves) — no diagnostic anywhere. That is the exact undiagnosed loss
this ticket exists to eliminate, reintroduced by its own fix. It is now dropped
loudly and recorded in `warned_late_name`, the same way a late wire fragment is.

The general lesson for the §Merge order reasoning above: it treated the two
buffers as opaque strings to be ordered, when the wire path had already
established that name fragments need *content* rules (repeat detection) and
*state* rules (has this call already emitted?) as well as an ordering rule.

## Consequences for behaviour

| Shape | Today | After |
|---|---|---|
| **A** | `Some("get_")` + `Some("weather")` — **contract violation** | `Some("get_")` then `None`; late-fragment `warn!` (`:237`) fires for `weather` |
| **B** | `Some("weather")` — silently wrong | `Some("get_weather")` — reunited |
| **C** | `Some("weather")` — silently loses `get_` | `Some("get_weather")` — reunited |
| **D** | `Some("beta")` — silently loses `alpha` | `Some("alphabeta")` — merged, one name, `warn!` |
| **E** | `Some("IDXIDX")` — silently loses `ID`/`IE` | `Some("IDXIDXIDIE")` — lossless but misordered, `warn!` |

Sequence A still loses `weather`, and that is correct and intended: once
`arguments` arrive, SMA-547's design treats the name as complete and emits it,
and a fragment arriving after that is genuinely unrecoverable. What changes is
that every loss above is now either **repaired** or **loud**.

## Testing

All tests live in `stream.rs`'s `mod tests`, where SMA-547's dual-key tests
already sit.

| Test | Asserts | Fails today? |
|---|---|---|
| `dual_key_call_emits_at_most_one_name_mid_stream` | Sequence A: over **every event from every `consume` call plus `finish()`**, exactly one carries `Some(name)` for `c1` | **yes** — the AC's required failing test |
| `name_fragments_split_across_the_key_boundary_reassemble` | Sequence B yields `get_weather` | **yes** |
| `id_keyed_buffer_created_before_the_index_keyed_one_merges_in_order` | Shape C yields `get_weather`, not `weathget_er` — pins the `seq` merge against a plain prepend | **yes** |
| `index_keyed_buffer_created_first_merges_in_order` | Shape B′ yields `get_weather` — pins `seq` against a plain append | **yes** |
| `interleaved_dual_keying_is_lossless_and_misordered` | Shape E yields `IDXIDXIDIE`; documents the accepted residual so an implementer cannot "discover" it and treat it as a bug | **yes** |
| `two_indexes_with_one_id_merge_into_a_single_call` | Shape D yields exactly one `Some(name)` for `c1` | **yes** |
| `dual_key_late_fragment_is_reported` | after Sequence A, `warned_late_name` contains **`Key::Id("c1")`** (not `Key::Index(0)`) | yes |
| `canonical_key_resolves_through_tool_calls` | `tool_calls` contains the canonical key after a canonicalized stream — pins the self-mapping a future "cleanup" would remove | yes |
| `late_name_fragment_warns_once` (`:1078`) | **retarget**: `:1094` asserts `name_emitted.get(&Key::Index(0))`, which becomes `Key::Id("c1")` | assertion updated |
| `one_call_id_under_two_keys_flushes_a_single_name` (`:1119`) | **retarget**: still one name, now `get_weather` not `get_`; drop the obsolete "Key::Index sorts before Key::Id" rationale | assertion updated |
| `flush_does_not_re_emit_a_name_already_flushed_under_another_key` (`:1159`) | unchanged assertion; **docstring corrected** — it passes because `name_emitted[canonical]` suppresses the second flush, not because of a sibling-key seed | no |
| `buffered_args_survive_a_bare_id_delta` (`:1200`) | unchanged — pins drain-once args across canonicalization | no |

**Every row marked `yes` must be observed failing against unmodified
`stream.rs`, and the failure output recorded in the implementation plan.** A
test never seen to fail proves nothing.

Two rigour notes the review sharpened:

- `dual_key_call_emits_at_most_one_name_mid_stream` must collect across **all
  `consume` returns and `finish()`** — the invariant is per-stream. A
  `finish()`-only assertion passes vacuously against today's code, because
  today's violation is two *mid-stream* emissions and `finish()` returns
  neither.
- **Sweep criterion for the implementer:** grep `mod tests` for every assertion
  naming `name_emitted`, `warned_late_name`, `pending` or `tool_calls` with a
  `Key::` literal. The first draft missed `late_name_fragment_warns_once` by not
  doing this. The openai counterpart (`chat.rs:1066-1072`) additionally asserts
  `warned_late_name.contains(&0)`, which the litellm test lacks — mirror it
  while retargeting.

## Documentation

- **mdBook: no edit.** Conscious call. Note the first draft's reasoning was
  wrong: `docs/book/src/concepts/agent-loop.md:57-62` **does** document the
  per-`call_id` name semantics ("`name` is `Some` on the first delta the
  provider can establish the whole name from … and `None` on the rest"). No
  edit is needed because that prose already describes **post-fix** behaviour —
  it is today's code that deviates from the book, not the other way round.
- **READMEs: no edit**, same reasoning. `providers-litellm/README.md:125-131`
  already documents the buffering and the one unrecoverable shape (a fragment
  after arguments have begun), which remains exactly true after this change.
- **CHANGELOGs: none by hand.** release-plz generates them.

## Release and conventions

`providers-litellm` is at **0.1.1** and `providers-openai` at **0.2.22** — both
released, so this is release-plz's normal flow. No stub-ascend ritual, no manual
`core` bump (nothing in `core` changes), no manual facade bump.

**Commit and PR scope: use `providers`, not `providers-litellm`.**
`.versionrc:18`'s `scopeRegex` allows `providers`, `providers-openai` and
`providers-anthropic` but **not** `providers-litellm`. A
`fix(providers-litellm): …` title is rejected by the local `commit-msg` hook and
the `commits` CI job, and the PR title is validated against the allowlist as it
exists on `main` (`pr-title.yml` runs on `pull_request_target`). SMA-547's PR
used `fix(providers): …` for the same reason.

## Out of scope

- The four non-fragmenting providers (Responses, Anthropic, Bedrock, Gemini).
  Established in SMA-547's blast-radius table.
- `providers-openai`'s *behaviour*, including its shape-D violation of SMA-533's
  assertion. Documented only; needs its own ticket if SMA-533 covers shape D.
- SMA-533's conformance suite itself.
- The `or_insert` policy when one `index` reports two different `id`s (`:220`).
  Pre-existing and unchanged.
- A pre-existing false positive in `warn_unresolved_pending`: a bare
  `{"index":0}` delta with no id and no `function` creates an empty `Pending`
  (`:228-230`) and then warns at EOS about "dropping" fragments that never
  existed. Unchanged here; noted so the §Why-this-is-complete claim about that
  function is not read as a clean bill of health.
- Fixture re-capture. See §Reachability.
