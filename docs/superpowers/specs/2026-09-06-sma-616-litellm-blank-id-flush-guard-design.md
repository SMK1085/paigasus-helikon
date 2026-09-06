# SMA-616 — Scope litellm's `flush_buffered_names` dedup net to non-blank `call_id`s

**Ticket:** [SMA-616](https://linear.app/smaschek/issue/SMA-616/litellm-flush-buffered-names-drops-a-blank-id-calls-name-via-the-call)
**Date:** 2026-09-06
**Related:** SMA-566 (added the guarded net to `openai/chat` and deferred this),
SMA-550 (introduced the net and the blank-id carve-out in `canonicalize`),
SMA-533 (the conformance suite, which stays green either way)

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
`blank_ids_do_not_collapse_distinct_calls`). Such deltas therefore stay on their
wire `Key`, but every one of them resolves through `tool_calls` to `call_id == ""`.

So two parallel **zero-argument** blank-id calls both reach the net carrying `""`.
The first claims it; the second is dropped, and the `error!` it fires blames "a
correlation-keying regression" that did not happen — the keying is behaving exactly
as designed.

The agent loop dispatches on the presence of an `Item::ToolCall` and reads the tool
to run from its `name`, so the dropped name is a real behavioural loss, not a
diagnostic one.

### 1.1 Why zero-argument is load-bearing

With arguments, both calls satisfy the mid-stream flush condition
(`!args_frag.is_empty()`) and emit their name from `handle_tool_call_delta`, never
reaching `flush_buffered_names`. `blank_ids_do_not_collapse_distinct_calls` drives
`"arguments": "{}"` and therefore exercises only that path. The end-of-stream net is
uncovered.

### 1.2 Why it was not fixed in SMA-566

SMA-566 hit the identical hazard while porting the net to `openai/chat` and guarded
it there (`!call_id.is_empty() && !already.insert(..)`, §3.6 of that spec). Fixing
litellm in the same PR would have turned a doc-only edit to that crate into a code
edit with its own version bump, for a defect SMA-566 does not name. The asymmetry is
deliberate and is cited from the guard comment SMA-566 added.

## 2. Decision

**Scope the at-most-one-name invariant to non-blank `call_id`s**, in `providers-litellm`,
exactly as `openai/chat` already does — guard the claim and filter the seeding.

No alternative is under consideration. Making the net blank-aware in some richer way
(keying the set on `(call_id, Key)`, say) would defeat its entire purpose: the net
exists precisely to catch two *different* keys resolving to one `call_id`. And
canonicalizing blank ids after all is the regression SMA-550 explicitly rejected.

## 3. The change

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
seed states the same "blank ids are exempt" rule as the guard, in the same place,
and so the two crates' seeds read identically. The `.cloned()` moves after the
filter, which also drops one clone per non-matching entry.

### 3.3 Scope the invariant in the doc comment

The method doc currently says the net "[s]kips … entries whose resolved `call_id`
already emitted a name". It gains the scoping note: the at-most-one invariant holds
for **non-blank** `call_id`s only. Two blank-id calls are not something the net can
fix — it is a property of an id that cannot identify.

The comment at the `continue` site gains the blank-id paragraph in the same words
`openai/chat` uses.

### 3.4 The `error!` message is unchanged

After the guard, the message only fires for a genuine non-blank collision, where
"a correlation-keying regression, not a backend quirk" is accurate. `openai/chat`
kept the identical wording under the identical guard.

## 4. Conformance-suite impact

**The SMA-533 suite stays green, and no case in it changes.** Its `litellm` module's
fixtures all carry real ids; there is no blank-id capture anywhere in the repo, and
the suite's fixture-provenance rule is that envelope shapes are transcribed from
captured traffic, never invented.

Were such a fixture ever added, the honest statement for litellm differs from the
one SMA-566 §3.6 makes for `openai/chat`, and the ticket's inherited phrasing ("both
before and after") is wrong here. `check.rs` groups name-carrying deltas by `call_id`
and violates on `count != 1`:

| | names under `""` | `classify` verdict |
|---|---|---|
| litellm, before this change | 1 (second dropped) | passes — *by losing a name* |
| litellm, after this change | 2 | `ToolNameNotExactlyOnce { call_id: "", count: 2 }` |
| `openai/chat`, before and after SMA-566 | 2 | `ToolNameNotExactlyOnce { call_id: "", count: 2 }` |

So this change trades a silent name loss that the checker reads as conformant for a
loud violation on a shape that cannot be conformant either way. That is the right
trade — the suite's "exactly once" is a statement about calls that *have* an
identity, and a blank id has none — but it is a change in the direction of the
hypothetical verdict, not a no-op, and the spec records it rather than repeating the
ticket's copy from SMA-566.

## 5. Aligning `openai/chat`

SMA-566 left three forward-references that go stale the moment this lands, all in
`crates/paigasus-helikon-providers-openai/src/backend/chat.rs`:

1. The seed comment — "…it matches `providers-litellm`'s seed once SMA-616 aligns
   the two (that seed is currently unfiltered)".
2. The guard comment — "`providers-litellm` carries the unguarded version of this
   net and loses that name today (SMA-616)".
3. The doc on `blank_ids_do_not_collapse_at_end_of_stream` — the same sentence.

All three are rewritten to **symmetric present tense**: each crate describes the same
rule in the same voice and names the other as carrying it too, so the pair reads as
one invariant implemented twice rather than as a migration in progress. Acceptance
criterion 3 ("`openai/chat` and `litellm` carry the same guard and the same scoping
note") requires this; leaving them would leave `openai/chat` asserting something
false.

This makes `providers-openai`'s half of the PR a comment-only edit.

## 6. Tests

**New:** `blank_ids_do_not_collapse_at_end_of_stream` in `stream.rs`'s unit module,
beside its sibling `blank_ids_do_not_collapse_distinct_calls`, mirroring the
`openai/chat` test of the same name.

Drives two parallel blank-id tool calls carrying a complete `name` and **no
`arguments` key at all**, so both stay buffered to end-of-stream and the flush net is
the only thing between them and the consumer. Asserts both names survive under `""`,
in wire order.

Per acceptance criterion 2, the test is verified to **FAIL** against the current
translator before the guard is applied — it is a defect proof for litellm, unlike the
`openai/chat` test of the same name, which passes both before and after and is a
guard only.

**Unchanged:** every existing test in both crates, and the conformance suite.

## 7. Out of scope

- **No mdBook or crate `README.md` edit.** This is an internal translator fix: no
  public API, feature flag, usage example, or crate-roster change. A conscious skip
  under CLAUDE.md's "make that a conscious call, not a silent skip" rule.
- **No hand version bump.** No `paigasus-helikon-core` API is added, so neither the
  core-bump nor the facade-bump caveat applies; release-plz bumps both provider
  crates from the squashed `fix(...)` commit in the normal flow.
- **No conformance-suite change** — see §4.
