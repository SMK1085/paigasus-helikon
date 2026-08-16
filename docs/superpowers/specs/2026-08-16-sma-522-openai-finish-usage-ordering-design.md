# SMA-522 — OpenAI `Finish`/`Usage` ordering

**Date:** 2026-08-16
**Issue:** [SMA-522](https://linear.app/smaschek/issue/SMA-522/openai-provider-emits-finish-before-usage-violating-the-core-event)
**Crate:** `paigasus-helikon-providers-openai`
**Status:** revised after adversarial review — awaiting approval

**Review history.** A first draft was attacked by an independent reviewer that
cross-checked every load-bearing claim against the code. It refuted four of
them: the Bedrock comparison that justified the design choice, the stated
SMA-402 impact, the "multi-choice preserved exactly" equivalence, and the
Bedrock follow-up ticket. All four are corrected in place below, with the
original claim named rather than quietly deleted — a spec whose rationale was
wrong should show that, because the corrections are the most useful thing in
it. The chosen implementation survived unchanged; only its justification and
scope did not.

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
licenses — drops the turn's only usage snapshot.

### Impact, stated accurately

The issue claims SMA-402's cross-turn token summing "silently under-counts
every OpenAI turn". **That is not true of any consumer in this repository**,
and the spec should not repeat it. Every in-repo consumer drains the stream to
`None` before reading usage:

- `crates/paigasus-helikon-core/src/agent.rs:941` — `while let Some(evt) =
  model_stream.next().await { … acc.observe(&ev); … }`, then `acc.finish()`
- `crates/paigasus-helikon-runtime-temporal/src/activities.rs:98`
- `crates/paigasus-helikon-runtime-tokio/src/retry.rs:222-232` (verbatim
  forward)

So the trailing `Usage` *is* observed today and nothing under-counts. This is a
**contract-conformance fix for external consumers**, plus a genuine secondary
repair (duplicate `Finish` emission — see "Multi-choice" below).

The "stops reading at `Finish`" pattern is nonetheless real, not hypothetical,
and there is an in-repo witness: `crates/paigasus-helikon-core/src/compacting_session.rs:237`
does `ModelEvent::Finish { .. } => break`. It happens to ignore `Usage`
(`_ => {}`), so it is unaffected today — but it is the honest evidence that a
contract-abiding consumer written tomorrow would lose the snapshot.

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
   the stream ends `finish_reason` chunk → `[DONE]`. This case is why the
   terminal event must be anchored to EOF.

**Provenance limit, stated up front.** This was confirmed against LiteLLM's
mock backend, **not** against `api.openai.com` — no first-party key was
available. The issue and this spec both generalise to "OpenAI"; the direct
evidence is one proxy. Two things keep that from undermining the work: the
trailing-usage split is what the OpenAI Chat Completions streaming spec
describes for `include_usage`, and the fix is strictly more permissive — it is
correct whether usage arrives on the finish chunk, on a trailing chunk, or
never. Confirming against the first-party API remains worthwhile and is noted
in the follow-ups.

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

### Multi-choice: a behaviour change, not a preserved invariant

An earlier draft claimed last-wins is "preserved exactly". It is not, and the
difference is worth stating because it is an unclaimed *benefit*.

`finish_event` today is a **per-`consume` local** (`chat.rs:244`, pushed at
`:298`), so the current code emits one `Finish` **per chunk carrying any
`finish_reason`**. Moving the stash to a struct field collapses that to exactly
one `Finish` at EOF. Concretely, for chunks

```text
A: choices:[{index:0, delta:{},            finish_reason:"stop"}]
B: choices:[{index:1, delta:{content:"x"}}]
C: choices:[{index:1, delta:{},            finish_reason:"length"}]
```

old emits `Finish(Stop), TokenDelta, Finish(Length)`; new emits
`TokenDelta, Finish(Length)`. The old sequence puts a `TokenDelta` *after* a
`Finish` — itself a contract violation. The same divergence appears in the far
likelier single-choice case where a proxy repeats `finish_reason` on the usage
chunk (LiteLLM does not, per Appendix A.1 — but that is one proxy).

New semantics, stated plainly: **exactly one `Finish` per stream; the last
`finish_reason` observed anywhere in the stream wins.** Pinned by a unit test.

`ModelSettings` exposes no `n` field and `build_request` never sets one, so
`n > 1` can only arrive from a nonstandard proxy. The translator emits a
`tracing::debug!` when it observes a *second, distinct* `finish_reason`, so
genuinely ambiguous multi-choice is visible in a trace rather than silently
resolved.

### Rejected: pair `[Usage, Finish]` on the usage event (the Bedrock pattern)

**Correction to an earlier draft of this spec.** It asserted that Bedrock emits
no `Finish` when the trailing `Metadata` event never arrives, and used that as
the reason to reject its pattern. **That is false.** Bedrock already has the
same EOF flush proposed here: `providers-bedrock/src/stream.rs:262`
(`pub(crate) fn finish(&mut self) -> Option<Result<ModelEvent, ModelError>>`,
documented as flushing "when the Bedrock stream ends normally (EOF) without
having emitted a `Metadata` event"), driven from
`providers-bedrock/src/model.rs:107-112`, and covered by
`finish_flushes_pending_stop_reason_without_metadata` (`stream.rs:768`).
Bedrock is a **hybrid** — pair-on-`Metadata` *plus* EOF flush — and is not
vulnerable to the failure the earlier table attributed to it.

The real reason to prefer the Gemini shape for OpenAI is **simplicity**: one
emission site and one piece of state, versus Bedrock's `pending_stop_reason` /
`metadata_seen` interaction, which exists because Bedrock must also handle
`Metadata` arriving *before* `MessageStop`. The OpenAI wire has no such
reordering, so that machinery would be dead weight.

Appendix A.2 (no usage chunk at all) still matters — it is why the terminal
event must be anchored to EOF rather than to the usage chunk — but it
discriminates against a *naive* pair-on-usage design, not against Bedrock as
actually implemented.

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

5. Emit a `tracing::debug!(target: "paigasus::openai::chat", …)` when `finish()`
   returns empty. That is the one silent exit producing a turn with no terminal
   event; a trace should distinguish truncation from a clean stop.

6. Correct the doc comment at `chat.rs:237-241`. It asserts a "Usage before
   Finish contract"; core states no such rule. That misreading is what made
   inline `Finish` look correct. Replace with the real invariant: `Finish` is
   terminal and emitted at end-of-stream; `Usage` flows through as it arrives.

7. Correct the **byte-identical misreading** at `backend/responses.rs:274-278`,
   which repeats "Event ordering follows the 'Usage before Finish' contract".
   Same crate, same PR, one line — leaving the root-cause misstatement in place
   next to its own fix makes no sense. (The same text at
   `providers-bedrock/src/stream.rs:14` and `:439` is deferred to the
   follow-up.)

8. Sweep the stale "async-openai 0.40" references at `chat.rs:120` and
   `chat.rs:179`; `Cargo.lock` pins 0.41.3. Free, since item 6 already edits
   comments in this file.

### The error arm discards a buffered `Finish` — a deliberate behaviour change

The `Some(Err(_))` and cancellation arms do **not** call `finish()`. For
cancellation this is mandated (`core/src/model.rs:65-67`). For the error arm it
is a **change from today's behaviour** and must be recorded as one: currently,
a stream that errors *after* the finish chunk has already delivered `Finish`
inline, so the consumer sees `Finish, Err`; afterwards it sees `Err` alone.

This is not hypothetical for the proxy audience motivating this work.
`CreateChatCompletionStreamResponse` declares `id`, `choices`, `created`,
`model`, and `object` as non-`Option`, so a proxy omitting `object` on its
trailing usage chunk yields a mid-stream deserialization error — exactly the
shape where the buffered `Finish` is now dropped.

**Decision: discard.** Yielding `Finish` and then `Err` would put an item after
the terminal event, which is the very thing this ticket exists to stop; and a
stream that errored did not complete cleanly, so reporting a clean terminal
reason would be a lie. Pinned by a test with a malformed trailing chunk.

### Exit-path semantics

Δ marks a change from current behaviour.

| Exit | Emitted tail | Δ | Rationale |
| --- | --- | --- | --- |
| Normal, usage present | `…Usage, Finish` | Δ (was `…Finish, Usage`) | the fix |
| Usage chunk absent | `…Finish` | — | terminal event still emitted |
| Truncated, no `finish_reason` | `…` (no `Finish`) | — | not a clean stop |
| Transport / parse error after finish chunk | `…Err(_)` | Δ (was `…Finish, Err`) | see above |
| Cancelled after finish chunk | `…` (nothing) | Δ (was `…Finish`) | contract mandates no `Finish` |
| Multi-`finish_reason` stream | one `Finish`, last wins | Δ (was one per chunk) | removes post-`Finish` deltas |

### Fixtures

`tests/fixtures/chat_text_usage_trailing.txt` — **new**. The finish chunk,
usage chunk, and `[DONE]` are transcribed byte-for-byte from Appendix A.1; the
ten content deltas are reconstructed by hand in the same shape (the appendix
elides them). The provenance comment must say exactly that — "transcribed
verbatim" would overclaim.

`chat_parallel_tool_calls.txt` and `chat_content_filter.txt` — restructured so
`usage` sits on its own trailing chunk matching the captured envelope (empty
`delta`, no `finish_reason` key), with the tool-call and content-filter
payloads otherwise unchanged.

`tests/chat_wire.rs:34-41` — **the third instance, and the one that states the
falsehood in English.** It builds an inline SSE body welding `usage` onto the
`finish_reason` chunk, under the comment *"Usage arrives on the same chunk as
`finish_reason` (per OpenAI's `stream_options.include_usage: true`
behaviour)"* — precisely what Appendix A refutes. Restructure the body to the
captured envelope and delete the comment. A spec arguing "the fixtures are the
entire regression signal" cannot leave the written-down false belief in place.

**Provenance comments are SSE comment lines.** These files are fed to a real
SSE parser via wiremock, so each provenance note must be a `:`-prefixed line.
A `#` or `//` line would parse as an unknown field — harmless but silently
wrong, and a bad precedent for the Anthropic fixtures, whose tests do split on
literal `\n`.

**Envelope caveat.** The capture is LiteLLM's mock, not `api.openai.com`. Its
usage chunk carries `"choices":[{"index":0,"delta":{}}]`; OpenAI's own API
sends `"choices":[]` (async-openai documents the field as *"Can also be empty
for the last chunk if you set `stream_options: {"include_usage": true}`"*).
Both parse identically — the choices loop simply does not run — but the anchor
fixture must not silently claim first-party provenance. Record the limitation
in its comment and add a second hand-authored variant with `"choices":[]` so
both envelopes are covered.

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

Tests are labelled by what they can actually catch. Only the first is a
regression test in the strict sense; conflating the two categories is how the
original blind spot was rationalised.

**Regression — fails on pre-fix code:**

1. Against `chat_text_usage_trailing.txt`: assert `Finish` is the final event,
   that a `Usage` precedes it, that there is **exactly one** `Finish`, and that
   the usage carries the real captured counts (`input_tokens: 8`,
   `output_tokens: 6`). The count assertions matter — without them a translator
   emitting a zeroed `Usage` would pass.
2. **Multi-choice / repeated `finish_reason`:** a `ChatTranslator` unit test
   asserting exactly one `Finish` with last-wins, using the A/B/C chunk
   sequence above. Fails pre-fix, which emits two.

**New-code guards — pass both before and after; they protect the *fix* from
regressing, and are not evidence the bug existed:**

3. No-usage stream (`finish_reason` → EOF) still emits exactly one `Finish`.
4. Truncated stream (no `finish_reason`) emits no `Finish`.
5. Cancellation fired **after** the finish chunk emits no `Finish`. The
   existing `tests/cancellation.rs:22` cancels before the first chunk and does
   not cover this race.
6. Malformed trailing chunk (mid-stream parse error after the finish chunk)
   emits `Err` and no `Finish`, pinning the discard decision above.

**Mutation check.** Tests 1 and 2 must be observed **failing** against the
pre-fix translator, not merely passing after it. Verified by reverting the
translator locally and recording the red output in the PR body. This is the
whole lesson of the ticket: the existing fixtures demonstrate that a test which
cannot fail on broken code is worse than no test, because it reads as coverage.

7. Existing tests over the restructured fixtures must continue to pass on their
   original assertions (finish reasons, tool-call assembly, usage values).

Unit tests 2-4 belong beside the existing `ChatTranslator` tests at
`chat.rs:378-541`, where they need no wiremock; only the fixture-driven and
cancellation cases need the integration harness.

### Responses backend

Verified immune by inspection, no code change. `ResponseCompleted` and
`ResponseIncomplete` both route through `terminal_events()`
(`backend/responses.rs:441-483`), which builds `Usage` and `Finish` from a
single event's own data. They cannot be split across chunks.

State the invariant in its durable form rather than the incidental one:
**`Usage` is constructed only inside `terminal_events`, which unconditionally
appends `Finish` before returning** (`responses.rs:455-462`, `:481`). "They
happen to arrive on the same event" is true but fragile; the above is what a
future arm emitting partial usage would have to break. Record it as a comment
on `terminal_events` so the guarantee is stated where it can be violated.

`ResponseFailed` and `ResponseError` yield `Err` with no `Finish` at all
(`responses.rs:408-426`). This is consistent with the contract, which mandates
that `Finish` be *terminal*, never that it be *mandatory* — the same reasoning
as the Chat error arm above. Stated explicitly because "is there a path with no
terminal event?" is precisely what the issue asked to discharge.

Its misread doc comment at `responses.rs:274-278` is fixed here (change 7);
that is the only edit this backend receives.

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

1. **Anthropic truncation gap** (retargeted — an earlier draft wrongly named
   Bedrock). `providers-anthropic/src/stream.rs:161-169` buffers `stop_reason`
   from `message_delta` and emits `Finish` only on `message_stop`, and its
   driver at `providers-anthropic/src/model.rs:113` is a bare `None => return`
   with no EOF flush — its only `fn finish*` is `finish_or_error`, a mapping
   helper that is never called from the driver. A stream truncated between
   `message_delta` and `message_stop` therefore emits `Usage` and no `Finish`,
   where Gemini, Bedrock, and post-fix OpenAI all emit one. Anthropic is the
   sole remaining provider with this gap.
2. **Misread contract comments in Bedrock** — `providers-bedrock/src/stream.rs:14`
   and `:439` repeat the "Usage must precede Finish" misstatement. The code is
   correct; only the prose is wrong.
3. **Cross-provider conformance** — a shared assertion across all five
   providers that a stream is `Finish`-terminated, that `Usage` (when present)
   precedes it, and specifically that **EOF after an observed stop reason emits
   `Finish`** — the last clause being the one that catches item 1. The
   follow-up should also settle the `finish()` return shape, which currently
   differs three ways: `Vec<Result<ModelEvent, ModelError>>` (Gemini),
   `Option<Result<ModelEvent, ModelError>>` (Bedrock), and `Vec<ModelEvent>`
   (proposed here, matching this crate's infallible `consume`).

Deliberately **not** taken on here, but worth recording as a real question: the
Gemini driver logs and `continue`s on an unparseable chunk
(`providers-gemini/src/model.rs:159-169`) whereas OpenAI terminates the stream.
Given that proxy compatibility is this design's stated motivation, that
asymmetry deserves its own decision rather than an incidental one.

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
