# SMA-531 — Anthropic EOF `Finish` flush

**Linear:** [SMA-531](https://linear.app/smaschek/issue/SMA-531/anthropic-provider-emits-no-finish-when-a-stream-truncates-between)
**Date:** 2026-08-16
**Crate:** `paigasus-helikon-providers-anthropic`

## Problem

`MessageTranslator` buffers `stop_reason` from the `message_delta` SSE event and
emits `ModelEvent::Finish` only when `message_stop` arrives
(`crates/paigasus-helikon-providers-anthropic/src/stream.rs:161-169`). The driver
that pumps the SSE stream ends on a bare `None => return`
(`crates/paigasus-helikon-providers-anthropic/src/model.rs:113`) with no
end-of-stream flush.

A stream truncated between `message_delta` and `message_stop` therefore emits
`Usage` and **no** `Finish` at all. A consumer waiting for the terminal event
never sees one.

The crate's only `fn finish*` is `finish_or_error` — a reason-mapping helper
(`stream.rs:178`) that the driver never calls.

### Why Anthropic is the last outlier

Gemini (`providers-gemini/src/stream.rs:73`, flushed from the driver's `[DONE]`
and EOF arms), Bedrock (`providers-bedrock/src/stream.rs:262`, flushed from
`model.rs:107-112`), and OpenAI (as of SMA-522 / PR #197) all flush a buffered
stop reason at end-of-stream. Anthropic is the only provider left without one.

### What the core contract actually says

`paigasus_helikon_core::Model::invoke` (`crates/paigasus-helikon-core/src/model.rs:55-67`)
constrains **ordering**, never **emission**:

> - `Usage` MAY appear anywhere […]
> - `Finish` is the terminal event; nothing follows it.
>
> Implementations that cannot honor cancellation MUST still terminate the stream
> when the `CancellationToken` fires (drop the underlying connection and end the
> stream without emitting `Finish`).

So the emission guarantee this ticket restores is a **de-facto convention** held
by three of four providers, not a written mandate — which is plausibly why four
providers derived it independently and two got it wrong. Codifying the wording is
deferred to SMA-533 (see [Scope boundaries](#scope-boundaries)). The cancellation
clause above *is* written, and directly backs the cancellation acceptance
criterion.

## Design

### Flush site: keep `message_stop`, add an EOF flush

Anthropic differs from OpenAI in a way that matters here. OpenAI *had* to defer
emission to EOF because `usage` arrives on a chunk **after** the one carrying
`finish_reason` — emitting inline put a `Usage` after the terminal event. Anthropic
has no such problem: `message_delta` carries `stop_reason` and `usage` together,
and `message_stop` is an explicit terminal event with nothing after it.

Bedrock is the true structural analogue (both buffer a reason, both emit inline on
a normal terminal event, both have a reason-mapper that can return `Err`), so this
follows Bedrock: **`message_stop` keeps emitting inline; `finish()` is added purely
as an EOF flush.**

Considered and rejected: moving emission entirely to EOF (the OpenAI/Gemini shape,
one flush site). It would widen two windows — a transport error or a cancellation
arriving in the `message_stop`→EOF gap would suppress a `Finish` that today is
already emitted. The two-site shape costs nothing in exchange, because of the
ownership invariant below.

### The ownership invariant

`stop_reason: Option<String>` has exactly one consumer per stream. Either
`message_stop` takes it, or `finish()` takes it — `Option::take` makes
double-emission **structurally impossible**, not merely conventionally avoided.

Consequence: on every well-formed stream `message_stop` has already drained the
buffer, so `finish()` returns `None` and is a no-op. All five existing fixtures
retain their current behaviour byte-for-byte; only the truncation case changes,
and it currently emits nothing.

### `stream.rs` — new method

```rust
/// Flush a stop reason buffered from `message_delta` when the stream ends
/// (EOF) before `message_stop` arrived.
///
/// Returns `None` when `message_stop` already drained the buffer (the
/// well-formed path) or when no stop reason was ever observed (truncation
/// mid-generation) — a truncated stream is never reported as a clean `Stop`.
///
/// **EOF path only.** Never call this on the cancellation or error paths.
pub(crate) fn finish(&mut self) -> Option<Result<ModelEvent, ModelError>> {
    self.stop_reason.take().map(|r| self.finish_or_error(&r))
}
```

`AnthropicEvent::MessageStop`'s arm is unchanged.

### `model.rs` — driver

```rust
None => {
    if let Some(terminal) = translator.finish() {
        yield terminal;
    }
    return;
}
```

### Return shape

`Option<Result<ModelEvent, ModelError>>` — identical to Bedrock's `finish()`.
SMA-533 records that the shape already differs three ways across providers
(Gemini `Vec<Result<…>>`, Bedrock `Option<Result<…>>`, OpenAI `Vec<ModelEvent>`)
and asks to settle it. Matching Bedrock means this change adds **no fourth
variant**. `Option` is also the honest shape: `finish_or_error` returns exactly
one terminal event or one error, never zero and never many.

### Error semantics at EOF

`finish_or_error` returns `Err` in two cases:

| `stop_reason` | Condition | Result |
| --- | --- | --- |
| `"refusal"` | always | `Err(ModelError::Refused)` |
| `"tool_use"` | `synthesizing_output && other_tool_fired` | `Err(ModelError::Other)` |

**The EOF flush yields these errors**, matching Bedrock's `finish()`. A stream
truncated after a refusal surfaces `Refused` rather than silence — silence is the
very failure mode this ticket exists to remove.

This reads the acceptance criterion *"emits exactly one `Finish`"* as *"emits
exactly one terminal event"*, of which `Err` is the error-shaped form. Recorded
explicitly because the literal wording admits a narrower reading.

### Paths that must not flush

Three exit paths `return` without calling `finish()`, discarding any buffered
reason. All three are deliberate:

| Path | Location | Rationale |
| --- | --- | --- |
| Cancellation | `model.rs:109` | Mandated by `core/src/model.rs:65-67` |
| Transport error | `model.rs` `Some(Err(e))` arm | A stream that failed did not finish |
| In-band `error` event | `translator.consume` → `Err` | Same; `Err` is already terminal |

## Testing

Tests are labelled by **what each can actually catch**, following the SMA-522
precedent. The acceptance criteria demand a regression test *verified failing
against current code* — "a test that cannot fail on broken code is what let the
OpenAI bug ship."

### Red-first regression tests (integration)

These compile against the current code and fail. They go in
`tests/messages_streaming.rs`.

| Test | Fixture | What it catches |
| --- | --- | --- |
| `truncated_after_message_delta_emits_finish` | new — `text_only.txt` minus `message_stop` | **The bug.** Currently no `Finish` is emitted at all. This is the AC's verified-failing test |
| `truncated_mid_content_block_emits_no_finish` | new — cut after a `content_block_delta` | The fix over-firing. Passes today; its mutation-check is "make `finish()` return `Finish{Stop}` unconditionally" |
| `error_after_buffered_stop_reason_emits_no_finish` | new — `message_delta` with `stop_reason`, then an in-band `error` event | A fix that flushes on the error arm. The existing `stream_error.txt` test **cannot** catch this: its stream never buffers a reason, so the discard is untested there |

Each asserts on the **whole event sequence**, not just the last element — an
assertion on `last()` alone cannot distinguish "no `Finish`" from "no events".

### Invariant tests (unit, in `stream.rs`)

These cannot be red-first: they call a method that does not yet exist, so they
fail to compile rather than fail. They pin the invariants instead.

- `finish_flushes_pending_stop_reason` — `message_delta` with a stop reason, no
  `message_stop`, then `finish()` → `Some(Ok(Finish{Stop}))`.
- `finish_is_none_when_no_stop_reason_observed` → `None`.
- `finish_is_none_after_message_stop_drained_it` — the anti-double-emit test;
  pins the ownership invariant directly.
- `finish_surfaces_refusal_as_error` — `stop_reason: "refusal"`, truncated →
  `Some(Err(ModelError::Refused))`.

### Cancellation tests (new file)

`tests/cancellation.rs` — the Anthropic crate has **zero** cancellation coverage
today, so the AC's cancellation clause is currently unpinned by any test. Modelled
on `providers-openai/tests/cancellation.rs`.

- `cancellation_mid_stream_emits_no_finish`
- `uncancelled_stream_emits_exactly_one_finish` — **not optional.** Without this
  control, the test above passes against a build that emits no events at all; the
  pair is what makes the cancellation assertion meaningful rather than vacuous.

### Fixtures

Three new files, each derived from `text_only.txt` so the truncation cases are
genuine prefixes of an already-verified stream shape. `.gitattributes` already
pins `crates/paigasus-helikon-providers-anthropic/tests/fixtures/*.txt` to
`text eol=lf`, so new fixtures inherit LF — no `.gitattributes` change needed.

## Scope boundaries

Deferred, with owners:

- **Core contract wording** (stating the emission guarantee, not just ordering) →
  **SMA-533**, appended during this ticket's design phase. It lands alongside the
  conformance suite that enforces it, so prose and test cannot drift — the drift
  SMA-532 documents in the Bedrock comments.
- **Unifying the three `finish()` return shapes** → **SMA-533**.
- **Bedrock doc comments misstating the ordering contract** → **SMA-532**.

Conscious no-ops (per CLAUDE.md these must be deliberate calls, not silent skips):

- **No mdBook edit.** No public API, quickstart/example flow, crate-roster, or
  documented-concept change. `finish()` is `pub(crate)`. The book's only mention
  of `Finish` (`docs/book/src/concepts/model-providers.md:57`) describes the
  `ModelEvent` union, which is unchanged.
- **No crate README edit.** The crate's public surface, install story, feature
  flags, and published status are all unchanged.

## Release impact

`paigasus-helikon-providers-anthropic` takes a patch bump through release-plz's
normal flow (an already-released crate; no stub-ascend ritual). No `-core` bump,
therefore no facade cascade and none of the same-PR manual-bump caveats in
CLAUDE.md apply.

## Acceptance criteria → coverage

| Criterion | Covered by |
| --- | --- |
| Truncation after an observed stop reason emits exactly one `Finish` | `truncated_after_message_delta_emits_finish`, `finish_flushes_pending_stop_reason` |
| Truncation with no stop reason observed emits no `Finish` | `truncated_mid_content_block_emits_no_finish`, `finish_is_none_when_no_stop_reason_observed` |
| Cancellation emits no `Finish` | `cancellation_mid_stream_emits_no_finish` + its control |
| Mid-stream error emits no `Finish` | `error_after_buffered_stop_reason_emits_no_finish` |
| Regression test verified failing against current code | `truncated_after_message_delta_emits_finish`, run red before the fix |
