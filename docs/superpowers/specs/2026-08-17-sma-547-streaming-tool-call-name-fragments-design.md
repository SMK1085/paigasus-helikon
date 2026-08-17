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
fragment. The other four providers receive the complete name in a single typed
field at block/item start, so their `name_emitted` flag is purely a
"don't repeat the name on every args delta" suppressor with nothing to fix:

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
`args`). This design does **not** add a second buffer beside it. Instead the
existing name buffer stops being cleared when the id resolves and becomes the
single accumulator for the whole call's name; the id now gates only *emission*,
not *accumulation*. `name_emitted` keeps its name but tightens in meaning: "the
complete name has been emitted for this call."

Once `call_id` is known, for each delta belonging to that call:

```text
pending_name += this delta's name fragment (if any)
args_out      = buffered pre-id args  ++  this delta's args fragment

flush  ⟺  !name_emitted
          && !pending_name.is_empty()
          && ( this delta's args fragment is non-empty
               || this delta carried no name fragment )
```

`args_out` is unchanged from today's behaviour — pre-id buffered args are still
prepended to the current fragment, and the flush condition tests *this delta's*
args fragment, not `args_out`. The distinction matters only on the delta where
the id first arrives: a call whose pre-id buffer holds args but whose id-carrying
delta contributes none does not, on that basis alone, count as evidence the name
is finished.

On flush, emit `name: Some(take(pending_name))` and insert into `name_emitted`;
otherwise emit `name: None`. **When `name` is `None` and the args fragment is
empty, emit no event at all** — `providers-litellm` already does this
(`stream.rs:220-222`); `providers-openai` currently pushes unconditionally and
adopts the guard.

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

This also fires on a **truncated** stream, where `finish()` currently returns an
empty vec because no `finish_reason` was seen. The flush is emitted; no `Finish`
is. Flushing is strictly more informative than dropping: the caller still learns
the turn was truncated (from the absent `Finish`), and additionally learns which
tool the model was calling. This matches the philosophy already stated at
`providers-litellm/src/stream.rs:252-258` — make the loss loud rather than
indistinguishable from "the model didn't call a tool."

## 3. A late fragment warns instead of vanishing

A name fragment arriving *after* the flush cannot be recovered — the
`ToolCallDelta` carrying the name has already been yielded downstream. This is
the one residual case, and no deferral scheme fixes it without abandoning
streaming names entirely (§ Alternatives, "always defer to `finish()`").

It is replaced with a `tracing::warn!` naming the `call_id` and the dropped
fragment, at targets `paigasus::openai::chat` / `paigasus::litellm::stream`.
Silence is what let the original defect survive review; a warning makes the
same class of backend behaviour visible next time.

Note the `target:` (colon) form — `target =` is the SMA-543 defect and must not
be reintroduced by these new call sites.

## 4. Core documentation

Doc-only; no behavioural change to `core`. Today's wording describes *position*
("`Some` on the first delta only"), not *completeness*, which is why the
defective emission read as conforming. Both sites are restated:

- `crates/paigasus-helikon-core/src/model.rs` — `ModelEvent::ToolCallDelta.name`
- `crates/paigasus-helikon-core/src/agent.rs` — `AgentEvent::ToolCallDelta.name`

to say: `Some` exactly once per `call_id`, on the first delta for which the
provider can establish the name is complete, `None` on every other delta; and
that when `Some`, the value is the **whole** name — a provider receiving the
name in fragments MUST buffer and concatenate, never emit a partial.

`ModelTurnAccumulator`'s keep-first behaviour is correct under this contract and
is **not** changed.

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

### Unit tests — both crates, in-module

Mirrored so the two crates cannot drift, alongside the existing
`orphan_name_buffered_and_flushed_with_id`:

1. fragmented name after the id resolves → assembled whole (**the regression
   test**);
2. single complete delta with non-empty args → name emitted on that same delta,
   no added latency;
3. name delta then args delta → name emitted on the args delta;
4. zero-argument sole delta → name flushed from `finish()`, ordered before
   `Finish`;
5. truncated stream with a buffered name → name flushed, no `Finish`;
6. late fragment after flush → warns, and the already-emitted name is unchanged.

### Regressions to keep green

- `providers-openai/tests/chat_streaming.rs::parallel_tool_calls_interleave_by_index`
  asserts `>= 4` `ToolCallDelta`s; the new rule yields exactly 4 and the
  name/args assertions still hold.
- `providers-anthropic/src/stream.rs:387` and
  `providers-openai/src/backend/responses.rs:556` assert `name.is_none()` on the
  second delta. Both providers are out of scope and untouched.
- `paigasus-helikon/tests/openai_litellm_message_parity.rs` covers
  `translate/request.rs` (`to_chat_messages`), a different file from the stream
  translators. The SMA-451 D6 byte-identity constraint does **not** reach this
  change. (SMA-547's ticket text implies otherwise; it is describing SMA-543.)

### Line endings

`.gitattributes` currently pins only
`crates/paigasus-helikon-providers-anthropic/tests/fixtures/*.txt` to
`text eol=lf`. The rule is extended to the `providers-litellm` and
`providers-openai` fixture directories, so a Windows checkout cannot break the
literal-`\n` splits the fixture harnesses rely on.

## 6. Docs and release mechanics

- `docs/book/src/concepts/agent-loop.md:57` — note the completeness guarantee
  where `ToolCallDelta` is listed under raw deltas for low-latency UIs.
- `crates/paigasus-helikon-providers-openai/README.md` and
  `crates/paigasus-helikon-providers-litellm/README.md` — note name buffering in
  the streaming section.
- **Version cascade, accepted:** touching `core/src/` even for doc comments makes
  release-plz patch-bump `paigasus-helikon-core`, which cascades to the facade
  and every dependent crate.
- **Commit scope:** `providers-litellm` is absent from `.versionrc`'s
  `scopeRegex`. Commits touching it use the `providers` parent scope; the PR
  title must too, since `pr-title.yml` runs on `pull_request_target` and reads
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

- The four non-fragmenting providers (§Blast radius).
- SMA-543 (`target =` → `target:`). Separate ticket; this design only requires
  that its *new* call sites use the correct form.
- Re-capturing `providers-openai`'s hand-authored `chat_parallel_tool_calls.txt`
  now that the §Evidence method makes a real capture possible. Worth doing;
  not here.
