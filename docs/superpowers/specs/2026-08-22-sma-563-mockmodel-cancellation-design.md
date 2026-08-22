# SMA-563 — `MockModel` honors its cancellation token

**Date:** 2026-08-22
**Ticket:** [SMA-563](https://linear.app/smaschek/issue/SMA-563/paigasus-helikon-evals-mockmodel-ignores-its-cancellation-token)
**Related:** SMA-533 (cross-provider stream conformance suite — where the defect was found)
**Classification:** bounded — one `invoke` body, its tests, and the two doc pages that describe it.

## 1. The defect

`crates/paigasus-helikon-evals/src/mock.rs` binds its token as `_cancel` and returns
`stream::iter(script)`. The whole scripted response — `Finish` included — is delivered
regardless of cancellation.

`paigasus_helikon_core::Model::invoke` (`crates/paigasus-helikon-core/src/model.rs:66-69`)
requires the opposite:

> Implementations that cannot honor cancellation MUST still terminate the stream when the
> `CancellationToken` fires (drop the underlying connection and end the stream without
> emitting `Finish`).

`MockModel` is what consumers reach for when testing their own agent loops. A mock that
ignores cancellation lets a consumer's cancellation handling look correct under test and
fail against a real provider — the one place this is least likely to be noticed.

### Scope is exactly one type

All 39 `impl Model for` sites in the workspace were checked. Every other one in a `src/`
file is behind that file's `#[cfg(test)]`, is a doc example, or is a pure delegator
(`CliModel` forwards the token to its inner model unchanged). `MockModel` is the only
shipped, non-test `Model` implementation that ignores its token. No sweep is warranted.

## 2. Behavior after the fix

`MockModel::invoke` keeps its current structure: it pops exactly one script per call and
still returns `ModelError::Other("MockModel: no more scripted responses")` when the queue
is empty. The returned stream gains one rule:

> Before yielding each item, the stream observes the token. The first observation of a
> fired token ends the stream.

Three consequences, all of which the tests pin:

| When the token fires | What the consumer sees |
|---|---|
| Before `invoke` is called | An empty stream. The script is still popped. |
| After *k* events have been yielded | Exactly those *k* events, then end-of-stream. No `Finish`. |
| Never | The full script, unchanged from today. |

### Decision: a pre-cancelled `invoke` still pops its script

`invoke` does its normal work and hands back an empty stream rather than returning `Err`
or leaving the queue untouched. Three reasons:

1. **Consistency with `RetryingModel`,** which deliberately does not race `invoke` with
   cancellation (`crates/paigasus-helikon-runtime-tokio/src/retry.rs:180-186`): racing it
   "would let a pre-cancelled token skip the invoke entirely (counter-intuitive and would
   break the attempt-count contract)".
2. **"One script consumed per `invoke`" stays unconditionally true,** so exhaustion is
   deterministic and independent of cancellation timing. The alternative — leaving the
   script when cancelled *before* `invoke` but consuming it when cancelled *mid-stream* —
   is two rules where one will do.
3. **The contract sentence presumes a stream** ("terminate the stream ... without emitting
   `Finish`"). Returning `Err` would make a cancelled turn indistinguishable from a genuine
   transport failure in a consumer's error branch.

### Decision: a synchronous `is_cancelled()` check, not `select!`

A scripted stream never awaits — every item is ready at poll time. There is therefore no
window in which the stream is parked and would need the token to wake it, which is the only
thing `select!` on `cancelled()` would buy. `is_cancelled()` is precisely the observation
available at each poll, and it keeps the stream free of any runtime dependency.

## 3. Implementation

**`crates/paigasus-helikon-evals/src/mock.rs`** — rename `_cancel` to `cancel` and wrap the
iterator:

```rust
Ok(Box::pin(
    stream::iter(script.into_iter().map(Ok))
        .take_while(move |_| std::future::ready(!cancel.is_cancelled())),
))
```

`take_while` comes from `futures_util::StreamExt`, which the file does not yet import. It
pulls the next item *before* evaluating the predicate and drops it when the predicate is
false; since the stream owns the script exclusively, that is not observable. If the closure
does not satisfy the `Send + 'static` bound `BoxStream` needs, `futures_util::stream::unfold`
over `(script.into_iter(), cancel)` is the equivalent fallback.

The struct-level doc comment and the `invoke` doc gain one sentence stating the rule and the
pre-cancelled-still-pops consequence.

## 4. Tests

### 4.1 `crates/paigasus-helikon-evals/tests/mock.rs` — the acceptance criteria

- **`cancel_mid_stream_ends_without_finish`** — script `[TokenDelta("a"), TokenDelta("b"),
  Finish(Stop)]`. Take one event and assert it is `TokenDelta("a")`; fire the token; assert
  the next poll is `None`; assert no `Finish` was ever observed. **Must be verified failing
  against the unfixed implementation**, where the next poll returns `TokenDelta("b")` — that
  verification is the acceptance criterion, not a formality.
- **`pre_cancelled_invoke_is_empty_and_still_pops`** — two scripts, token cancelled before
  the first `invoke`. Assert the first stream is empty; assert the second `invoke` yields the
  **second** script (proving the pop happened); assert a third `invoke` errors as exhausted.

### 4.2 `crates/paigasus-helikon-runtime-tokio/src/retry.rs` — pinning `RetryingModel`

Added to the existing inline `mod tests`, beside `cancellation_aborts_backoff_promptly`,
reusing its `ScriptModel` fake and `drain()` helper.

- **`cancel_after_content_ends_stream_without_finish`** — `Resp::Ok` (`TokenDelta("ok")` then
  `Finish(Stop)`). Take the first event, fire the token, assert the stream ends with no
  `Finish`. Deterministic: the forwarding loop's `select!` is `biased` with `cancelled()`
  first, so a ready token wins over a ready item.
- **`pre_cancelled_invoke_still_calls_inner_once`** — token cancelled before `invoke`; the
  drained stream is empty *and* `model.calls() == 1`. This pins the documented decision that
  `invoke` is not raced with cancellation, which is what keeps the attempt-count contract
  honest.

Both of these pass against today's code by construction, so passing proves nothing on its
own. Each is **mutation-checked**: temporarily delete the `() = cancel.cancelled() => return`
arm from the relevant `select!` and confirm the test goes red. The check is performed and its
result reported; the mutation is not committed.

## 5. Documentation

A user-facing behavior change, so per `CLAUDE.md` both surfaces move in the same PR:

- `crates/paigasus-helikon-evals/README.md` — the `MockModel` and `ScriptFile` section.
- `docs/book/src/concepts/observability-evaluation.md` — the `MockModel` and `ScriptFile`
  subsection.

One sentence each: the stream ends when the token fires, without `Finish`, and a
pre-cancelled `invoke` yields an empty stream but still consumes its script.

## 6. Release mechanics

A `fix(evals)` change to an already-published crate plus a test-only change to
`runtime-tokio`. release-plz handles the patch bump and the facade cascade on its own. No
core API is added, so none of `CLAUDE.md`'s manual-bump caveats apply: no stub ascend, no
same-PR core bump, no manual facade bump.

## 7. Non-goals

- **Registering `MockModel` as a conformance-suite subject.** It is not a provider. The
  harness fires its token only after the stream falls quiet
  (`tests/provider-stream-conformance/src/lib.rs:455-476`), and a synchronous in-memory
  stream never quiesces, so the mechanism does not apply and the suite's missing-evidence
  floor would reject it.
- **Test-local `Model` fakes.** They are not shipped; consumers cannot reach them.
- **Any edit to the `Model` trait's contract wording.** The contract is correct; the
  implementation was wrong.
