# SMA-547 — Streaming tool-call name fragments dropped after the id resolves

**Status:** approved
**Date:** 2026-08-17
**Ticket:** [SMA-547](https://linear.app/smaschek/issue/SMA-547/streaming-tool-call-name-fragments-arriving-after-the-id-resolves-are)
**Related:** PR #199 (SMA-451, which introduced `providers-litellm` and whose review raised this), SMA-543

## The defect

Both OpenAI-Chat-compatible stream translators buffer tool-call `name` fragments
that arrive *before* `tool_calls[].id` is known and concatenate them correctly.
Once the id resolves and the name has been emitted, a `name_emitted` guard
suppresses every later fragment:

- `crates/paigasus-helikon-providers-openai/src/backend/chat.rs:381-390`
- `crates/paigasus-helikon-providers-litellm/src/stream.rs:213-218`

A backend that splits a function name across deltas *after* supplying the id
therefore yields a tool call named `get_` rather than `get_weather`. Silently:
nothing warns, and `ModelTurnAccumulator` (`core/src/model.rs:574-580`) keeps
only the first `Some(name)`.

## Blast radius: exactly two translators

Only these two receive the name as a per-delta `Option<String>` that can
fragment. The other four receive the complete name in one typed field and have
nothing to fix — though for two different reasons. Responses, Anthropic and
Bedrock get it at block/item start and carry a `name_emitted` flag that is purely
a "don't repeat the name on every args delta" suppressor. **Gemini has no such
flag at all**: `providers-gemini/src/stream.rs:40-51` emits one complete
`ToolCallDelta` per `functionCall` part, with `fc.name` a required `String`.

| Provider | Name source | Fragmentable? |
|---|---|---|
| `providers-openai` (chat) | `FunctionCallStream.name: Option<String>`, per delta | **yes** |
| `providers-litellm` | `ToolCallChunk.function.name: Option<String>`, per delta | **yes** |
| `providers-openai` (responses) | `output_item.added` → `fc.name: String` | no |
| `providers-anthropic` | `content_block_start` → `ToolUse { name }` | no |
| `providers-bedrock` | `ContentBlockStart::ToolUse` → `name` | no |
| `providers-gemini` | `functionCall.name`, one event | no |

Those four are **out of scope**.

## Evidence: the defect is reachable in production

The ticket presents the fragmented shape as hypothetical. It is not.

LiteLLM **1.98.0** was run in Docker against a local fake OpenAI-compatible
upstream emitting a name split across two post-id deltas. LiteLLM passed the
fragments through **verbatim** — `{"name":"get_"}` then `{"name":"weather"}`,
with no reassembly. LiteLLM does not normalize tool-call names.

This matters for severity. LiteLLM's purpose is fronting arbitrary backends, so
the exposure is "any backend that fragments a name," not "OpenAI, which does
not." The two captures are reproduced verbatim as the fixtures in §5.

### Capture method (and why the recorded blocker does not apply)

`providers-openai/tests/fixtures/chat_parallel_tool_calls.txt` carries this
provenance header, recorded during SMA-451:

> Provenance: HAND-AUTHORED. […] the parallel tool-call payload is hand-built
> because a keyless LiteLLM mock cannot emit streamed tool calls
> (`mock_tool_calls` is ignored on the proxy's streaming path).

That is accurate about `mock_response`/`mock_tool_calls`, and it is why this
design does **not** use them. Pointing LiteLLM at a local fake
OpenAI-compatible upstream via `api_base` sidesteps the limitation entirely:
the proxy performs its real translation over a real HTTP stream, and what it
emits is genuine LiteLLM output. No API key is involved.

Reproduction:

```yaml
# litellm_config.yaml
model_list:
  - model_name: shape-fragment
    litellm_params:
      model: openai/gpt-4o-mini-fragment
      api_key: sk-fake-not-used
      api_base: http://host.docker.internal:8099/v1
```

```bash
docker run -d --rm --name litellm -p 4000:4000 \
  -v "$PWD/litellm_config.yaml:/app/config.yaml" \
  ghcr.io/berriai/litellm:main-latest --config /app/config.yaml --port 4000
curl -N -X POST http://127.0.0.1:4000/v1/chat/completions \
  -H 'content-type: application/json' -H 'authorization: Bearer sk-1234' \
  -d '{"model":"shape-fragment","stream":true,
       "stream_options":{"include_usage":true},
       "messages":[{"role":"user","content":"weather in Berlin?"}],
       "tools":[{"type":"function","function":{"name":"get_weather",
                 "parameters":{"type":"object",
                 "properties":{"city":{"type":"string"}}}}}]}'
```

The fake upstream is ~90 lines of `http.server` returning a fixed SSE frame
list; it is a throwaway capture harness and is **not** committed. The captured
output is what the repo keeps.

### Captured frames (fragmented shape, `tool_calls` arrays only)

```json
{"id":"call_abc","function":{"arguments":"","name":"get_"},"type":"function","index":0}
{"function":{"arguments":"","name":"weather"},"type":"function","index":0}
{"function":{"arguments":"{\"city\":\"Berlin\"}"},"type":"function","index":0}
```

Two things this settles by inspection rather than assumption:

1. **LiteLLM does not reassemble the name** — `get_` and `weather` arrive as
   separate `name` fields on separate deltas, exactly as the fake upstream sent
   them.
2. **LiteLLM emits `index` on every tool-call delta**, including continuations
   that carry no `id`. So `providers-litellm` correlates these under
   `Key::Index(0)` throughout, and the `Key::Id` branch (`stream.rs:154`) is not
   reached by LiteLLM's own output. The dual-keying hazard noted in §2 is
   therefore latent rather than reachable through this proxy — which is why §2
   guards against it rather than restructuring the keying.

LiteLLM also reorders keys and adds `"type":"function"` to continuation deltas
that the upstream did not send; both are cosmetic and already tolerated by the
crate's `ToolCallChunk` deserializer.

## Decision

Of the ticket's three options, this design takes **defer the name until it can
be established as complete** (option 1). Rejected:

- *Allow fragments; make the accumulator append.* Would change the documented
  contract on `ModelEvent::ToolCallDelta` **and** `AgentEvent::ToolCallDelta` —
  the latter is `Serialize`d and streamed over SSE by `runtime-axum` /
  `runtime-actix`, so a naive external consumer doing
  `if let Some(n) = name { show(n) }` would render two "calling…" notices where
  it renders one today. A behavioural break for external consumers, to fix a
  defect that can be fixed without one.
- *Document the limitation, do not fix.* The capture above shows the shape is
  reachable through a real proxy, which removes the premise this option rests
  on.

## 1. The flush rule

Both translators already carry a per-call buffer for fragments that arrive
*before* the id is known (`PendingToolCall` / `Pending`, holding `name` and
`args`). This design does **not** add a second buffer beside it — but the two
fields acquire **different lifecycles**, which must be stated precisely because
the naive reading ("stop clearing the buffer") duplicates argument bytes on
every delta and corrupts the call:

- **`Pending.args` is drain-once, exactly as today.** It is taken on the first
  post-id delta and never re-prepended.
- **`Pending.name` accumulates** across every delta for the call and is cleared
  only by a flush.

Concretely: the entry is no longer `remove`d wholesale on the first post-id
delta. `args` is taken via `std::mem::take` on that delta; the entry itself
survives, holding only the growing `name`, and is removed when the name flushes
(or drained by `finish()`).

`name_emitted` keeps its name but tightens in meaning: "the complete name has
been emitted for this call."

Once `call_id` is known, for each delta belonging to that call:

```text
name_frag  = this delta's function.name  after unwrap_or("")
args_frag  = this delta's function.arguments after unwrap_or("")

pending.name += name_frag
args_out      = take(pending.args) ++ args_frag        // drain-once

flush  ⟺  !name_emitted
          && !pending.name.is_empty()
          && ( !args_frag.is_empty()  ||  name_frag.is_empty() )
```

Both conditions test the **post-`unwrap_or("")` effective fragment**, so a
backend sending `"name": ""` on continuation deltas is treated identically to
one omitting the field — matching how `chat.rs:376-380` already collapses the
two.

The flush condition deliberately tests `args_frag` (this delta's own
contribution), **not** `args_out`. A call whose pre-id buffer holds args but
whose id-carrying delta contributes none is not, on that basis alone, evidence
that the name is finished.

On flush, emit `name: Some(take(pending.name))` and insert into `name_emitted`;
otherwise emit `name: None`.

**The emit-nothing guard tests `args_out`, not `args_frag`.** When `name` is
`None` *and* `args_out` is empty, emit no event. Testing `args_frag` here would
silently discard buffered pre-id arguments on the sequence
`{index:0, arguments:"{\"a\":"}` (no id) → `{index:0, id:"c1"}` (no name, no
args): `args_out` holds `{"a":` but `args_frag` is empty, so the event would be
suppressed and the bytes lost. `providers-litellm` already tests the combined
value (`stream.rs:220`); `providers-openai` pushes unconditionally today and
adopts the guard **in the `args_out` form**.

The rule is the union of two independent completion signals, and is strictly
earlier than either alone:

- *non-empty args* catches the single-complete-delta shape with **zero** added
  latency;
- *no name fragment on this delta* catches a name fragmented across deltas
  whose `arguments` are all empty strings, which the args signal alone would
  defer to end-of-stream.

Traced against the two captures in §5:

| shape | delta 1 | delta 2 | delta 3 |
|---|---|---|---|
| normal | `id`, `name:"get_weather"`, `args:""` → hold, emit nothing | `args:"{\"city\":"`, no name → **flush `get_weather`** | `args:"\"Berlin\"}"` → `name:None` |
| fragmented | `id`, `name:"get_"`, `args:""` → hold | `name:"weather"`, `args:""` → hold | `args:"{…}"`, no name → **flush `get_weather`** |

### Observable change for `providers-openai`

The name event moves from the id-carrying delta to the following one, and the
two empty name-only deltas stop being emitted. For
`chat_parallel_tool_calls.txt` that is 4 `ToolCallDelta` events instead of 6,
with identical concatenated args and identical names.

## 2. Flushing at end-of-stream

A name still buffered when the stream ends — the sole-delta zero-argument shape,
where no args fragment ever arrives — is flushed from `finish()` as
`ToolCallDelta { name: Some(..), args_delta: String::new() }`, **ordered before
`Finish`**, preserving core's Finish-is-terminal contract.

**This is a correctness requirement, not a diagnostic nicety.** The agent loop
dispatches tools on the *presence* of an `Item::ToolCall`
(`core/src/loop_state.rs:281`), reading the tool to run from that item's `name`
(`loop_state.rs:325-336`). `ModelTurnAccumulator` fills an unseen name with
`unwrap_or_default()` (`core/src/model.rs:527`). So without this flush, a
zero-argument tool call reaches the dispatcher as `Item::ToolCall { name: "" }`
and fails to resolve — where today its name is emitted on the id-carrying delta
and it dispatches correctly. Skipping the flush, or implementing only its
clean-EOF half, silently breaks zero-argument tool calls.

### Which entries flush, and under which `call_id`

- The `call_id` comes from the resolved map — `self.tool_calls[&key]`.
- An entry whose id **never resolved** is *not* flushed; it has no `call_id` to
  emit under and remains the domain of `warn_unresolved_pending` (below).
- An entry is skipped if a name has already been emitted for its resolved
  `call_id`. This is not redundant with `name_emitted`: `providers-litellm`
  keys state by `Key::Index(i)` *or* `Key::Id(id)` depending on which field the
  delta carried (`stream.rs:153-155`), so one `call_id` can be reached under two
  keys. That already lets a name be emitted twice for a single call today; the
  flush must not add a third. The check is against a set of `call_id`s that have
  emitted a name, not against `name_emitted`'s `Key`s.

### `warn_unresolved_pending` must be narrowed

`providers-litellm`'s `finish()` calls `warn_unresolved_pending()` first
(`stream.rs:238`), and it iterates **all** of `self.pending`
(`stream.rs:259-269`), warning "fragments whose id was never resolved; dropping
them". Under this design `pending` routinely holds entries whose id *is*
resolved — that is the point of the change — so the warning would fire falsely
at `warn` level on every healthy stream that flushes a name. Two changes:

1. Narrow it to entries **absent from `self.tool_calls`** (genuinely unresolved).
2. Call it **after** the flush, so flushed entries are gone by then.

`providers-openai` has no equivalent warning today. The spec does not add one —
that would be scope creep — but §5's mirrored unit tests must not assume it
exists.

### Truncated streams

The flush also fires when no `finish_reason` was seen and `finish()` therefore
returns no `Finish`. This preserves today's behaviour rather than introducing
new tool execution: the loop already calls `acc.finish()` unconditionally when
the model stream drains (`core/src/agent.rs:978`) and `ModelTurnAccumulator`
already defaults `finish_reason` to `Stop` (`model.rs:558`), so a zero-argument
tool call on a truncated stream is dispatched **today**, with its name. Omitting
the flush here would change that; including it holds the line.

### Paths that never reach `finish()`

The flush is unreachable on `Some(Err(e))` (`chat.rs:86-89`), on cancellation
(`chat.rs:57`, `:71-75`), and on `providers-litellm`'s transport-error and
mid-stream-error-frame arms (`model.rs:158-161`, `:188-193`). A buffered name is
lost on those paths. This is **accepted**, following the precedent SMA-531 set
for `Finish` on dirty-cut truncation: a stream that died mid-transport has no
reliable turn to reconstruct.

## 3. A late fragment warns instead of vanishing

A name fragment arriving *after* the flush cannot be recovered — the
`ToolCallDelta` carrying the name has already been yielded downstream. This is
the one residual case, and no deferral scheme fixes it without abandoning
streaming names entirely (§ Alternatives, "always defer to `finish()`").

It is replaced with a `tracing::warn!` naming the `call_id` and the dropped
fragment, at targets `paigasus::openai::chat` / `paigasus::litellm::stream`.
Silence is what let the original defect survive review; a warning makes the
same class of backend behaviour visible next time.

**The warning must be deduplicated, or it becomes the noise it exists to cut
through.** Several OpenAI-compatible servers repeat the full function name on
*every* tool-call delta — a perfectly correct stream that would otherwise emit
one `warn` per argument chunk. Two suppressions:

1. Warn **at most once per `call_id`**, following the `warned_multi_choice: bool`
   pattern already used at `stream.rs:65,84-91`.
2. Warn **not at all** when the late fragment equals, or is a prefix of, the name
   already emitted for that call — that is a repeat, not a lost fragment.

Note the `target:` (colon) form — `target =` is the SMA-543 defect and must not
be reintroduced by these new call sites.

## 4. Core documentation — deferred to SMA-533

**`core` is not touched by this change.** No file under
`crates/paigasus-helikon-core/` is edited, so there is no core version bump and
no facade cascade.

Today's `ToolCallDelta.name` wording describes *position* ("`Some` on the first
delta only") rather than *completeness*, which is why the defective emission
read as conforming. Tightening it is genuinely worth doing, and an earlier draft
of this spec did it here. It was dropped on the project owner's decision at
GATE 1, for the reason SMA-533 itself states:

> Land the wording alongside the suite that enforces it, so prose and test cannot
> drift (compare SMA-532, where the Bedrock doc comments have already drifted
> from the contract they describe).

Adding a `MUST` here with nothing enforcing it is exactly that failure mode, and
the facade cascade would be paid twice — once for this prose, once for
SMA-533's.

### Handoff to SMA-533

SMA-533's "Also settle the contract wording" section currently names only
`model.rs:55-63` (`Finish` *emission*). It should additionally carry the
completeness wording at:

- `crates/paigasus-helikon-core/src/model.rs` — `ModelEvent::ToolCallDelta.name`
- `crates/paigasus-helikon-core/src/agent.rs` — `AgentEvent::ToolCallDelta.name`

stating: `Some` exactly once per `call_id`, on the first delta for which the
provider can establish the name is complete, `None` on every other delta; and
that when `Some`, the value is the whole name so far as the provider can
determine — a provider receiving the name in fragments MUST buffer and
concatenate them, and MUST NOT emit a name it can detect is still incomplete.

The "can detect" qualifier is load-bearing and must survive into the doc
comment. §1's args signal flushes on a single delta carrying both
`{"name":"get_","arguments":"{\"ci"}`, emitting `Some("get_")` — a partial. No
translator can rule that out without abandoning streaming names (§Alternatives).
An unqualified "never emit a partial" would make the two providers this ticket
fixes non-conformant with the contract. §3's warning is the
detection-after-the-fact escape hatch and should be cross-referenced.

A conformance assertion belongs with it: **for every provider, at most one
`ToolCallDelta` per `call_id` carries `Some(name)`.** That is mechanically
checkable over SMA-533's existing synthetic-stream table, and it is the
assertion that would have caught this defect.

`ModelTurnAccumulator`'s keep-first behaviour is correct under that contract and
is not changed by this ticket either.

## 5. Tests

### Captured fixtures — `providers-litellm`

The crate has **no** tool-call streaming fixture today (only
`text_then_trailing_usage`, `truncated_no_finish`, `unknown_finish_reason`,
`unparseable_frame`), so nothing pins the normal tool-call path this change also
touches. Two captures land, each with a provenance header naming LiteLLM 1.98.0
and the §Evidence method:

- `tests/fixtures/tool_call_stream.txt` — normal shape; asserts one
  `ToolCallDelta` carrying `Some("get_weather")`, args concatenating to
  `{"city":"Berlin"}`, `Usage` before a terminal `Finish { ToolCalls }`.
- `tests/fixtures/tool_call_stream_fragmented_name.txt` — the SMA-547 shape;
  asserts the assembled name is `get_weather`, **not** `get_`.

Driven end-to-end through the existing `events_for()` wiremock harness in
`tests/streaming.rs`.

Two conventions to follow:

- **Provenance headers use the SSE comment form** (`: Provenance: …`), as
  `providers-openai/tests/fixtures/chat_parallel_tool_calls.txt:1-4` does, so
  `eventsource-stream` ignores them. The existing litellm fixtures carry no
  header; these two introduce the convention to the crate.
- **Do not copy `streaming.rs:121-125`'s `usage_pos == evs.len() - 2`
  ("Usage must immediately precede Finish").** §2's flush can place a
  `ToolCallDelta` between them. Assert `usage_pos < finish_pos` instead. The
  existing assertion is on a text-only fixture and stays as it is.

### Unit tests — both crates, in-module

Mirrored so the two crates cannot drift, alongside the existing
`orphan_name_buffered_and_flushed_with_id` (`chat.rs:437`) and its litellm
counterpart `tool_call_id_arriving_late_does_not_lose_name_or_args`
(`stream.rs:493`):

1. fragmented name after the id resolves → assembled whole (**the regression
   test**);
2. single complete delta with non-empty args → name emitted on that same delta,
   no added latency;
3. name delta then args delta → name emitted on the args delta;
4. zero-argument sole delta → name flushed from `finish()`, ordered before
   `Finish`;
5. truncated stream with a buffered name → name flushed, no `Finish`;
6. late fragment after flush → warns once, and the already-emitted name is
   unchanged; a late fragment that repeats the emitted name does **not** warn;
7. **buffered pre-id args + a bare id-carrying delta** (no name, no args) → the
   buffered args are emitted, not swallowed by the emit-nothing guard (§1);
8. `finish()` **idempotency** — a second call yields nothing, because the flush
   *takes* its buffer. `providers-openai` has this for `Finish` at
   `chat.rs:666-677`; `providers-litellm` has no equivalent and gains one.

**Each new test must be demonstrated failing against the pre-fix code**, and the
observed failure recorded in the PR. This is the standard `providers-litellm`
already sets for itself (`tests/streaming.rs:44-49`: "This test is confirmed to
FAIL against the pre-fix code"), and it is what separates a regression test from
a test that cannot fail.

### Regressions to keep green

- **`providers-openai/src/backend/chat.rs:517-544
  ::orphan_name_concatenates_with_id_bearing_name` breaks under the new rule and
  must be rewritten.** It feeds `(0, None, Some("sea"), None)` then
  `(0, Some("c1"), Some("rch"), None)` and asserts `out.len() == 1` with
  `name == Some("search")`. Under §1 the second delta carries a name fragment and
  no args, so the name is held and nothing is emitted — `out.len() == 0`. The
  property it pins (buffered + id-chunk name fragments concatenate in order) is
  still correct and still worth pinning; the test is rewritten to consume both
  deltas and then call `finish()`, asserting the flush carries `Some("search")`.
  Do **not** delete it.
- `providers-openai/tests/chat_streaming.rs::parallel_tool_calls_interleave_by_index`
  asserts `>= 4` `ToolCallDelta`s; the new rule yields exactly 4 and the
  name/args assertions still hold. Verified by tracing all six fixture deltas.
- `providers-anthropic/src/stream.rs:387` and
  `providers-openai/src/backend/responses.rs:556` assert `name.is_none()` on the
  second delta. Both providers are out of scope and untouched.
- `paigasus-helikon/tests/openai_litellm_message_parity.rs` covers
  `translate/request.rs` (`to_chat_messages`), a different file from the stream
  translators. The SMA-451 D6 byte-identity constraint does **not** reach this
  change. (SMA-547's ticket text implies otherwise; it is describing SMA-543.)

Before implementing, grep both crates for every remaining test that asserts on
`ToolCallDelta` counts, name timing, or empty `args_delta` — the two above were
found that way, and the first was missed on the first pass.

### Line endings — no change needed

`.gitattributes:4` already pins `providers-litellm/tests/fixtures/*.txt` and
`:16` already pins `providers-openai/tests/fixtures/*.txt` to `text eol=lf`
(added by SMA-522). The new fixtures are covered on creation.

## 6. Docs and release mechanics

- `docs/book/src/concepts/agent-loop.md:57` — where `ToolCallDelta` is listed
  under raw deltas for low-latency UIs, note that a provider may buffer a
  fragmented tool name and that the name therefore arrives on the first delta the
  provider can establish it from. **Word this as provider behaviour, not as a
  core guarantee** — the guarantee is SMA-533's to state (§4), and the book must
  not get ahead of the contract it documents.
- **READMEs — neither has a "streaming" section today, so name the target
  precisely rather than inventing one.** The openai README is 32 lines
  (Install / Example / Links / License); it gains a short `## Streaming` section
  between Example and Links, stating that tool-call names are buffered until
  complete and that the name arrives on the first delta the translator can
  establish it from. The litellm README gains one bullet under its existing
  `## Limitations` (`:118`) covering the same point plus §3's residual case.
- **No manual version work, and no core bump.** Both touched crates are already
  released (`providers-openai` 0.2.21, `providers-litellm` 0.1.0), so release-plz
  performs their bumps itself and its `dependencies_update` cascade updates the
  facade automatically. The same-PR manual-bump ritual in CLAUDE.md applies to
  crates *ascending from 0.0.0* and to same-PR `core` API additions — neither
  applies here. A manual bump would in fact *defeat* the cascade.
- **Commit scope:** `providers-litellm` is absent from `.versionrc`'s
  `scopeRegex` (`:18`). Commits touching it use the `providers` parent scope; the
  PR title must too, since `pr-title.yml` runs on `pull_request_target` and reads
  the allowlist from `main`.

## Alternatives considered

**Always defer the name to `finish()`.** Maximally correct — even a backend
interleaving name fragments after arguments begin would assemble correctly,
eliminating §3's residual case. Rejected because it destroys the streaming
"calling `<tool>`…" affordance that `docs/book/src/concepts/agent-loop.md:57`
advertises `ToolCallDelta` for. Trading a real feature for a pathological case
no observed backend produces is the wrong trade.

**Flush on the first non-empty args fragment only** (the ticket's literal
suggestion). Simpler, one condition, and it fixes the reported shape. Rejected
because a name fragmented across deltas whose `arguments` are all empty strings
would defer to end-of-stream, where the union rule resolves it mid-stream at no
cost.

## Out of scope

- The four non-fragmenting providers (§Blast radius). Note one divergence this
  change introduces: `providers-openai`'s Responses backend
  (`responses.rs:360-373`) and `providers-bedrock` (`stream.rs:163-180`) emit a
  `ToolCallDelta` only from their argument-delta arms, so a zero-argument tool
  call produces no event at all in those providers. After §2 the two in-scope
  providers gain an end-of-stream flush the other two lack. That asymmetry is
  pre-existing and not made worse here, but it is worth a follow-up ticket.
- **Any edit to `paigasus-helikon-core`.** The `ToolCallDelta.name` completeness
  wording is deferred to **SMA-533**, which should be picked up next — see §4 for
  the exact wording and the conformance assertion to land with it.
- SMA-543 (`target =` → `target:`). Separate ticket; this design only requires
  that its *new* call sites use the correct form.
- Re-capturing `providers-openai`'s hand-authored `chat_parallel_tool_calls.txt`
  now that the §Evidence method makes a real capture possible. Worth doing;
  not here.
