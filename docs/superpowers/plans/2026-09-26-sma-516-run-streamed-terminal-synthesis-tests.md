# SMA-516 run_streamed terminal-synthesis tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the last step of `TemporalRunner::run_streamed` (FailureSlot wiring, terminal synthesis, stream construction) under fast unit tests, prove each test with a mutation, and add one live streamed test.

**Architecture:** Extract the assembly into a private pure function `into_streaming(events, result) -> RunResultStreaming` in `crates/paigasus-helikon-runtime-temporal/src/runner.rs`. `run_streamed` calls only `run_inner` and `into_streaming`. Async unit tests drive `RunResultStreaming::collect()` on the helper's output. One env-gated live test calls `run_streamed` against a real Temporal dev server.

**Tech Stack:** Rust (MSRV 1.94), tokio (`#[tokio::test]`), futures-util, the `paigasus-helikon-core` runner types, Temporal CLI dev server for the live test.

**Spec:** `docs/superpowers/specs/2026-09-26-sma-516-run-streamed-terminal-synthesis-tests-design.md`

## Global Constraints

- No public API change. `into_streaming` is private (no `pub`).
- The rustdoc on `run_streamed` stays on `run_streamed`. It must not contain an intra-doc link to `into_streaming` or any other private item (`rustdoc::private_intra_doc_links` fails the required `docs` gate).
- No version bumps in any `Cargo.toml`. No CHANGELOG edits (release-plz owns them).
- No book (`docs/book/`) or README edits. This is a deliberate decision in the spec.
- Commit messages: `<type>(<scope>): SMA-516 <lowercase subject>`, scope `runtime-temporal`, types `refactor` / `test`. End every commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Stage files with explicit paths. Never `git add -A` or `git add .` (`.env` and `.claude` are not gitignored).
- Never run `git checkout`, `git switch`, `git reset`, `git stash`, or `git rebase`. Do not move HEAD or the branch.
- Work from the worktree root `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-516`. Every file path in Write/Edit calls must start with that prefix.
- Run every cargo command in the foreground and wait for it to finish. Do not background a cargo run and end your turn.

## Review Focus

1. A real agent-failure log already ends in `RunFailed` → no second terminal, and `collect()` still gives the typed error (T1 pins this, including two guard mutations that the SMA-515 tests miss).
2. An `AgentFailed` status with an empty log (`unknown_agent_outcome`) → one synthetic frame and the typed error (T2).
3. Cancel/timeout/infra → the slot stays empty, so `collect()` is `RunError::Other` with the exact frame text, not `RunError::Agent` (T3–T5 plus the "set slot for every Err" mutation).
4. A drained stream collects as `Ok` for any input → tests must use a fresh handle for `collect()` (enforced by the two-handle pattern in Task 1).
5. The `run_inner` → `into_streaming` join, which no unit test reaches (Task 3's live test).

---

### Task 1: Extract `into_streaming` and pin it with seven unit tests

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/runner.rs` (the `run_streamed` impl at about lines 240–280; new helper after `append_synthetic_terminal` at about line 324; the `mod tests` block from about line 364)

**Interfaces:**
- Consumes (existing, same file): `fn synthetic_terminal_event(result: &Result<RunResult, RunError>) -> Option<AgentEvent>`, `fn append_synthetic_terminal(events: &mut Vec<AgentEvent>, event: Option<AgentEvent>)`.
- Produces: `fn into_streaming(events: Vec<AgentEvent>, result: Result<RunResult, RunError>) -> RunResultStreaming` (private, module level). Task 2 mutates its body.

Facts the test code depends on (verified):
- `RunError::Other` is `#[error(transparent)]`, so `e.to_string()` is the inner `anyhow` message.
- `AgentError::Other` is `#[error(transparent)]`. `AgentError::MaxTurnsExceeded(3)` displays as `"max turns (3) exceeded"`.
- `RunInterrupt::Cancelled.terminal_message() == "run cancelled"`, `RunInterrupt::TimedOut.terminal_message() == "run timed out"`.
- `collect()` reads the slot only after it saw a `RunFailed`. With no `RunFailed` it returns `Ok`. With a `RunFailed` and an empty slot it returns `RunError::Other(anyhow!(<frame text>))`.
- `AgentError`, `RunError`, `AgentEvent` have no `PartialEq`. Use `match`/`matches!` with literal payloads.
- `collect()` consumes the handle and drops the events on `Err`. Therefore each `Err` test builds the input twice and calls `into_streaming` twice: one handle for the frames, one for `collect()`. **Never drain `events` and then `collect()` the same handle** — the second call sees an empty stream and returns `Ok` for any input.

- [ ] **Step 1: Write the failing tests**

In `mod tests`, change the import line

```rust
    use paigasus_helikon_core::{AgentError, TokenUsage};
```

to

```rust
    use paigasus_helikon_core::{AgentError, ContentPart, Item, TokenUsage};
```

Then append these items at the end of `mod tests` (before its closing `}`):

```rust
    // ---- SMA-516: the assembled `RunResultStreaming` ----------------------
    //
    // `collect()` consumes the handle and drops its events on `Err`, and
    // `RunError` is not `Clone`. So every `Err` case builds its input twice:
    // one handle is drained for the frames, a fresh one is collected. Draining
    // and then collecting the SAME handle would see an empty stream and return
    // `Ok` for any input — a test that can never fail.

    type Input = (Vec<AgentEvent>, Result<RunResult, RunError>);

    /// Drain a handle's raw frames.
    async fn frames(streaming: RunResultStreaming) -> Vec<AgentEvent> {
        futures_util::StreamExt::collect::<Vec<_>>(streaming.events).await
    }

    /// Exactly one terminal, and it is the last frame.
    fn assert_one_terminal_last(frames: &[AgentEvent]) {
        assert_eq!(
            frames.iter().filter(|e| e.is_terminal()).count(),
            1,
            "exactly one terminal frame: {frames:?}"
        );
        assert!(
            frames.last().is_some_and(AgentEvent::is_terminal),
            "the terminal must be the last frame: {frames:?}"
        );
    }

    const UNKNOWN_AGENT: &str = "no agent named 'x' is registered on this worker";
    const INFRA: &str = "temporal workflow failed: boom";

    /// T1: the realistic agent failure — the driver already wrote `RunFailed`.
    fn agent_failure_with_terminal() -> Input {
        (
            vec![
                AgentEvent::TurnStarted { turn: 0 },
                AgentEvent::RunFailed {
                    error: AgentError::MaxTurnsExceeded(3).to_string(),
                },
            ],
            Err(RunError::Agent(AgentError::MaxTurnsExceeded(3))),
        )
    }

    /// T2: reachable today — `workflow.rs`'s `unknown_agent_outcome` returns
    /// `AgentFailed` with NO events (so do the driver's `Handoff` and
    /// unknown-`NextAction` arms). Both the synthetic frame and the slot are
    /// load-bearing here.
    fn agent_failure_without_terminal() -> Input {
        (
            Vec::new(),
            Err(RunError::Agent(AgentError::Other(anyhow::anyhow!(
                UNKNOWN_AGENT
            )))),
        )
    }

    fn interrupted(err: RunError) -> Input {
        (vec![AgentEvent::TurnStarted { turn: 0 }], Err(err))
    }

    /// T5: an infrastructure failure — `run_inner` returns no events.
    fn infra_failure() -> Input {
        (
            Vec::new(),
            Err(RunError::Other(anyhow::anyhow!(INFRA))),
        )
    }

    /// T1. Fails if the slot is not set, if `new` replaces `with_failure`, if
    /// the guard is bypassed, or if the guard tests only `RunCompleted` or reads
    /// `events.first()` — the last two survive every SMA-515 guard test.
    #[tokio::test]
    async fn agent_failure_with_its_own_terminal_collects_the_typed_error() {
        let (events, result) = agent_failure_with_terminal();
        let frames = frames(into_streaming(events, result)).await;
        assert_eq!(frames.len(), 2, "the log must pass through unchanged: {frames:?}");
        assert!(matches!(frames[0], AgentEvent::TurnStarted { turn: 0 }));
        assert_one_terminal_last(&frames);

        let (events, result) = agent_failure_with_terminal();
        match into_streaming(events, result).collect().await {
            Err(RunError::Agent(AgentError::MaxTurnsExceeded(3))) => {}
            Err(other) => panic!("expected the typed RunError::Agent(MaxTurnsExceeded(3)), got {other:?}"),
            Ok(_) => panic!("a failed run must never collect as Ok"),
        }
    }

    /// T2.
    #[tokio::test]
    async fn agent_failure_without_a_terminal_gets_one_and_the_typed_error() {
        let (events, result) = agent_failure_without_terminal();
        let frames = frames(into_streaming(events, result)).await;
        assert_eq!(frames.len(), 1, "one synthetic frame: {frames:?}");
        assert_one_terminal_last(&frames);
        match &frames[0] {
            AgentEvent::RunFailed { error } => assert_eq!(error, UNKNOWN_AGENT),
            other => panic!("the synthetic terminal must be RunFailed, got {other:?}"),
        }

        let (events, result) = agent_failure_without_terminal();
        match into_streaming(events, result).collect().await {
            Err(RunError::Agent(AgentError::Other(e))) => assert_eq!(e.to_string(), UNKNOWN_AGENT),
            Err(other) => panic!("expected the typed RunError::Agent(Other(..)), got {other:?}"),
            Ok(_) => panic!("a failed run must never collect as Ok"),
        }
    }

    /// Shared body for T3–T5: the slot must stay EMPTY, so `collect()` returns
    /// `RunError::Other` carrying the frame text — the same as `TokioRunner`
    /// for an interrupt. The variant is pinned on purpose: a change to it must
    /// update these tests in the same PR.
    async fn assert_untyped_failure(input: fn() -> Input, prefix_len: usize, text: &str) {
        let (events, result) = input();
        let frames = frames(into_streaming(events, result)).await;
        assert_eq!(frames.len(), prefix_len + 1, "prefix plus one synthetic frame: {frames:?}");
        if prefix_len == 1 {
            assert!(
                matches!(frames[0], AgentEvent::TurnStarted { turn: 0 }),
                "the input prefix must stay in place: {frames:?}"
            );
        }
        assert_one_terminal_last(&frames);
        match frames.last() {
            Some(AgentEvent::RunFailed { error }) => assert_eq!(error, text),
            other => panic!("the synthetic terminal must be RunFailed, got {other:?}"),
        }

        let (events, result) = input();
        match into_streaming(events, result).collect().await {
            Err(RunError::Other(e)) => assert_eq!(e.to_string(), text),
            Err(other) => panic!("the slot must stay empty, so expected RunError::Other, got {other:?}"),
            Ok(_) => panic!("a failed run must never collect as Ok"),
        }
    }

    /// T3.
    #[tokio::test]
    async fn cancelled_run_collects_as_err_with_the_canonical_text() {
        assert_untyped_failure(|| interrupted(RunError::Cancelled), 1, "run cancelled").await;
    }

    /// T4.
    #[tokio::test]
    async fn timed_out_run_collects_as_err_with_the_canonical_text() {
        assert_untyped_failure(
            || interrupted(RunError::Timeout),
            1,
            RunInterrupt::TimedOut.terminal_message(),
        )
        .await;
    }

    /// T5.
    #[tokio::test]
    async fn infra_failure_collects_as_err_with_its_own_text() {
        assert_untyped_failure(infra_failure, 0, INFRA).await;
    }

    /// T6.
    #[tokio::test]
    async fn successful_run_gains_no_frame_and_collects_ok() {
        let events = vec![
            AgentEvent::MessageOutput {
                item: Item::AssistantMessage {
                    content: vec![ContentPart::Text { text: "hi".to_owned() }],
                    agent: None,
                },
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        let collected = into_streaming(events, Ok(RunResult::default()))
            .collect()
            .await
            .unwrap_or_else(|e| panic!("a completed run must collect as Ok, got {e:?}"));
        assert_eq!(collected.final_output, "hi");
        assert_eq!(collected.events.len(), 2, "no frame appended: {:?}", collected.events);
        assert_one_terminal_last(&collected.events);
        assert!(matches!(collected.events[1], AgentEvent::RunCompleted { .. }));
    }

    /// T7 — decided 2026-09-26: the event log wins. A log ending in
    /// `RunCompleted` gets no second terminal even when the result is `Err`, so
    /// `collect()` returns `Ok` while `run()` would return `Err`. `run()` and
    /// `collect()` differ in `Ok` versus `Err` only in this state. It is
    /// unreachable today: `DurableDriver::interrupt` returns a `Done` outcome
    /// unchanged, pinned by `driver.rs`'s `terminal_wins_over_late_interrupt`.
    #[tokio::test]
    async fn log_ending_in_run_completed_wins_over_an_err_result() {
        let events = vec![
            AgentEvent::TurnStarted { turn: 0 },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        let collected = into_streaming(events, Err(RunError::Cancelled))
            .collect()
            .await
            .unwrap_or_else(|e| panic!("the log's RunCompleted must win, got {e:?}"));
        assert_eq!(collected.events.len(), 2, "no second terminal: {:?}", collected.events);
        assert_one_terminal_last(&collected.events);
        assert!(matches!(collected.events[1], AgentEvent::RunCompleted { .. }));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-temporal --lib runner::tests 2>&1 | tail -20`
Expected: compile error `cannot find function `into_streaming` in this scope`.

- [ ] **Step 3: Extract the helper and make `run_streamed` a thin caller**

Replace the body of `run_streamed` (from `let (mut events, result) = …` to the closing `))` of `Ok(RunResultStreaming::with_failure(…))`) with:

```rust
        let (events, result) = self.run_inner(agent, ctx, input, config).await?;
        Ok(into_streaming(events, result))
```

Replace the second paragraph of the `run_streamed` rustdoc (the one that starts `/// On a failed run the terminal error is wired into …`) with:

```rust
    /// On a failed run a terminal `RunFailed` event is present whenever the
    /// durable log did not already end in a terminal. An agent failure is also
    /// wired into the returned handle's [`FailureSlot`], so
    /// [`RunResultStreaming::collect`] surfaces the typed [`RunError::Agent`] —
    /// matching `TokioRunner`. Cancellation, timeout and infrastructure
    /// failures leave the slot empty, so `collect` returns [`RunError::Other`]
    /// carrying the terminal frame's text, as `TokioRunner` does for an
    /// interrupt.
```

Keep the first rustdoc paragraph ("**Buffered, not live.** …") unchanged.

Insert this new function directly after `append_synthetic_terminal`:

```rust
/// Assemble the durable run's `(events, result)` into the handle
/// [`Runner::run_streamed`] returns. Pure, so the whole assembly is unit-tested
/// without a Temporal server (SMA-516).
fn into_streaming(
    mut events: Vec<AgentEvent>,
    result: Result<RunResult, RunError>,
) -> RunResultStreaming {
    let failure = FailureSlot::new();
    let terminal = synthetic_terminal_event(&result);
    // Move the structured error into the slot only for an agent failure;
    // the event itself was already rendered above.
    if let Err(RunError::Agent(err)) = result {
        failure.set(err);
    }

    // `collect()` only reads the failure slot once it observes a terminal
    // `RunFailed` in the stream. The durable event log already carries one
    // for `AgentFailed` runs; synthesize one for the terminal states that do
    // not (cancellation/timeout/infra), so a failed run never collects as
    // `Ok`. `append_synthetic_terminal` owns the guard that stops this from
    // appending a *second* terminal (SMA-422).
    append_synthetic_terminal(&mut events, terminal);

    RunResultStreaming::with_failure(stream::iter(events).boxed(), failure)
}
```

(`[`Runner::run_streamed`]` in a private item's doc is fine — the lint fires only for public docs that link to private items.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-temporal --lib runner::tests 2>&1 | tail -25`
Expected: all `runner::tests` pass, including the 7 new ones and the 9 existing ones (16 total).

- [ ] **Step 5: Format, lint, and doc-check**

Run:
```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-runtime-temporal --all-features --no-deps
```
Expected: no diff left by fmt that is not yours, clippy clean, doc clean. If clippy flags a test item, fix it.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-runtime-temporal/src/runner.rs
git commit -m "refactor(runtime-temporal): SMA-516 extract into_streaming and pin run_streamed's assembly

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Mutation-check every new test

No code is kept from this task. Each mutation is applied, tested, and reverted. The result is a record of test names that failed, which goes into the PR body.

**Files:**
- Temporarily modify: `crates/paigasus-helikon-runtime-temporal/src/runner.rs` (`into_streaming` and `append_synthetic_terminal` only)
- Create: `/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/2fd3a28d-84d5-4dff-b876-68b4a4d97fdc/scratchpad/sma-516-mutations.md` (the record; outside the repo)

**Interfaces:**
- Consumes: `into_streaming` from Task 1 and the seven tests T1–T7 with the names below.
- Produces: the mutation record.

Test name map:
- T1 `agent_failure_with_its_own_terminal_collects_the_typed_error`
- T2 `agent_failure_without_a_terminal_gets_one_and_the_typed_error`
- T3 `cancelled_run_collects_as_err_with_the_canonical_text`
- T4 `timed_out_run_collects_as_err_with_the_canonical_text`
- T5 `infra_failure_collects_as_err_with_its_own_text`
- T7 `log_ending_in_run_completed_wins_over_an_err_result`

For each mutation below:
1. Apply the edit with the Edit tool.
2. Run `cargo test -p paigasus-helikon-runtime-temporal --lib runner::tests 2>&1 | grep -E "^test |test result"`.
3. Confirm that **every** listed test shows `FAILED`. If one listed test passes, stop and report it — that is a finding, not something to work around.
4. Revert the edit with the Edit tool (exact inverse). Do not use `git checkout`/`git restore`.
5. Append one line to the record: mutation, tests that failed, whether it matches the table.

| # | Mutation | Must fail |
|---|---|---|
| M1 | In `into_streaming`, delete the line `failure.set(err);` (replace the `if let` body with `let _ = err;`) | T1, T2 |
| M2 | In `into_streaming`, `RunResultStreaming::with_failure(stream::iter(events).boxed(), failure)` → `{ let _ = failure; RunResultStreaming::new(stream::iter(events).boxed()) }` | T1, T2 |
| M3 | In `into_streaming`, replace the `if let Err(RunError::Agent(err)) = result { failure.set(err); }` block with `match result { Err(RunError::Agent(err)) => failure.set(err), Err(e) => failure.set(AgentError::Other(anyhow::Error::new(e))), Ok(_) => {} }` (add `AgentError` to the `use paigasus_helikon_core::{…}` list for this mutation only, and remove it on revert) | T3, T4, T5 |
| M4 | In `into_streaming`, delete the line `append_synthetic_terminal(&mut events, terminal);` (replace with `let _ = terminal;`) | T2, T3, T4, T5 |
| M5 | In `into_streaming`, `append_synthetic_terminal(&mut events, terminal);` → `append_synthetic_terminal(&mut events, { let _ = terminal; None });` | T2, T3, T4, T5 |
| M6 | In `into_streaming`, `append_synthetic_terminal(&mut events, terminal);` → `events.extend(terminal);` | T1, T7 |
| M7 | In `append_synthetic_terminal`, `events.last().is_some_and(AgentEvent::is_terminal)` → `events.last().is_some_and(\|e\| matches!(e, AgentEvent::RunCompleted { .. }))` | T1 |
| M8 | In `append_synthetic_terminal`, `events.last()` → `events.first()` | T1 |

- [ ] **Step 1: Run M1–M8** as described above, one at a time.
- [ ] **Step 2: Confirm the file is back to the Task 1 state**

Run: `git diff --stat` → expected: empty output (no uncommitted change to tracked files).
Run: `cargo test -p paigasus-helikon-runtime-temporal --lib runner::tests 2>&1 | tail -3` → expected: all pass.

- [ ] **Step 3: Report** the eight record lines in your final message. No commit in this task.

---

### Task 3: Live streamed test

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/tests/temporal_live.rs` (add one test directly after `model_failure_maps_to_typed_agent_error`, which ends at about line 843)

**Interfaces:**
- Consumes (existing in that file): `fn gate() -> Option<String>`, `async fn connect(addr: &str) -> Client`, `fn start_worker(addr, queue, agent, tool_start_to_close, model_start_to_close, tool_max_attempts)` returning a handle with `async fn stop(self)`, `struct FailingModel` (its error text contains `"connection lost"`). Imports already present: `Arc`, `Duration`, `LlmAgent`, `MemorySession`, `Session`, `RunConfig`, `RunContext`, `RunError`, `Runner`, `AgentInput`, `TemporalRunner`, `TemporalRunnerConfig`.
- Produces: `streamed_model_failure_collects_the_typed_agent_error`.

- [ ] **Step 1: Add the test**

```rust
/// Streamed twin of `model_failure_maps_to_typed_agent_error` (SMA-516). The
/// unit tests in `src/runner.rs` pin `into_streaming` in isolation; this is the
/// only test of the join between `run_inner` and `into_streaming`, i.e. of
/// `run_streamed` itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streamed_model_failure_collects_the_typed_agent_error() {
    let Some(addr) = gate() else {
        return;
    };
    let queue = format!("helikon-streamfail-{}", uuid::Uuid::new_v4());

    let agent = Arc::new(
        LlmAgent::builder::<()>()
            .name("failer")
            .model(FailingModel)
            .build(),
    );

    let worker = start_worker(
        addr.clone(),
        queue.clone(),
        Arc::clone(&agent),
        Duration::from_secs(5),
        Duration::from_secs(10),
        5,
    );

    let session: Arc<dyn Session> = Arc::new(MemorySession::new());
    let runner = TemporalRunner::new(
        connect(&addr).await,
        TemporalRunnerConfig::new(queue.clone()),
    );
    let collected = tokio::time::timeout(Duration::from_secs(60), async {
        runner
            .run_streamed(
                agent.as_ref(),
                RunContext::ephemeral(()).with_session(Arc::clone(&session)),
                AgentInput::from_user_text("go"),
                RunConfig::default(),
            )
            .await
            .expect("only a session read failure is an outer Err")
            .collect()
            .await
    })
    .await
    .expect("streamed model-failure run resolves within 60s");

    worker.stop().await;

    match collected {
        Err(RunError::Agent(agent_err)) => {
            let message = agent_err.to_string();
            assert!(
                message.contains("connection lost"),
                "the model error message must survive into the typed error: {message:?}"
            );
        }
        Err(other) => panic!("expected the typed RunError::Agent, got {other:?}"),
        Ok(_) => panic!("a failed run must never collect as Ok"),
    }
}
```

- [ ] **Step 2: Verify it compiles and loud-skips without a server**

Run: `cargo test -p paigasus-helikon-runtime-temporal --test temporal_live streamed_model_failure -- --nocapture 2>&1 | tail -8`
Expected: `SKIPPED: set TEMPORAL_TEST_SERVER=…` and `test result: ok. 1 passed`.

- [ ] **Step 3: Run it against a local dev server**

The `temporal` CLI is installed at `/opt/homebrew/bin/temporal`. Start the server in the background with the Bash tool's `run_in_background: true`:

```bash
temporal server start-dev --headless --port 7233
```

Wait until it answers: `temporal operator cluster health --address localhost:7233` prints `SERVING` (retry for up to 30 s). Then run in the foreground:

```bash
TEMPORAL_TEST_SERVER=localhost:7233 HELIKON_REQUIRE_TEMPORAL=1 \
  cargo test -p paigasus-helikon-runtime-temporal --test temporal_live 2>&1 | tail -20
```

Expected: every live test passes, including the new one. Then stop the dev server (kill its background process). If port 7233 is already in use, use another port in both commands. If the server cannot start, report that; CI's `temporal-it` job will still run the test on the PR.

- [ ] **Step 4: Mutation-check the live test**

In `crates/paigasus-helikon-runtime-temporal/src/runner.rs`, apply M1 from Task 2 (delete `failure.set(err);`). Re-run only the new test against the dev server:

```bash
TEMPORAL_TEST_SERVER=localhost:7233 HELIKON_REQUIRE_TEMPORAL=1 \
  cargo test -p paigasus-helikon-runtime-temporal --test temporal_live streamed_model_failure 2>&1 | tail -8
```

Expected: FAILED with `expected the typed RunError::Agent`. Revert M1 with the Edit tool and confirm `git diff --stat -- crates/paigasus-helikon-runtime-temporal/src/runner.rs` is empty. Skip this step only if Step 3 could not start a server, and say so.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-temporal/tests/temporal_live.rs
git commit -m "test(runtime-temporal): SMA-516 add a live streamed model-failure test

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Final verification (controller, after all tasks)

Run the exact CI gates from the worktree root:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Known local noise: on macOS, `cargo test --workspace --all-features` can fail ~48 bedrock tests with a NATIVE_ROOTS error that depends on the checkout path, not the code. A worktree under `.claude/worktrees/` is not the scratchpad; if those failures appear, confirm they are the bedrock NATIVE_ROOTS failures only, and note it.
