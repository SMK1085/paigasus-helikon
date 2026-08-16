# SMA-522 — OpenAI `Finish`/`Usage` ordering

**Date:** 2026-08-16
**Issue:** [SMA-522](https://linear.app/smaschek/issue/SMA-522/openai-provider-emits-finish-before-usage-violating-the-core-event)
**Crate:** `paigasus-helikon-providers-openai`
**Status:** approved

## Problem

`paigasus_helikon_core::Model::invoke` states the event-ordering contract
(`crates/paigasus-helikon-core/src/model.rs:55-63`):

> - `Usage` MAY appear anywhere; most providers emit one immediately before
>   `Finish` […] Each `Usage` is a complete snapshot (last-wins) […]
> - `Finish` is the terminal event; nothing follows it.

Only `Finish` is positionally constrained. `Usage` is not.

`ChatTranslator::consume` (`crates/paigasus-helikon-providers-openai/src/backend/chat.rs:242-303`)
orders `Usage` before `Finish` **within a single chunk**, then appends `Finish`
at the end of the chunk that carried `finish_reason`. With
`stream_options.include_usage: true`, OpenAI does not put usage on that chunk —
it arrives on a **separate trailing chunk**. The provider therefore emits
`Finish` and then `Usage`, on every streaming turn.

A consumer that stops reading at `Finish` — which the contract explicitly
licenses — drops the turn's only usage snapshot. SMA-402's cross-turn token
summing then silently under-counts every OpenAI turn.

## Evidence

Reproduced first-hand against a keyless LiteLLM proxy (`main-stable`,
Docker 29.6.2). Full traces in Appendix A. The essential shape:

```text
data: {…"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}     ← Finish emitted here
data: {…"choices":[{"index":0,"delta":{}}],"usage":{…}}                ← Usage arrives after
data: [DONE]
```

Two details the issue does not record, both confirmed by capture:

1. The usage-bearing chunk still carries `choices: [{index:0, delta:{}}]`, and
   omits the `finish_reason` key entirely (it is absent, not `null`).
2. With `include_usage` omitted, **no trailing usage chunk appears at all** —
   the stream ends `finish_reason` chunk → `[DONE]`. This case decides the
   design (below).

## Why the current tests do not catch it

Both Chat fixtures encode a wire shape that does not occur — they weld `usage`
onto the `finish_reason` chunk:

- `tests/fixtures/chat_parallel_tool_calls.txt:13`
- `tests/fixtures/chat_content_filter.txt:3`

The issue names only the first. Against either, the inline-`Finish`
implementation looks correct, so a test written from them passes on broken
code.

This has a sharp consequence for the fix: **the translator change alone is
behaviourally invisible against the current fixtures.** With usage on the
finish chunk, the old code returns `[Usage, Finish]` from one `consume` call
and the new code returns `[Usage]` then `[Finish]` from `finish()` — the
emitted sequence is identical. The fixtures are the entire regression signal.
Re-recording is not the polish on this ticket; it is the substance.

## Design

### Chosen: EOF-anchored `finish()` (the Gemini pattern)

`ChatTranslator` gains a `finish_reason: Option<FinishReason>` field. `consume`
keeps its delta and `Usage` emission unchanged but stashes the mapped finish
reason instead of appending `Finish`. A new `finish()` takes the stash and
returns `vec![ModelEvent::Finish{..}]`, or an empty vec when no `finish_reason`
was ever observed.

Multi-choice last-wins semantics are preserved: today the loop overwrites
`finish_event` per choice, and the stash overwrites identically.

### Rejected: pair `[Usage, Finish]` on the usage event (the Bedrock pattern)

`paigasus-helikon-providers-bedrock/src/stream.rs:14-22` solves the same
problem by buffering the stop reason and emitting `Usage` immediately followed
by `Finish` when the `Metadata` event lands.

Captured evidence rules this out for OpenAI. The two patterns diverge exactly
when the trailing event never arrives:

| Pattern | Normal stream | No usage chunk (Appendix A.2) |
| --- | --- | --- |
| Gemini — buffer reason, emit `Finish` at EOF | `…Usage, Finish` ✅ | `…Finish` ✅ |
| Bedrock — emit `[Usage, Finish]` on usage | `…Usage, Finish` ✅ | **no `Finish` at all** ❌ |

Adopting Bedrock's shape would trade an ordering violation for a missing
terminal event. That matters concretely: SMA-451 targets third-party
OpenAI-compatible proxies, which may ignore `stream_options` entirely.

The same reasoning implies a latent gap in the Bedrock provider itself — a
stream ending after `MessageStop` without a `Metadata` event emits no `Finish`.
Out of scope here; filed as a follow-up.

### Rejected: a generic reordering combinator in core

A stream adapter wrapping every provider would fix all of them at once, but it
masks provider bugs rather than fixing them, adds per-event cost to every
stream, and duplicates the cross-provider conformance work already scoped to a
follow-up.

## Changes

### `src/backend/chat.rs`

1. Add `finish_reason: Option<FinishReason>` to `ChatTranslator`; initialise to
   `None` in `new()`.
2. In `consume`, replace the local `finish_event` with a write to
   `self.finish_reason`, and delete the trailing "append Finish last" block.
   `Usage` emission is unchanged.
3. Add `pub(crate) fn finish(&mut self) -> Vec<ModelEvent>` returning the
   buffered `Finish`, or empty when none was observed.
4. In `invoke`'s driver loop, change the stream-exhausted arm:

   ```rust
   None => {
       for ev in translator.finish() { yield Ok(ev); }
       return;
   }
   ```

   `async-openai`'s `create_stream()` consumes `[DONE]` internally, so this is
   the single finish site — the OpenAI analogue of Gemini's two
   (`providers-gemini/src/model.rs:150,156`).

5. Correct the doc comment at `chat.rs:237-241`. It asserts a "Usage before
   Finish contract"; core states no such rule. That misreading is what made
   inline `Finish` look correct. Replace with the real invariant: `Finish` is
   terminal and emitted at end-of-stream; `Usage` flows through as it arrives.

The `Some(Err(_))` and cancellation arms deliberately do **not** call
`finish()`. An errored stream has no clean terminal event, and the contract
requires cancellation to end the stream *without* `Finish`
(`core/src/model.rs:65-67`).

### Exit-path semantics

| Exit | Emitted tail | Rationale |
| --- | --- | --- |
| Normal, usage present | `…Usage, Finish` | contract satisfied |
| Usage chunk absent | `…Finish` | terminal event still emitted |
| Truncated, no `finish_reason` | `…` (no `Finish`) | not a clean stop |
| Transport error | `…Err(_)` | error is terminal |
| Cancelled | `…` (nothing) | mandated by the contract |

### Fixtures

`tests/fixtures/chat_text_usage_trailing.txt` — **new**, transcribed verbatim
from the capture in Appendix A.1. This is the provenance anchor and the
regression test's target.

`chat_parallel_tool_calls.txt` and `chat_content_filter.txt` — restructured so
`usage` sits on its own trailing chunk matching the captured envelope (empty
`delta`, no `finish_reason` key), with the tool-call and content-filter
payloads otherwise unchanged.

Each fixture gets a leading comment recording provenance — what was captured
and what was hand-authored.

**Capture limits, established empirically.** A keyless LiteLLM mock cannot
provoke either non-happy path:

- `content_filter` — not reproducible on demand by any mock.
- `tool_calls` — `mock_tool_calls` is silently ignored on the proxy's streaming
  path; a request carrying `tools` with `tool_choice: "required"` still returns
  plain mock text (0 occurrences of `tool_calls` in the stream).

So exactly one fixture is genuinely captured, and the other two borrow its
envelope. The provenance comments must say so plainly rather than imply a
recording that did not happen.

**Line endings.** `.gitattributes` pins `text eol=lf` for the Anthropic fixture
directory and the tools fixtures, but **not** for
`providers-openai/tests/fixtures/`. That gap is not a live bug: the Anthropic
tests split fixtures on literal `\n`, whereas the OpenAI tests hand the bytes
to wiremock as `text/event-stream` and let a real SSE parser read them
(`tests/chat_streaming.rs:25-33`) — and SSE treats `\r\n` as a valid line
terminator. A CRLF checkout would therefore still parse, which is why Windows
CI is green today.

Add the rule anyway, for the reason the tools entry already gives ("pin LF for
consistency with the convention above"): these are wire-format fixtures whose
bytes are asserted on, and a future test that does split on `\n` would
otherwise fail only on Windows. Stated as consistency and defence, not as a
fix for a present failure.

### Tests

1. **Regression (the load-bearing one).** Against
   `chat_text_usage_trailing.txt`, assert `Finish` is the final event and that
   a `Usage` event precedes it.
2. **Mutation check.** Confirm the test genuinely **fails** against the
   pre-fix translator, not merely that it passes after. A test that cannot fail
   on the broken code is the precise failure mode that let this ship — the
   existing fixtures already demonstrate it. Verified by reverting the
   translator locally and observing a red test; recorded in the PR body.
3. **No-usage stream.** A stream ending `finish_reason` → EOF with no usage
   chunk still emits exactly one `Finish`.
4. **Truncated stream.** A stream ending with no `finish_reason` emits no
   `Finish`.
5. Existing tests over the restructured fixtures must continue to pass on their
   original assertions (finish reasons, tool-call assembly, usage values).

### Responses backend

Verified immune by inspection, no code change. `ResponseCompleted` and
`ResponseIncomplete` both route through `terminal_events()`
(`backend/responses.rs:441-483`), which builds `Usage` and `Finish` from a
single event's own data. They cannot be split across chunks, so the
cross-chunk failure mode does not exist there. Recorded here as the issue's
"should be checked" item, discharged with its reasoning.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

`cargo test --workspace --all-features` is the exact CI gate and must be run as
written, not per-crate.

## Out of scope

Filed as separate Linear issues:

1. **Bedrock truncation gap** — no `Metadata` after `MessageStop` yields no
   `Finish`. Includes the same misread "Usage must precede Finish" comment at
   `providers-bedrock/src/stream.rs:14`.
2. **Cross-provider conformance** — a shared assertion that every provider's
   stream ends with `Finish`, and that `Usage`, when present, precedes it.
   Would have caught this class across all five providers.

## Release

No manual version bump. `paigasus-helikon-providers-openai` is already
released, so release-plz bumps and publishes it automatically; a manual bump
would defeat the `dependencies_update` cascade and strand the facade.

No mdBook or README edit. `ChatTranslator` is `pub(crate)`; no public API,
feature flag, crate status, or documented concept changes. Recorded as a
conscious call per CLAUDE.md rather than a silent skip.

## Appendix A — captured traces

Environment: `ghcr.io/berriai/litellm:main-stable`, Docker 29.6.2, arm64 macOS,
`gpt-4o-mini` with `mock_response`, no API key.

### A.1 — `stream_options.include_usage: true`

Middle content deltas elided; the tail is verbatim.

```text
data: {"id":"chatcmpl-1cc5e8c0-…","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}
…
data: {"id":"chatcmpl-1cc5e8c0-…","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"."}}]}

data: {"id":"chatcmpl-1cc5e8c0-…","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"id":"chatcmpl-1cc5e8c0-…","created":1786898631,"model":"mock-fast","object":"chat.completion.chunk","choices":[{"index":0,"delta":{}}],"usage":{"completion_tokens":6,"prompt_tokens":8,"total_tokens":14,"completion_tokens_details":{"reasoning_tokens":0}}}

data: [DONE]
```

Note the key-order difference on the usage chunk (`created` before `object`) —
incidental, but preserved in the transcribed fixture since it is what the wire
produced.

### A.2 — `stream_options` omitted

```text
data: {…,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]
```

No trailing usage chunk. This is the case that rules out the Bedrock pattern.

### A.3 — tool calls not provokable

`mock-tools` configured with `mock_tool_calls`, requested with `tools` and
`tool_choice: "required"`: the stream returns plain mock text and contains zero
`tool_calls` occurrences.

## Reproduction

```yaml
model_list:
  - model_name: mock-fast
    litellm_params:
      model: openai/gpt-4o-mini
      api_key: fake-key
      mock_response: "Hello from the mock backend."
general_settings: {master_key: sk-probe-1234}
```

```bash
docker run -d --name litellm-probe -p 4000:4000 \
  -v "$PWD/litellm-config.yaml:/app/config.yaml" \
  ghcr.io/berriai/litellm:main-stable --config /app/config.yaml --port 4000

curl -sN http://localhost:4000/v1/chat/completions \
  -H 'Content-Type: application/json' -H 'Authorization: Bearer sk-probe-1234' \
  -d '{"model":"mock-fast","messages":[{"role":"user","content":"hi"}],
       "stream":true,"stream_options":{"include_usage":true}}'
```
