# SMA-563 — `MockModel` honors its cancellation token

**Date:** 2026-08-22
**Ticket:** [SMA-563](https://linear.app/smaschek/issue/SMA-563/paigasus-helikon-evals-mockmodel-ignores-its-cancellation-token)
**Related:** SMA-533 (cross-provider stream conformance suite — where the defect was found)
**Classification:** bounded — two `invoke` bodies, their tests, and the doc pages that describe them.

## 1. The defect

`crates/paigasus-helikon-evals/src/mock.rs` binds its token as `_cancel` and returns
`stream::iter(script)`. The whole scripted response — `Finish` included — is delivered
regardless of cancellation.

`paigasus_helikon_core::Model::invoke` (`crates/paigasus-helikon-core/src/model.rs:68-70`)
requires the opposite:

> Implementations that cannot honor cancellation MUST still terminate the stream when the
> `CancellationToken` fires (drop the underlying connection and end the stream without
> emitting `Finish`).

`MockModel` is what consumers reach for when testing their own agent loops. A mock that
ignores cancellation lets a consumer's cancellation handling look correct **at the `Model`
boundary** and fail against a real provider — the one place this is least likely to be
noticed. §2.3 states precisely how far that claim goes, because it does not extend to the
run level.

### 1.1 Scope: two shipped implementations, not one

All 39 `impl Model for` sites in the workspace fall into four buckets:

1. **Real providers and decorators** — `providers-openai/src/model.rs:54`,
   `providers-anthropic/src/model.rs:39`, `providers-bedrock/src/model.rs:34`,
   `providers-gemini/src/model.rs:78`, `providers-litellm/src/model.rs:74`, and
   `runtime-tokio/src/retry.rs:169`. All six honor the token, and the first five are held to
   it by the SMA-533 conformance suite's assertion 5. `RetryingModel` is not — §4.2 is what
   pins it.
2. **Test-local fakes** — behind their file's `#[cfg(test)]`, or in `tests/`. Not shipped.
3. **Doc examples and pure delegators** — `core/src/model.rs`'s `NoopModel`,
   `core/src/agent.rs:327`'s `MyModel`, temporal's `NullModel`; and `CliModel`
   (`cli/src/model.rs`), which forwards the token to its inner model unchanged.
4. **Shipped implementations that ignore the token** — `evals::MockModel`, **and
   `DemoModel` at `crates/paigasus-helikon/examples/langfuse_tracing.rs:76-97`**, which
   binds `_cancel` and returns `stream::iter(events)`: the identical defect.

`DemoModel` is not `#[cfg(test)]`, not a doc example, and not a delegator. It is compiled by
`cargo clippy --all-targets`, packaged into the published facade crate, and is exactly the
scaffold a consumer copies when writing their own scripted model. It is fixed in this PR by
the same two-line change; leaving a defective copy-paste template while fixing the mock
would defeat the ticket's own reasoning.

## 2. Behavior after the fix

`MockModel::invoke` keeps its current structure: it pops exactly one script per call and
still returns `ModelError::Other("MockModel: no more scripted responses")` when the queue
is empty. The returned stream gains one rule:

> At each poll the stream pulls the next scripted item, then observes the token. If the
> token has fired, the item is dropped and the stream ends; otherwise the item is yielded.

The token is **observed, not awaited**. A consumer that stops polling never learns the
stream has ended — which is exactly what `is_cancelled()` delivers and all a synchronous
mock can offer. The doc wording must not imply a wakeup.

Three consequences:

| When the token fires | What the consumer sees | Pinned by |
|---|---|---|
| Never | The full script, in order, `Finish` included | §4.1 `uncancelled_invoke_yields_full_script` |
| After *k* events | Exactly those *k* events, then end-of-stream. No `Finish`. | §4.1 `cancel_mid_stream_ends_without_finish` |
| Before `invoke` is called | An empty stream. The script is still popped. | §4.1 `pre_cancelled_invoke_is_empty_and_still_pops` |

The un-cancelled row is not a formality. The change inserts a combinator whose entire job
is to drop items, and **no existing test drains a `MockModel` stream to `None`** — the three
in `crates/paigasus-helikon-evals/tests/mock.rs` each read one event and stop, and the only
draining consumer, `tests/eval_run.rs`, is blind to a dropped trailing `Finish` because
`ModelTurnAccumulator::new` defaults `finish_reason` to `FinishReason::Stop`
(`crates/paigasus-helikon-core/src/model.rs:569`), so `ExactMatch` still passes. An
off-by-one that silently drops the last event of every script would ship green today.

### 2.1 Truncating mid-tool-call is now reachable

A script cancelled part-way through a `ToolCallDelta` sequence leaves a truncated
`args_delta`. `build_items` (`crates/paigasus-helikon-core/src/model.rs:530-535`) does
`serde_json::from_str` on the concatenation, so `acc.finish()` returns `Err` and
`agent.rs:983-991` surfaces it as `AgentEvent::RunFailed { error: "invalid tool args for
call_id=…" }`. This cannot happen today because `MockModel` never truncates.

**This is correct, not a regression** — it is what a real provider does when a connection
drops mid-tool-call, and reproducing it deterministically is a capability the mock gains.
But it is a new user-visible failure shape reachable only through this change, so it is
stated here and tested at the `Model` boundary (§4.1).

### 2.2 Decision: a pre-cancelled `invoke` still pops its script

`invoke` does its normal work and hands back an empty stream rather than returning `Err`
or leaving the queue untouched. Three reasons:

1. **Consistency with `RetryingModel`,** which deliberately does not race `invoke` with
   cancellation (`crates/paigasus-helikon-runtime-tokio/src/retry.rs:181-187`): racing it
   "would let a pre-cancelled token skip the invoke entirely (counter-intuitive and would
   break the attempt-count contract)".
2. **"One script consumed per `invoke`" stays unconditionally true,** so exhaustion is
   deterministic and independent of cancellation timing. The alternative — leaving the
   script when cancelled *before* `invoke` but consuming it when cancelled *mid-stream* —
   is two rules where one will do.
3. **The contract sentence presumes a stream** ("terminate the stream ... without emitting
   `Finish`"). Returning `Err` would make a cancelled turn indistinguishable from a genuine
   transport failure in a consumer's error branch.

### 2.3 Run-level cancellation semantics are unchanged — deliberately

This fix operates at the `Model` boundary and stops there. A pre-cancelled run backed by
`evals::MockModel` still returns `Ok(RunResult)`, not `Err(RunError::Cancelled)`: the empty
stream produces `ModelTurn { items: [], finish_reason: Stop }`, the loop terminates on it,
and `controlled()` (`crates/paigasus-helikon-runtime-tokio/src/lib.rs:54-71`) is `biased`
with `stream.next()` **first** — so with a synchronous mock, whose events are always ready,
the `cancelled()` arm is never reached before the terminal arrives.

That is a deliberate decision from SMA-421 ("a genuine terminal wins over any
cancel/timeout"), codified at `crates/paigasus-helikon-runtime-tokio/tests/run_control.rs:63-87`
(`prefired_cancel_still_completes_ready_run`). **This spec does not touch it.** Against a
real provider the stream would be `Pending`, the cancel arm would win, and the caller would
get `Err(RunError::Cancelled)` — so a divergence between mock and provider survives at the
run level. Closing that is a separate question about `controlled()`'s bias, not about
`MockModel`, and is out of scope here.

### 2.4 Decision: a synchronous `is_cancelled()` check, not `select!`

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

`take_while` comes from `futures_util::StreamExt`, which the file does not yet import. The
bound is satisfied, not merely hoped for: `futures-util` is pinned at `0.3` with
`default-features = false, features = ["std"]` (root `Cargo.toml`), `StreamExt::take_while`
is not feature-gated, `std::future::ready` yields a `Send` `Ready<bool>`, and
`CancellationToken` is `tokio_util`'s (re-exported at
`crates/paigasus-helikon-core/src/context.rs:1006`), which is `Send + Sync + 'static`. So
the combinator is `Send + 'static` and boxes into `BoxStream<'static, …>`.

`take_while` pulls the next item *before* evaluating the predicate and drops it when the
predicate is false. That is unobservable here — the stream owns the `Vec<ModelEvent>`
exclusively and dropping an unconsumed `ModelEvent` has no side effect — and it is why §2
states the rule as pull-then-observe. **There is no `unfold` fallback**: `unfold` over
`(script.into_iter(), cancel)` naturally checks *before* pulling, which is a different rule
than the one documented, and the bound above removes the reason to hedge.

**`crates/paigasus-helikon/examples/langfuse_tracing.rs`** — the same two-line change to
`DemoModel` (§1.1).

The `MockModel` struct doc and its `invoke` doc gain a sentence stating the rule, that the
token is observed at each poll rather than awaited, and that a pre-cancelled `invoke` still
consumes its script.

## 4. Tests

### 4.1 `crates/paigasus-helikon-evals/tests/mock.rs` — the acceptance criteria

- **`uncancelled_invoke_yields_full_script`** — script `[TokenDelta("a"), TokenDelta("b"),
  Finish(Stop)]`, token never fired. Drain to `None` and assert all three events in order,
  the terminal `Finish` included. Guards the drop-combinator against an off-by-one that
  nothing else in the repo would catch (§2).
- **`cancel_mid_stream_ends_without_finish`** — the same script. Take one event and assert
  it is `TokenDelta("a")`; fire the token; assert the next poll is `None`; assert no
  `Finish` was ever observed. **Must be verified failing against the unfixed
  implementation**, where the next poll returns `TokenDelta("b")`. That verification is the
  acceptance criterion, not a formality, and its output is reported.
- **`pre_cancelled_invoke_is_empty_and_still_pops`** — two scripts, distinguishable by
  their first event. Invoke #1 gets an already-cancelled token; assert its stream is empty.
  **Invokes #2 and #3 each get a fresh `CancellationToken::new()`** — without that, #2's
  stream would also be empty and the assertion below would be unwritable. Assert #2 yields
  the **second** script's first event (proving #1 popped), and that #3 errors as exhausted.
- **`cancel_mid_tool_call_truncates_the_args`** — script
  `[ToolCallDelta{call_id:"c1", name:Some("f"), args_delta:"{\"a\":"},
  ToolCallDelta{call_id:"c1", name:None, args_delta:"1}"}, Finish(ToolCalls)]`. Take the
  first delta, cancel, assert the stream ends with only that delta and no `Finish`. Pins
  §2.1 at the `Model` boundary — the downstream parse failure is correct behavior and is
  documented, not asserted here.

### 4.2 `crates/paigasus-helikon-runtime-tokio/src/retry.rs` — pinning `RetryingModel`

Added to the existing inline **`mod decorator_tests`** (line 378), which is where
`ScriptModel`, `Resp`, `drain()` and `cancellation_aborts_backoff_promptly` live.

**A correction lands first.** `cancellation_aborts_backoff_promptly` (`retry.rs:544-561`)
does not test what its name says. It fires the token *before* the first poll; the peek
`select!` (`retry.rs:211-215`) is `biased` with `cancelled()` first, so it returns before
`model_stream.next()` is ever polled — the scripted `Resp::ErrFirst` is never observed and
`policy.next_delay`/`backoff` are never reached. The comment at `retry.rs:558` ("invoke #1
fails, enters backoff, cancellation wins") is false, and `backoff()`'s cancelled branch
(`retry.rs:163`) together with both `if !backoff(…) { return; }` sites (`retry.rs:196`,
`retry.rs:241`) have **zero** coverage. The comment is corrected in this PR.

Two tests are added:

- **`cancel_after_content_ends_stream_without_finish`** — `Resp::Ok` (`TokenDelta("ok")`
  then `Finish(Stop)`). Take the first event, fire the token, assert the stream ends with no
  `Finish`. Deterministic: the forwarding loop's `select!` (`retry.rs:223-227`) is `biased`
  with `cancelled()` first, and `WaitForCancellationFuture` is `Ready` on first poll for an
  already-cancelled token, so a ready token beats a ready item with no race.
- **`cancel_during_backoff_ends_stream`** — the coverage gap above. Under
  `start_paused = true`, a script of `[ErrFirst(Unavailable), Ok]` with a long `base_delay`;
  spawn the drain, let attempt #1 fail and park in `backoff()`, then fire the token from
  another task. Assert the stream ends with no `Finish` and `calls() == 1`. This is the only
  `RetryingModel` cancellation path involving real latency, and it is currently untested.

A test that pre-fires the token and asserts `calls() == 1` is **not** added: that is exactly
`cancellation_aborts_backoff_promptly`, which already covers it (its name notwithstanding).

**Mutation checks.** Both new tests pass against today's code by construction, so passing
proves nothing on its own. Each is falsified before being trusted, and the result reported:

| Test | Mutation | Expected |
|---|---|---|
| `cancel_after_content_ends_stream_without_finish` | delete `() = cancel.cancelled() => return` from the forwarding loop (`retry.rs:224`) | red — `Finish` is emitted |
| `cancel_during_backoff_ends_stream` | make `backoff()` (`retry.rs:160-166`) always `sleep` and return `true` | red — the test hangs or retries |

Neither mutation is committed. Note the mutation that would falsify "invoke is not raced
with cancellation" is *adding* a race around `retry.rs:188`, not deleting the peek arm —
deleting the peek arm leaves `calls() == 1` passing, which is why no test in this spec
rests on that assertion alone.

## 5. Documentation

A user-facing behavior change, so per `CLAUDE.md` these surfaces move in the same PR:

- `crates/paigasus-helikon-evals/README.md` — the `MockModel` and `ScriptFile` section.
- `docs/book/src/concepts/observability-evaluation.md` — the `MockModel` and `ScriptFile`
  subsection.

One sentence each: the stream ends when the token fires, without `Finish`; the token is
observed at each poll rather than awaited; a pre-cancelled `invoke` yields an empty stream
but still consumes its script.

`crates/paigasus-helikon-evals/src/lib.rs` does **not** `include_str!` its README — only the
facade does (`crates/paigasus-helikon/src/lib.rs:1`) — so the new README sentence carries no
doctest risk.

**Consciously skipped:** `docs/book/src/reference/crates.md:39`. Its one-line description of
`paigasus-helikon-evals` needs no behavioral edit. Its version column is stale (`0.1.6`
against an actual `0.1.7`), but that column mirrors release-plz-owned data by hand and this
PR cannot know its own post-merge number; hand-editing it would be a guess that goes stale
again on the next release. The staleness is pre-existing and is raised separately rather
than patched blind here.

## 6. Release mechanics

A `fix(evals)` change touching packaged files in **two** crates:
`crates/paigasus-helikon-evals/**` and `crates/paigasus-helikon-runtime-tokio/src/retry.rs`
(the retry tests are inline in a packaged source file). release-plz attributes bumps by file
path, so both take a patch bump and the facade cascades. All automatic — no core API is
added, so none of `CLAUDE.md`'s manual-bump caveats apply: no stub ascend, no same-PR core
bump, no manual facade bump.

## 7. Non-goals

- **Registering `MockModel` as a conformance-suite subject.** It is not a provider. The
  harness fires its token only after the stream falls quiet
  (`tests/provider-stream-conformance/src/lib.rs:419-432`, `457-477`), and a synchronous
  in-memory stream never quiesces, so the mechanism does not apply and the suite's
  missing-evidence floor would reject it.
- **Reusing `check::classify` directly on the drained events.** It is `pub` and
  HTTP-independent, so this was considered. Declined on two grounds: it takes a `Scenario`,
  which encodes the paced-HTTP scenario catalogue (and drives `expects_stop_reason()`)
  rather than an arbitrary event sequence, so every call would be a forced fit; and it would
  make `evals` dev-depend on a `publish = false` crate that pulls `hyper` and
  `aws-smithy-eventstream` into its dev graph. The mock's assertions are two lines and are
  cheap to move by hand if the contract's rules change.
- **Changing run-level cancellation semantics** — see §2.3. `controlled()`'s biased
  ordering is SMA-421's deliberate decision.
- **A `tracing::debug!` on the empty-stream path.** `evals` has no `tracing` dependency, and
  adding one for a debug line is disproportionate. The confusing case — a silently consumed
  script — is addressed by documenting it in the `invoke` doc and the README instead.
- **Test-local `Model` fakes.** They are not shipped; consumers cannot reach them.
- **Any edit to the `Model` trait's contract wording.** The contract is correct; the
  implementations were wrong.
