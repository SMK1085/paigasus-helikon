# SMA-563 `MockModel` Cancellation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the two shipped `Model` implementations that ignore their `CancellationToken` — `evals::MockModel` and the facade's `DemoModel` example — terminate their streams when the token fires without emitting `Finish`, and pin `RetryingModel`'s cancellation conformance with tests that are verified to actually catch a regression.

**Architecture:** Both offenders return `futures_util::stream::iter(events)`. Both gain the same one-line wrapper: `.take_while(move |_| std::future::ready(!cancel.is_cancelled()))`. The token is *observed* at each poll, not awaited — correct because a scripted stream never parks, so there is no window in which a wakeup would matter. `invoke` itself is unchanged: it still pops exactly one script per call, so exhaustion stays independent of cancellation timing.

**Tech Stack:** Rust 2024, `futures-util` 0.3 (`StreamExt::take_while`), `tokio_util::sync::CancellationToken` (re-exported as `paigasus_helikon_core::CancellationToken`), `#[tokio::test]`.

**Spec:** `docs/superpowers/specs/2026-08-22-sma-563-mockmodel-cancellation-design.md`

## Global Constraints

- **Commit prefix:** `<type>(<scope>): SMA-563 <lowercase message>`. Valid scopes for this work: `evals`, `runtime-tokio`, `facade`, `docs`, `spec`, `plan`. `convco` runs in a `commit-msg` hook and will reject anything else.
- **Run `cargo fmt --all` before every commit.** The `pre-commit` hook is a deliberate no-op; `pre-push` runs fmt + full-workspace clippy and is slow, so catching it early saves minutes.
- **Never `git add -A`** — `.env` and `.claude` are untracked but *not* gitignored. Use explicit paths, then verify with `git show --stat`.
- **Every new test must be falsified before it is trusted.** Tasks 1, 4 and 5 each carry an explicit mutation step. Report the observed output; do not assert a test is meaningful because it is green.
- **Do not commit any mutation.** Every mutation step is paired with a revert step.
- **Do not edit the `Model` trait's contract wording** in `crates/paigasus-helikon-core/src/model.rs`. The contract is correct; the implementations were wrong.
- **`git` safety:** work only inside this worktree. Never run a command that moves `HEAD` to another branch or checks out `main`.

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/paigasus-helikon-evals/tests/mock.rs` | Acceptance tests for `MockModel` cancellation | 1, 2 |
| `crates/paigasus-helikon-evals/src/mock.rs` | The fix + its doc comments | 2 |
| `crates/paigasus-helikon/examples/langfuse_tracing.rs` | Same fix for the shipped `DemoModel` example | 3 |
| `crates/paigasus-helikon-runtime-tokio/src/retry.rs` | `RetryingModel` conformance pins, in `mod decorator_tests` | 4, 5 |
| `crates/paigasus-helikon-evals/README.md` | Crate-page doc | 6 |
| `docs/book/src/concepts/observability-evaluation.md` | mdBook doc | 6 |

Task 1 is deliberately split from Task 2: Task 1's whole value is that the tests **fail**, and a reviewer must be able to see that failure output before any fix exists to hide it.

---

### Task 1: Write the failing acceptance tests for `MockModel`

**Files:**
- Modify: `crates/paigasus-helikon-evals/tests/mock.rs` (append; the file currently ends at line 75 with `script_file_selects_per_case_with_default_fallback`)

**Interfaces:**
- Consumes: `MockModel::with_script(Vec<ModelEvent>) -> Arc<Self>`, `MockModel::with_scripts(Vec<Vec<ModelEvent>>) -> Arc<Self>`, `Model::invoke(ModelRequest, CancellationToken) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError>` — all already exist and are unchanged by this plan.
- Produces: nothing later tasks consume.

- [ ] **Step 1: Add the imports the new tests need**

The file's current header is:

```rust
use futures_util::StreamExt as _;
use paigasus_helikon_core::{CancellationToken, Model, ModelEvent, ModelRequest};
use paigasus_helikon_evals::{MockModel, ScriptFile};
```

`FinishReason` is needed for the scripts. Change the middle line to:

```rust
use paigasus_helikon_core::{CancellationToken, FinishReason, Model, ModelEvent, ModelRequest};
```

- [ ] **Step 2: Append the four new tests**

Append verbatim to the end of `crates/paigasus-helikon-evals/tests/mock.rs`:

```rust
/// The three-event script the cancellation tests share.
fn abc_script() -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta { text: "a".into() },
        ModelEvent::TokenDelta { text: "b".into() },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

/// Guards the drop-combinator: with no cancellation the whole script must
/// still arrive, terminal `Finish` included. No other test in this repo
/// drains a `MockModel` stream to `None`, and `eval_run.rs` cannot catch a
/// dropped trailing `Finish` because `ModelTurnAccumulator` defaults
/// `finish_reason` to `Stop` — so an off-by-one here would ship green.
#[tokio::test]
async fn uncancelled_invoke_yields_full_script() {
    let model = MockModel::with_script(abc_script());
    let mut s = model
        .invoke(ModelRequest::new(), CancellationToken::new())
        .await
        .unwrap();

    let mut got = Vec::new();
    while let Some(item) = s.next().await {
        got.push(item.unwrap());
    }

    assert_eq!(got.len(), 3, "whole script must arrive: {got:?}");
    assert!(matches!(&got[0], ModelEvent::TokenDelta { text } if text == "a"));
    assert!(matches!(&got[1], ModelEvent::TokenDelta { text } if text == "b"));
    assert!(
        matches!(
            &got[2],
            ModelEvent::Finish {
                reason: FinishReason::Stop
            }
        ),
        "terminal Finish must not be dropped: {:?}",
        got[2]
    );
}

/// The acceptance criterion: cancelling mid-stream truncates and withholds
/// `Finish`, per the `Model::invoke` contract.
#[tokio::test]
async fn cancel_mid_stream_ends_without_finish() {
    let cancel = CancellationToken::new();
    let model = MockModel::with_script(abc_script());
    let mut s = model
        .invoke(ModelRequest::new(), cancel.clone())
        .await
        .unwrap();

    let first = s.next().await.unwrap().unwrap();
    assert!(matches!(&first, ModelEvent::TokenDelta { text } if text == "a"));

    cancel.cancel();

    let rest: Vec<_> = {
        let mut v = Vec::new();
        while let Some(item) = s.next().await {
            v.push(item.unwrap());
        }
        v
    };
    assert!(
        rest.is_empty(),
        "stream must end on cancellation, got {rest:?}"
    );

    let all = [vec![first], rest].concat();
    assert!(
        !all.iter()
            .any(|e| matches!(e, ModelEvent::Finish { .. })),
        "a cancelled stream must not emit Finish: {all:?}"
    );
}

/// A pre-cancelled `invoke` yields nothing but still consumes its script, so
/// "one script per invoke" holds regardless of cancellation timing.
///
/// Invokes #2 and #3 get FRESH tokens on purpose: reusing the cancelled one
/// would make their streams empty too and the "second script" assertion
/// unwritable.
#[tokio::test]
async fn pre_cancelled_invoke_is_empty_and_still_pops() {
    let cancel = CancellationToken::new();
    cancel.cancel();

    let model = MockModel::with_scripts(vec![
        vec![ModelEvent::TokenDelta {
            text: "first".into(),
        }],
        vec![ModelEvent::TokenDelta {
            text: "second".into(),
        }],
    ]);

    let mut s1 = model
        .invoke(ModelRequest::new(), cancel.clone())
        .await
        .unwrap();
    assert!(
        s1.next().await.is_none(),
        "a pre-cancelled invoke must yield an empty stream"
    );

    // Fresh token: proves script #1 was popped, not replayed.
    let mut s2 = model
        .invoke(ModelRequest::new(), CancellationToken::new())
        .await
        .unwrap();
    let ev = s2.next().await.unwrap().unwrap();
    assert!(
        matches!(&ev, ModelEvent::TokenDelta { text } if text == "second"),
        "invoke #2 must get the SECOND script, got {ev:?}"
    );

    assert!(
        model
            .invoke(ModelRequest::new(), CancellationToken::new())
            .await
            .is_err(),
        "both scripts consumed, so invoke #3 must report exhaustion"
    );
}

/// Cancelling part-way through a tool call truncates the accumulated
/// `args_delta`. That is correct — it is what a real provider does when the
/// connection drops mid-call — and this pins it at the `Model` boundary. The
/// downstream consequence (core's `build_items` fails to parse the truncated
/// JSON) is documented in the spec, not asserted here.
#[tokio::test]
async fn cancel_mid_tool_call_truncates_the_args() {
    let cancel = CancellationToken::new();
    let model = MockModel::with_script(vec![
        ModelEvent::ToolCallDelta {
            call_id: "c1".into(),
            name: Some("lookup_spending".into()),
            args_delta: "{\"month\":".into(),
        },
        ModelEvent::ToolCallDelta {
            call_id: "c1".into(),
            name: None,
            args_delta: "\"july\"}".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]);
    let mut s = model
        .invoke(ModelRequest::new(), cancel.clone())
        .await
        .unwrap();

    let first = s.next().await.unwrap().unwrap();
    assert!(matches!(
        &first,
        ModelEvent::ToolCallDelta { args_delta, .. } if args_delta == "{\"month\":"
    ));

    cancel.cancel();

    let mut rest = Vec::new();
    while let Some(item) = s.next().await {
        rest.push(item.unwrap());
    }
    assert!(
        rest.is_empty(),
        "stream must end mid-tool-call on cancellation, got {rest:?}"
    );
}
```

- [ ] **Step 3: Format**

Run: `cargo fmt --all`

- [ ] **Step 4: Run the new tests and CAPTURE THE FAILURE**

Run:

```bash
cargo test -p paigasus-helikon-evals --test mock 2>&1 | tail -40
```

Expected, against the **unfixed** `mock.rs`:

- `uncancelled_invoke_yields_full_script` — **PASS** (it describes today's behavior; it is a guard for the *next* task, not a red test).
- `cancel_mid_stream_ends_without_finish` — **FAIL**. The stream ignores the token, so `rest` is `[TokenDelta("b"), Finish(Stop)]` and the `rest.is_empty()` assertion trips with `stream must end on cancellation, got [...]`.
- `pre_cancelled_invoke_is_empty_and_still_pops` — **FAIL**. `s1.next()` returns `TokenDelta("first")`, tripping `a pre-cancelled invoke must yield an empty stream`.
- `cancel_mid_tool_call_truncates_the_args` — **FAIL**. `rest` contains the second delta and the `Finish`.

**This failure output is the acceptance criterion for the ticket.** Paste the three failure messages into the commit body. If any of the three cancellation tests PASSES here, stop — the test is not exercising the defect and must be fixed before proceeding.

- [ ] **Step 5: Commit the failing tests**

```bash
git add crates/paigasus-helikon-evals/tests/mock.rs
git commit -m "test(evals): SMA-563 pin MockModel cancellation, currently failing"
```

---

### Task 2: Make `MockModel` honor the token

**Files:**
- Modify: `crates/paigasus-helikon-evals/src/mock.rs`
- Test: `crates/paigasus-helikon-evals/tests/mock.rs` (from Task 1 — no new tests)

**Interfaces:**
- Consumes: Task 1's four tests.
- Produces: the `take_while` idiom Task 3 copies verbatim.

- [ ] **Step 1: Add the `StreamExt` import**

`crates/paigasus-helikon-evals/src/mock.rs` currently imports:

```rust
use futures_core::stream::BoxStream;
use futures_util::stream;
```

Add `StreamExt` beside it, so the block reads:

```rust
use futures_core::stream::BoxStream;
use futures_util::{stream, StreamExt as _};
```

- [ ] **Step 2: Update the struct doc comment**

Replace the doc comment on `pub struct MockModel` — currently:

```rust
/// A scripted [`Model`] that replays pre-recorded `ModelEvent`s: one
/// script per `invoke` call, in order. Running out of scripts yields a
/// `ModelError` — deterministic by construction.
```

with:

```rust
/// A scripted [`Model`] that replays pre-recorded `ModelEvent`s: one
/// script per `invoke` call, in order. Running out of scripts yields a
/// `ModelError` — deterministic by construction.
///
/// Honors cancellation as [`Model::invoke`] requires: the stream observes
/// the token at each poll and ends on the first fired observation, without
/// emitting `Finish`. The token is *observed*, not awaited — a consumer
/// that stops polling never learns the stream has ended, which is all a
/// synchronous scripted stream can offer.
```

- [ ] **Step 3: Rewrite `invoke`**

Replace the whole `invoke` method — currently:

```rust
    async fn invoke(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let script = self
            .scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| {
                ModelError::Other(anyhow::anyhow!("MockModel: no more scripted responses"))
            })?;
        Ok(Box::pin(stream::iter(script.into_iter().map(Ok))))
    }
```

with:

```rust
    /// Pops one script and replays it.
    ///
    /// The script is popped unconditionally — a pre-cancelled `invoke`
    /// consumes its script and returns an empty stream rather than an error,
    /// so "one script per `invoke`" holds regardless of cancellation timing
    /// and exhaustion stays deterministic. This matches `RetryingModel`,
    /// which deliberately does not race `invoke` with cancellation.
    async fn invoke(
        &self,
        _request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let script = self
            .scripts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
            .ok_or_else(|| {
                ModelError::Other(anyhow::anyhow!("MockModel: no more scripted responses"))
            })?;
        // `take_while` pulls the item before testing the predicate and drops
        // it when false. Unobservable here: the stream owns the script
        // exclusively, so a dropped `ModelEvent` has no side effect.
        Ok(Box::pin(
            stream::iter(script.into_iter().map(Ok))
                .take_while(move |_| std::future::ready(!cancel.is_cancelled())),
        ))
    }
```

- [ ] **Step 4: Format**

Run: `cargo fmt --all`

- [ ] **Step 5: Run the tests — all four must now pass**

Run:

```bash
cargo test -p paigasus-helikon-evals --test mock 2>&1 | tail -20
```

Expected: `test result: ok.` with 8 passed (4 pre-existing + 4 new), 0 failed.

If `uncancelled_invoke_yields_full_script` now FAILS, the combinator is dropping the last event — that is the off-by-one this test exists to catch. Fix it before continuing; do not weaken the test.

- [ ] **Step 6: Run the whole evals crate to check for collateral damage**

Run:

```bash
cargo test -p paigasus-helikon-evals 2>&1 | tail -20
```

Expected: all test binaries green — in particular `eval_run.rs`, which drains `MockModel` streams through `LlmAgent`.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-evals/src/mock.rs
git commit -m "fix(evals): SMA-563 terminate the MockModel stream on cancellation"
```

---

### Task 3: Fix `DemoModel` in the shipped facade example

**Files:**
- Modify: `crates/paigasus-helikon/examples/langfuse_tracing.rs:76-97`

**Interfaces:**
- Consumes: the `take_while` idiom from Task 2.
- Produces: nothing.

Why this is in scope: `DemoModel` has the identical defect, is not `#[cfg(test)]`, is packaged into the published facade crate, and is precisely the scaffold a consumer copies when writing their own scripted model. Confirmed in scope by the user at the spec gate.

- [ ] **Step 1: Apply the same fix**

In `crates/paigasus-helikon/examples/langfuse_tracing.rs`, the `invoke` signature currently binds `_cancel`:

```rust
    async fn invoke(
        &self,
        _req: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
```

Change `_cancel` to `cancel`:

```rust
    async fn invoke(
        &self,
        _req: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
```

Then replace the final line of the method — currently:

```rust
        Ok(Box::pin(futures_util::stream::iter(events)))
```

with:

```rust
        // The `Model::invoke` contract requires the stream to end when the
        // token fires, without emitting `Finish`.
        Ok(Box::pin(
            futures_util::StreamExt::take_while(futures_util::stream::iter(events), move |_| {
                std::future::ready(!cancel.is_cancelled())
            }),
        ))
    }
```

Note the fully-qualified `futures_util::StreamExt::take_while(...)` call form: the example does not `use futures_util::StreamExt`, and this avoids adding an import to a file whose import block is already long. If the implementer prefers, adding `use futures_util::StreamExt as _;` to the import block and using method syntax is equally acceptable — pick one and be consistent.

- [ ] **Step 2: Format**

Run: `cargo fmt --all`

- [ ] **Step 3: Verify the example still compiles**

Run:

```bash
cargo check -p paigasus-helikon --example langfuse_tracing --features runtime-tokio 2>&1 | tail -20
```

Expected: `Finished` with no errors and no warnings about an unused `cancel`.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon/examples/langfuse_tracing.rs
git commit -m "fix(facade): SMA-563 honor cancellation in the langfuse example model"
```

---

### Task 4: Pin `RetryingModel` cancellation after content has started

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/src/retry.rs` — inside `mod decorator_tests` (opens at line 378)

**Interfaces:**
- Consumes (all already in `decorator_tests`): `ScriptModel::new(Vec<Resp>) -> Arc<Self>`, `ScriptModel::calls() -> usize`, `Resp::{ErrFirst(ModelError), Ok, OkThenErr(ModelError)}`, `zero_backoff() -> RetryPolicy`, `drain(BoxStream<...>) -> Vec<Result<ModelEvent, ModelError>>`, `RetryingModel::shared(Arc<M>, RetryPolicy)`.
- Produces: nothing.

Note `Resp::Ok` yields `TokenDelta("ok")` then `Finish(Stop)`.

- [ ] **Step 1: Add the test**

Append inside `mod decorator_tests`, directly after `cancellation_aborts_backoff_promptly` (which ends at line 561):

```rust
    /// Contract pin: once content has started, cancellation ends the stream
    /// and `Finish` is withheld.
    ///
    /// Deterministic despite both branches being ready: the forwarding loop's
    /// `select!` is `biased` with `cancelled()` first, and
    /// `WaitForCancellationFuture` is `Ready` on its first poll for an
    /// already-cancelled token — so the token always beats the ready item.
    #[tokio::test]
    async fn cancel_after_content_ends_stream_without_finish() {
        let model = ScriptModel::new(vec![Resp::Ok]);
        let cancel = CancellationToken::new();
        let retrying = RetryingModel::shared(Arc::clone(&model), zero_backoff());
        let mut stream = retrying
            .invoke(ModelRequest::new(), cancel.clone())
            .await
            .unwrap();

        let first = stream.next().await.unwrap().unwrap();
        assert!(matches!(first, ModelEvent::TokenDelta { ref text } if text == "ok"));

        cancel.cancel();

        let rest = drain(stream).await;
        assert!(
            rest.is_empty(),
            "cancellation must end the stream, got {rest:?}"
        );
        assert_eq!(model.calls(), 1, "no retry after content started");
    }
```

- [ ] **Step 2: Format, then run it**

Run:

```bash
cargo fmt --all
cargo test -p paigasus-helikon-runtime-tokio --lib decorator_tests::cancel_after_content 2>&1 | tail -15
```

Expected: PASS. This proves nothing yet — Step 3 is what gives it meaning.

- [ ] **Step 3: MUTATION CHECK — falsify the test**

In `crates/paigasus-helikon-runtime-tokio/src/retry.rs`, the forwarding loop reads:

```rust
                        loop {
                            let next = tokio::select! {
                                biased;
                                () = cancel.cancelled() => return,
                                x = model_stream.next() => x,
                            };
```

Temporarily delete the `() = cancel.cancelled() => return,` line so the loop no longer observes the token.

Run:

```bash
cargo test -p paigasus-helikon-runtime-tokio --lib decorator_tests::cancel_after_content 2>&1 | tail -20
```

Expected: **FAIL** — `rest` now contains `Finish(Stop)`, tripping `cancellation must end the stream, got [...]`.

Record the observed output. If it PASSES, the test is worthless as written — fix the test, not the mutation.

- [ ] **Step 4: Revert the mutation**

Restore the deleted `() = cancel.cancelled() => return,` line.

Run:

```bash
git diff --stat crates/paigasus-helikon-runtime-tokio/src/retry.rs
```

Expected: the diff shows **only** the added test, no change inside `invoke`. If `invoke` still shows a change, the revert was incomplete — fix it now.

- [ ] **Step 5: Re-run to confirm green after revert**

Run: `cargo test -p paigasus-helikon-runtime-tokio --lib decorator_tests 2>&1 | tail -15`

Expected: all `decorator_tests` pass.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-runtime-tokio/src/retry.rs
git commit -m "test(runtime-tokio): SMA-563 pin cancellation after content starts"
```

Put the Step 3 mutation output in the commit body as evidence the test bites.

---

### Task 5: Cover the untested backoff-cancellation path and fix the misleading comment

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/src/retry.rs` — the comment at line 558, and a new test in `mod decorator_tests`

**Interfaces:**
- Consumes: the same `decorator_tests` helpers as Task 4.
- Produces: nothing.

**Why this task exists.** `cancellation_aborts_backoff_promptly` (lines 544-561) does not test backoff. It fires the token *before* the first poll; the first-item peek `select!` (lines 211-215) is `biased` with `cancelled()` first, so it returns before `model_stream.next()` is ever polled — the scripted `Resp::ErrFirst` is never observed and `policy.next_delay`/`backoff` are never reached. Consequently `backoff()`'s cancelled branch (line 163) and both `if !backoff(…) { return; }` sites (lines 196, 241) have **zero** coverage. The existing test is still valid for what it actually does (a pre-fired token yields an empty stream after exactly one `invoke`); only its comment lies.

- [ ] **Step 1: Correct the false comment**

At `crates/paigasus-helikon-runtime-tokio/src/retry.rs:558` the line reads:

```rust
        // First poll: invoke #1 fails, enters backoff, cancellation wins → stream ends.
```

Replace it with:

```rust
        // First poll: invoke #1 runs, then the peek `select!` — `biased` with
        // `cancelled()` first — returns immediately, so the scripted error is
        // never observed and backoff is never entered. This test therefore
        // covers the PRE-FIRED path only; `cancel_during_backoff_ends_stream`
        // covers the actual backoff sleep.
```

Also rename nothing — the test's name is now covered by the comment's correction, and renaming it would churn a test other readers may reference.

- [ ] **Step 2: Add the backoff-cancellation test**

Append inside `mod decorator_tests`, after `cancellation_aborts_backoff_promptly`:

```rust
    /// The one `RetryingModel` cancellation path with real latency: parked in
    /// `backoff()` between attempts. Untested before SMA-563 — the test named
    /// `cancellation_aborts_backoff_promptly` never reaches backoff at all.
    ///
    /// `start_paused` makes tokio's clock virtual, so the hour-long delay costs
    /// no wall-clock time; the drain is spawned so the token can be fired from
    /// this task while the stream is parked.
    #[tokio::test(start_paused = true)]
    async fn cancel_during_backoff_ends_stream() {
        let model = ScriptModel::new(vec![Resp::ErrFirst(ModelError::Unavailable), Resp::Ok]);
        let policy = RetryPolicy::new()
            .base_delay(Duration::from_secs(3600))
            .jitter(false);
        let cancel = CancellationToken::new();
        let retrying = RetryingModel::shared(Arc::clone(&model), policy);
        let stream = retrying
            .invoke(ModelRequest::new(), cancel.clone())
            .await
            .unwrap();

        let handle = tokio::spawn(drain(stream));

        // Let the spawned task run: attempt #1 fails and parks in `backoff()`.
        // `yield_now` under a paused clock hands over without advancing time,
        // so the 1-hour sleep cannot elapse and mask the cancellation.
        tokio::task::yield_now().await;
        cancel.cancel();

        let items = handle.await.unwrap();
        assert!(
            items.is_empty(),
            "cancelling during backoff must end the stream with no items, got {items:?}"
        );
        assert_eq!(
            model.calls(),
            1,
            "the retry must not fire after cancellation"
        );
    }
```

- [ ] **Step 3: Format, then run it**

Run:

```bash
cargo fmt --all
cargo test -p paigasus-helikon-runtime-tokio --lib decorator_tests::cancel_during_backoff 2>&1 | tail -20
```

Expected: PASS, and fast (virtual clock).

If it **hangs**, the drain never parked in backoff before the cancel — insert a second `tokio::task::yield_now().await;` before `cancel.cancel()` and re-run. Do not "fix" a hang by shortening `base_delay`: a short delay would let the sleep complete and the test would pass for the wrong reason.

- [ ] **Step 4: MUTATION CHECK — falsify the test**

`backoff()` at lines 160-166 reads:

```rust
async fn backoff(cancel: &CancellationToken, d: Duration) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(d) => true,
    }
}
```

Temporarily replace its body with an unconditional sleep:

```rust
async fn backoff(_cancel: &CancellationToken, d: Duration) -> bool {
    tokio::time::sleep(d).await;
    true
}
```

Run:

```bash
cargo test -p paigasus-helikon-runtime-tokio --lib decorator_tests::cancel_during_backoff 2>&1 | tail -20
```

Expected: **FAIL — on the `calls()` assertion, reporting `2` where `1` is expected.**

Be precise about *which* assertion trips, because it is not the obvious one. With the cancel arm gone, the runtime goes idle on the paused clock, auto-advance fires the 1-hour timer, and attempt #2 runs — so `calls()` becomes 2. But attempt #2's *first-item peek* is a different `select!` that still has its `biased` `cancelled()` arm, and the token is fired by then, so it returns immediately and `items` stays **empty**. The `items.is_empty()` assertion therefore still passes; only `assert_eq!(model.calls(), 1, …)` catches the mutation.

That is why the test asserts both. Record the observed output. If `items` is non-empty as well, that is fine — it means the timing differed — but a run where **both** assertions pass means the test does not bite and must be fixed.

- [ ] **Step 5: Revert the mutation and verify**

Restore the original `backoff()` body.

Run:

```bash
git diff crates/paigasus-helikon-runtime-tokio/src/retry.rs | grep -E "^[+-]" | grep -v "^[+-][+-]" | head -40
```

Expected: the diff shows only the corrected comment and the new test — **no change to `backoff()`**.

- [ ] **Step 6: Run the full crate**

Run: `cargo test -p paigasus-helikon-runtime-tokio 2>&1 | tail -20`

Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-runtime-tokio/src/retry.rs
git commit -m "test(runtime-tokio): SMA-563 cover cancellation during retry backoff"
```

Put the Step 4 mutation output in the commit body.

---

### Task 6: Update the two documentation surfaces

**Files:**
- Modify: `crates/paigasus-helikon-evals/README.md` — the `## MockModel and ScriptFile` section
- Modify: `docs/book/src/concepts/observability-evaluation.md` — the `### MockModel and ScriptFile — deterministic replay` subsection

**Interfaces:** none.

`crates/paigasus-helikon-evals/src/lib.rs` does **not** `include_str!` its README — only the facade does — so the new README prose carries no doctest risk and needs no ` ```ignore ` fencing.

- [ ] **Step 1: Update the crate README**

In `crates/paigasus-helikon-evals/README.md`, the `## MockModel and ScriptFile` section's single paragraph ends with:

> ...so one file can drive a whole dataset.

Append a new paragraph directly after it:

```markdown
`MockModel` honors its `CancellationToken` as the `Model::invoke` contract requires: the stream observes the token at each poll and ends on the first fired observation, without emitting `Finish`. The token is observed, not awaited — a consumer that stops polling never learns the stream has ended. An `invoke` called with an already-cancelled token yields an empty stream but still consumes its script, so "one script per `invoke`" holds regardless of cancellation timing.
```

- [ ] **Step 2: Update the mdBook page**

In `docs/book/src/concepts/observability-evaluation.md`, the `### MockModel and ScriptFile — deterministic replay` subsection's second paragraph ends with:

> ...`.agent()`/`.shared_agent()` remain for genuinely stateless or live agents.

Append a new paragraph directly after it:

```markdown
`MockModel` honors its `CancellationToken` as the `Model::invoke` contract
requires: the stream observes the token at each poll and ends on the first
fired observation, without emitting `Finish`. The token is *observed*, not
awaited — a consumer that stops polling never learns the stream has ended,
which is all a synchronous scripted stream can offer. An `invoke` called with
an already-cancelled token yields an empty stream but still consumes its
script, so "one script per `invoke`" holds regardless of cancellation timing.

Cancelling part-way through a tool call leaves a truncated `args_delta`, which
core's turn accumulator then fails to parse — matching what a real provider
does when a connection drops mid-call. That is the point: it is now
reproducible deterministically.
```

- [ ] **Step 3: Verify the book still builds**

Run:

```bash
mdbook build docs/book 2>&1 | tail -20
```

Expected: clean. `[output.linkcheck] warning-policy = "error"`, so a broken link fails the build. If `mdbook` is not installed, say so and skip this step — CI's `book-build` job is a required check and will catch it.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-evals/README.md docs/book/src/concepts/observability-evaluation.md
git commit -m "docs(evals): SMA-563 document MockModel cancellation semantics"
```

---

### Task 7: Full local CI gate

**Files:** none modified (fix-forward only if a gate trips).

- [ ] **Step 1: Format check**

Run: `cargo fmt --all -- --check`

Expected: no output, exit 0.

- [ ] **Step 2: Clippy, the exact CI invocation**

Run:

```bash
cargo clippy --workspace --all-features --all-targets -- -D warnings 2>&1 | tail -30
```

Expected: no warnings. `--all-targets` is what compiles the `langfuse_tracing` example, so this is the gate that covers Task 3.

- [ ] **Step 3: The full test gate, exactly as CI runs it**

Run:

```bash
cargo test --workspace --all-features 2>&1 | tail -40
```

Expected: all green. Run the **workspace** form, not per-crate — a per-crate run resolves a different feature union and has masked real failures in this repo before.

Known environment caveat: on macOS, `paigasus-helikon-providers-bedrock` produces a large block of `NATIVE_ROOTS` failures that track the **checkout path**, not the code. This session already runs from a worktree, which is the configuration that passes; if those failures appear anyway, confirm they also reproduce on `main` before treating them as caused by this branch.

- [ ] **Step 4: Docs gate**

Run:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps 2>&1 | tail -20
```

Expected: clean apart from the known, accepted `paigasus-helikon` lib/bin filename-collision warning documented in `CLAUDE.md`.

- [ ] **Step 5: Report**

Report each gate's actual result. If any gate fails, fix it and re-run that gate before reporting completion. Do not report "all gates pass" without having seen each command's output.

---

## Verification Summary

The ticket's two acceptance criteria and where they are discharged:

| Acceptance criterion | Discharged by |
|---|---|
| `MockModel` terminates its stream when the token fires, without emitting `Finish` | Task 2 |
| A test asserts it, **verified to fail against the current implementation** | Task 1 Step 4 — the failure output is captured and committed |

The ticket's note ("`RetryingModel` … worth pinning that with a test in the same change") is discharged by Tasks 4 and 5, both mutation-checked rather than assumed meaningful.
