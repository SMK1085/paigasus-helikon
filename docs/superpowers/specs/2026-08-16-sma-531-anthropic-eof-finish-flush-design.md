# SMA-531 — Anthropic clean-EOF `Finish` flush

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

A stream whose HTTP body **ends cleanly** after `message_delta` but before
`message_stop` therefore emits `Usage` and **no** `Finish` at all. A consumer
waiting for the terminal event never sees one.

The crate's only `fn finish*` is `finish_or_error` — a reason-mapping helper
(`stream.rs:178`) that the driver never calls.

### Provenance

This defect was found by **code reading** during SMA-522 (OpenAI `Finish`/`Usage`
ordering), not from a captured trace or a user report. There is no packet-level
evidence of the failure firing in production. That matters for scoping — see
[What "truncation" means here](#what-truncation-means-here).

### Why Anthropic is the outlier

Anthropic is the last translator that **buffers a stop reason without a
clean-EOF flush**. Gemini (`providers-gemini/src/stream.rs:73`, flushed from the
driver's `[DONE]` and EOF arms), Bedrock (`providers-bedrock/src/stream.rs:262`,
flushed from `model.rs:107-112`), and OpenAI's Chat backend (as of SMA-522 /
PR #197) all have one.

Stated precisely because a reader who checks will find that OpenAI's *Responses*
backend also has a bare `None => return` (`src/backend/responses.rs:70`). That is
**not** the same bug: its translator never buffers a reason, emitting `Usage` and
`Finish` together from a single terminal event (`responses.rs:441-453`).

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
by three of four buffering translators, not a written mandate — plausibly why
four providers derived it independently and two got it wrong. Codifying the
wording is deferred to SMA-533 (see [Scope boundaries](#scope-boundaries)). The
cancellation clause above *is* written, and directly backs the cancellation
acceptance criterion.

## What "truncation" means here

The word is ambiguous, and the ambiguity is load-bearing. A stream can stop early
in two ways that surface at completely different points in the driver:

| Form | How it reaches the driver | Covered by this fix? |
| --- | --- | --- |
| **Clean short body** — server sends the chunked terminator / satisfies `Content-Length`, then FIN, after `message_delta` but before `message_stop` | `event_stream.next()` → `None`, i.e. `model.rs:113` | **Yes.** This is the fix. |
| **Dirty cut** — connection reset, proxy idle-timeout, chunked stream cut without its `0\r\n\r\n` terminator | `event_stream.next()` → `Some(Err(e))`, i.e. `model.rs:114-117` → `yield Err(ModelError::Transport(..))` | **No — deliberately.** |

**This ticket scopes to the clean-body form only.** The dirty cut keeps yielding
`Err(Transport)` with any buffered reason discarded, because:

1. All three prior-art providers do exactly this. SMA-522 / PR #197 added a test
   specifically to *pin* it ("a buffered `Finish` is discarded on both the
   stream-error and cancellation exit paths").
2. **SMA-533's acceptance clause 6 mandates it**: "A mid-stream error emits no
   `Finish`." Flushing on the transport-error arm would put this crate in direct
   violation of a filed conformance criterion.
3. `Err` is already a terminal signal; a stream that failed did not finish.

**Residual gap, stated plainly:** if the failure mode that motivated SMA-531 turns
out to be a dirty cut rather than a clean short body, this fix does not address it
and the ticket should be reopened with a captured trace. No test in this plan can
tell the two apart — `tests/messages_streaming.rs:1-4` records that wiremock
serves each fixture as one complete, well-formed HTTP response, so **every**
fixture exercises the clean-EOF path by construction.

## Design

### Flush site: keep `message_stop`, add a clean-EOF flush

Anthropic differs from OpenAI in a way that matters. OpenAI *had* to defer
emission to EOF because `usage` arrives on a chunk **after** the one carrying
`finish_reason` — emitting inline put a `Usage` after the terminal event.
Anthropic has no such problem: `message_delta` carries `stop_reason` and `usage`
together, and `message_stop` is an explicit terminal event with nothing after it.

Bedrock is the true structural analogue (both buffer a reason, both emit inline on
a normal terminal event, both have a reason-mapper that can return `Err`), so this
follows Bedrock: **`message_stop` keeps emitting inline; `finish()` is added as a
clean-EOF flush.**

Considered and rejected: moving emission entirely to EOF (the OpenAI/Gemini shape,
one flush site). It would widen two windows — a transport error or a cancellation
arriving in the `message_stop`→EOF gap would suppress a `Finish` that today is
already emitted. The `biased` `tokio::select!` at `model.rs:107` polls
cancellation *before* the stream, so that gap is a real scheduling window, not
merely the microseconds of wire time between `message_stop` and FIN.

**Adversarial-review counter, recorded:** once the `terminal_emitted` guard below
exists, both shapes are equally safe against double-emission, so the two-site
design's remaining advantage is narrower than first argued — it preserves today's
exact behaviour and nothing more. The two-site shape was approved with this
tradeoff explicit; noting the counter so it can be revisited cheaply.

### The ownership invariant — stated correctly

An earlier draft of this spec claimed `Option::take` makes double-emission
"structurally impossible". **That was false**, and the fix as originally drafted
would have introduced a new contract violation:

```text
message_delta{stop_reason:"end_turn"}   → buffer := Some("end_turn")
message_stop                            → take() → emits Finish{Stop}
message_delta{stop_reason:"max_tokens"} → buffer := Some("max_tokens")   ← re-armed
EOF                                     → finish() → emits Finish{Length}   ← SECOND terminal
```

`take()` prevents re-emitting the *same* buffered value; it does nothing about a
*replacement* value. Today the second reason is silently discarded because no
flush exists — adding one without a guard turns a silent discard into a violation
of `core/src/model.rs:63` on exactly the malformed streams this ticket is about.

**Fix: a `terminal_emitted: bool` field**, set by both emission sites and checked
by both. The honest invariant is then: *at most one terminal event per stream,
enforced by an explicit flag; the buffered reason is additionally single-use via
`take()`.*

The flag is checked in the `MessageStop` arm too, which also closes a
**pre-existing** latent bug independent of this fix — today
`message_delta`/`message_stop`/`message_delta`/`message_stop` emits two `Finish`
events inline. Small, adjacent, and clearly correct; called out here so it can be
vetoed as scope creep if preferred.

### `stream.rs` — new field and method

Add to `MessageTranslator`:

```rust
/// Set once a terminal event has been emitted, by either emission site.
/// Guards against a second `message_delta` re-arming `stop_reason` after
/// `message_stop` already emitted — see the ownership invariant above.
terminal_emitted: bool,
```

The `MessageStop` arm becomes:

```rust
AnthropicEvent::MessageStop => {
    if let Some(reason) = self.stop_reason.take() {
        if !self.terminal_emitted {
            self.terminal_emitted = true;
            out.push(self.finish_or_error(&reason));
        }
    }
}
```

New method:

```rust
/// Flush a stop reason buffered from `message_delta` when the response body
/// ends cleanly before `message_stop` arrived.
///
/// Returns `None` when a terminal event was already emitted (the well-formed
/// path, where `message_stop` drained the buffer) or when no stop reason was
/// ever observed. A stream that ended *before* `message_delta` is never
/// reported as a clean `Stop`; one that ended *after* it is, because
/// `message_delta.stop_reason` is the model's own authoritative decision and
/// `message_stop` is only a frame terminator.
///
/// **Clean-EOF path only.** Never call this on the cancellation or
/// transport-error paths.
pub(crate) fn finish(&mut self) -> Option<Result<ModelEvent, ModelError>> {
    if self.terminal_emitted {
        return None;
    }
    let reason = self.stop_reason.take()?;
    self.terminal_emitted = true;
    tracing::warn!(
        target: "paigasus::anthropic::stream",
        stop_reason = %reason,
        "stream body ended without message_stop; flushing buffered stop reason",
    );
    Some(self.finish_or_error(&reason))
}
```

The `tracing::warn!` is not decoration. Without it a flushed terminal is
indistinguishable from a normal one downstream, so truncation rate stays
**unmeasurable in production** — for a bug whose entire symptom was invisibility.
The crate already logs at comparable seams (`model.rs:125-131` for unparseable
payloads, `stream.rs:111-114` for dropped signature deltas), and OpenAI logs on
the sibling branch (`backend/chat.rs:330-334`). `warn` rather than `debug`
because, unlike OpenAI's no-reason-observed case, this one means a real stream
was cut short.

Decision: the flushed `Finish` is **not** distinguishable at the `ModelEvent`
level — no `FinishReason::Other("truncated:…")`. The reason the model gave is the
reason the consumer should see; the log carries the operational signal.

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

Borrow-checker note: `self.stop_reason.take()` ends its `&mut` borrow when it
returns an owned value, so the later `&self` call to `finish_or_error` is fine.
Proven in-tree — Bedrock ships the identical shape at
`providers-bedrock/src/stream.rs:262-266`.

### Error semantics at clean EOF

`finish_or_error` (`stream.rs:186-203`) returns `Err` in two cases:

| `stop_reason` | Condition | Result |
| --- | --- | --- |
| `"refusal"` | always | `Err(ModelError::Refused { reason })` |
| `"tool_use"` | `synthesizing_output && other_tool_fired` | `Err(ModelError::Other)` |

Note `Refused` is a **struct** variant (`core/src/model.rs:419-422`), so test
patterns must be `Some(Err(ModelError::Refused { .. }))`.

**The clean-EOF flush yields these errors**, matching Bedrock's `finish()`. A
stream cut after a refusal surfaces `Refused` rather than silence — silence is the
failure mode this ticket exists to remove.

This reads the acceptance criterion *"emits exactly one `Finish`"* as *"emits
exactly one terminal event"*, of which `Err` is the error-shaped form. Recorded
explicitly because the literal wording admits a narrower reading.

### Behaviour change: truncated-refusal runs move from success to `RunFailed`

The `Err` half of this change is **not** behaviour-neutral, and this must be in
the PR description.

- **Today:** a cleanly-truncated refusal emits no `Finish`, so
  `ModelTurnAccumulator`'s default `finish_reason: FinishReason::Stop`
  (`core/src/model.rs:558`) applies and the run **completes successfully** with
  the partial text.
- **After:** `agent.rs:967-974` turns any stream `Err` into
  `failure.set(AgentError::Model(e))` + `yield AgentEvent::RunFailed` + `return`.
  The same stream now **hard-fails the run**.

Defensible — it matches the untruncated refusal path — but operators taking a
patch bump will see new `RunFailed`s. It applies to both `Err` rows above.

`RetryingModel` will **not** rescue these: `runtime-tokio/src/retry.rs:217-233`
retries only when the **first** stream item is an `Err`, and Anthropic always
emits `Usage` from `message_start` first.

Counterweight, verified: the `Ok(Finish{..})` half is behaviourally near-inert.
`finish_reason` is not branched on — `loop_state.rs:280` and `:355` both match
`ModelResponse { items, usage, .. }` and decide on the *presence of
`Item::ToolCall`*; `runtime-temporal/src/driver.rs:289-301` merely passes it
through. So the observable win is the terminal event's *existence*, not its
reason.

### Every exit path, enumerated

The `stream!` body in `model.rs` has seven exits. The earlier draft claimed three
and got the `consume → Err` row wrong; an under-inclusive list invites a future
refactor (e.g. hoisting translator construction) to silently create a leak.

**Before the translator exists** (constructed at `model.rs:104`) — nothing to flush:

| Line | Exit |
| --- | --- |
| `:60` | Cancellation before the response arrives |
| `:65` | Request send error |
| `:100` | Non-success HTTP status |

**After the translator exists:**

| Line | Exit | Flush? |
| --- | --- | --- |
| `:109` | Mid-loop cancellation | **No** — mandated by `core/src/model.rs:65-67` |
| `:113` | Clean EOF | **Yes** — the new flush site |
| `:116` | Transport error on the event stream | **No** — see [What "truncation" means here](#what-truncation-means-here) |
| `:138` | `consume` returned `Err` | **No** — `Err` is already terminal |

`consume` returns `Err` for **two** reasons, not one: the in-band `error` event
(`stream.rs:171-173`) *and* the `input_json_delta`-without-`tool_use` protocol
violation (`stream.rs:142-145`).

Not an exit, but interacting with the fix: the unparseable-payload `continue` at
`model.rs:132`. An unparseable `message_stop` is now **rescued** by the clean-EOF
flush — a small unplanned win worth knowing about.

Related forward-compatibility note: `AnthropicEvent` (`src/sse.rs:9-40`) has no
catch-all variant, so any event type added upstream (fine-grained tool streaming,
interleaved thinking betas) fails deserialization and hits that same `continue`.
If Anthropic ever ships a terminal event other than `message_stop`, the clean-EOF
flush becomes the only thing that saves the stream — an argument in this fix's
favour.

## Testing

Tests are labelled by **what each can actually catch**, following the SMA-522
precedent. The acceptance criteria demand a regression test *verified failing
against current code* — "a test that cannot fail on broken code is what let the
OpenAI bug ship."

### Red-first regression tests

These fail against current code. Both go in `tests/messages_streaming.rs`.

| Test | Fixture | What it catches |
| --- | --- | --- |
| `clean_eof_after_message_delta_emits_finish` | new — `text_only.txt` minus the `message_stop` event | **The bug.** Current code emits `[Usage, TokenDelta, TokenDelta, Usage]` and returns at `model.rs:113` with the reason still buffered — no `Finish`. This is the AC's verified-failing test |
| `body_cut_inside_message_stop_emits_finish` | new — cut **mid-line inside** the `message_stop` event | The same bug via the shape a real byte-level cut actually takes: the partial event is discarded by the SSE parser, so this is a strictly stronger red-first test than the clean-boundary one |

### Over-firing guards

These pass today **and** after the fix. They exist so a wrong fix goes red.

| Test | Fixture | Mutation it kills |
| --- | --- | --- |
| `eof_mid_content_block_emits_no_finish` | new — cut after a `content_block_delta` | `finish()` returning `Finish{Stop}` unconditionally |
| `error_after_buffered_stop_reason_emits_no_finish` | new — `message_delta` with `stop_reason`, then an in-band `error` event | A fix that flushes on the error arm. The existing `stream_error.txt` **cannot** catch this: that fixture has no `message_delta` at all, so it never buffers a reason and an error-arm flush would go undetected |

Each asserts on the **whole event sequence**, not just `last()` — an assertion on
the last element alone cannot distinguish "no `Finish`" from "no events".

### Fixture byte-level requirement

`eventsource-stream` 0.2 (`Cargo.toml:28`) follows the SSE spec: an event
dispatches on a **blank line**, and pending data at EOF is discarded. Every new
fixture whose final event must reach the translator **must** end with `\n\n`.

This is a trap, not a nicety. A truncated fixture missing its terminator never
delivers `message_delta`, so `stop_reason` is never buffered, `finish()` returns
`None`, and `clean_eof_after_message_delta_emits_finish` stays **red after the
fix** — inviting an implementer to "fix" it by making `finish()` unconditional.
The existing fixtures get this right (`text_only.txt` ends `0a0a`, verified with
`xxd`) but only by convention.

`body_cut_inside_message_stop_emits_finish` is the deliberate exception: its final
partial event has *no* terminator, and that is the point.

### Invariant tests (unit, in `stream.rs`)

These cannot be red-first — they call a method that does not yet exist, so they
fail to compile rather than fail. They pin the invariants instead.

- `finish_flushes_pending_stop_reason` — `message_delta` with a stop reason, no
  `message_stop`, then `finish()` → `Some(Ok(Finish{Stop}))`.
- `finish_flushes_tool_use_as_tool_calls` — the highest-consequence `Ok` variant:
  the agent loop executes tool calls assembled from a cut-short stream.
- `finish_is_none_when_no_stop_reason_observed` → `None`.
- `finish_is_none_after_message_stop_drained_it` — the anti-double-emit test.
  Also asserts a **second** `finish()` call returns `None` (idempotency, matching
  Bedrock's `providers-bedrock/src/stream.rs:787-791`).
- `second_message_delta_after_message_stop_does_not_double_finish` — the
  `terminal_emitted` guard, against the exact sequence in the ownership-invariant
  section.
- `repeated_message_stop_emits_one_finish` — the pre-existing inline double-emit
  the guard also closes.
- `finish_surfaces_refusal_as_error` → `Some(Err(ModelError::Refused { .. }))`.
- `finish_surfaces_both_tools_error` — the second `Err` row, which the earlier
  draft left untested even though Bedrock tests its equivalent
  (`providers-bedrock/src/stream.rs:814-829`).

### Cancellation tests (new file)

`tests/cancellation.rs` — the Anthropic crate has **zero** cancellation coverage
today, so the AC's cancellation clause is currently unpinned by any test. Modelled
on `providers-openai/tests/cancellation.rs`.

- `cancellation_before_first_chunk_emits_no_finish` — named for what it actually
  reaches. wiremock 0.6's only delay primitive is `ResponseTemplate::set_delay`,
  which delays the **whole** response, so cancellation always fires at
  `model.rs:60`, before a translator exists. The interesting case — cancel firing
  *after* `message_delta` buffered a reason — is **unreachable** in this harness;
  the OpenAI file documents exactly this and its comment is carried over verbatim.
  That case is guaranteed **structurally**: the `tokio::select!` cancel arm at
  `model.rs:109` `return`s without calling `translator.finish()`.
- `uncancelled_stream_emits_exactly_one_finish` — **not optional.** Without this
  control, the test above passes against a build that emits no events at all; the
  pair is what makes the cancellation assertion meaningful rather than vacuous.

### Backfilling the no-regression claim

The "all five existing fixtures keep their exact behaviour" claim is currently
**unenforced**. `tests/messages_streaming.rs:64-83` asserts `oks[0]`..`oks[4]`
positionally but never asserts the length; `:113-118` asserts only `oks.last()`.
A regression appending a second `Finish` passes both unchanged — by this spec's
own standard, exactly a test that cannot fail on broken code.

- Add `assert_eq!(oks.len(), 5)` to
  `text_only_stream_emits_usage_token_deltas_usage_finish`.
- Add a shared helper asserting **exactly one `Finish` and that it is last**, and
  apply it to all five fixture tests.

## Scope boundaries

Deferred, with owners (both verified as filed Linear issues):

- **Core contract wording** (stating the emission guarantee, not just ordering) →
  **SMA-533**, appended during this ticket's design phase. It lands alongside the
  conformance suite that enforces it, so prose and test cannot drift — the drift
  SMA-532 documents in the Bedrock comments.
- **Unifying the three `finish()` return shapes** → **SMA-533**.
- **Bedrock doc comments misstating the ordering contract** → **SMA-532**.
- **Dirty-cut (transport-error) truncation** → out of scope by design; see
  [What "truncation" means here](#what-truncation-means-here).

Conscious no-ops (per CLAUDE.md these must be deliberate calls, not silent skips):

- **No mdBook edit.** No public API, quickstart/example flow, crate-roster, or
  documented-concept change. `finish()` is `pub(crate)`. The book's only mention
  of `Finish` (`docs/book/src/concepts/model-providers.md:57`) describes the
  `ModelEvent` union, which is unchanged.
- **No crate README edit.** `crates/paigasus-helikon-providers-anthropic/README.md`
  carries no streaming or `Finish` content; public surface, install story, feature
  flags, and published status are all unchanged.

## Release impact

`paigasus-helikon-providers-anthropic` takes a patch bump (`0.1.21` → `0.1.22`)
through release-plz's normal flow — an already-released crate, no stub-ascend
ritual. **release-plz then cascades an automatic patch bump to the facade**, since
`release-plz.toml:10` sets `dependencies_update = true` and the facade pins
`paigasus-helikon-providers-anthropic = { … version = "0.1.21" }` at root
`Cargo.toml:146`.

No `-core` bump and no *manual* version edits in this PR, so none of CLAUDE.md's
same-PR manual-bump caveats apply.

PR-title gates: `providers-anthropic` is a valid scope in both `.versionrc:18` and
`.github/workflows/pr-title.yml:53`.

## Acceptance criteria → coverage

| Criterion | Covered by | Status |
| --- | --- | --- |
| Clean EOF after an observed stop reason emits exactly one `Finish` | `clean_eof_after_message_delta_emits_finish`, `body_cut_inside_message_stop_emits_finish`, `finish_flushes_pending_stop_reason`, `finish_flushes_tool_use_as_tool_calls` | Fully tested |
| Clean EOF with no stop reason observed emits no `Finish` | `eof_mid_content_block_emits_no_finish`, `finish_is_none_when_no_stop_reason_observed` | Fully tested |
| Cancellation emits no `Finish` | `cancellation_before_first_chunk_emits_no_finish` + control | **Partially tested + structurally guaranteed** — the post-buffer interleaving is unreachable under wiremock |
| Mid-stream error emits no `Finish` | `error_after_buffered_stop_reason_emits_no_finish` | Fully tested |
| Regression test verified failing against current code | `clean_eof_after_message_delta_emits_finish`, run red before the fix | Fully tested |
| *(added)* At most one terminal event per stream, including malformed input | `second_message_delta_after_message_stop_does_not_double_finish`, `repeated_message_stop_emits_one_finish` | Fully tested |
| *(added)* Well-formed streams still emit exactly one `Finish`, last | shared helper across all five existing fixture tests | Fully tested |
