# SMA-531 Anthropic clean-EOF `Finish` flush — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Anthropic provider emit exactly one terminal `Finish` when its response body ends cleanly after `message_delta` but before `message_stop`, and never emit one on truncation-without-a-stop-reason, cancellation, or error.

**Architecture:** Add `MessageTranslator::finish()` — a clean-EOF flush of the buffered `stop_reason` — and call it from the driver's stream-exhausted arm only. `message_stop` keeps emitting inline; a new `terminal_emitted` flag makes "at most one terminal event per stream" an enforced invariant rather than an accident of `Option::take`.

**Tech Stack:** Rust 2024, `async-stream`, `eventsource-stream` 0.2, `wiremock` 0.6, `tokio` (test runtime), `tracing`.

**Spec:** `docs/superpowers/specs/2026-08-16-sma-531-anthropic-eof-finish-flush-design.md`

## Global Constraints

- **Working directory:** `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-531`. This is a git worktree — run everything from here, never `cd` to the main checkout.
- **Only one crate is touched:** `crates/paigasus-helikon-providers-anthropic`. Do not modify `paigasus-helikon-core`, the facade, or any other provider.
- **Do not edit any version number, `Cargo.toml`, `CHANGELOG.md`, or `release-plz.toml`.** release-plz handles the bump.
- **No mdBook edit and no README edit.** Deliberate — recorded in the spec's Scope boundaries. Do not "helpfully" add one.
- **Commit format:** `<type>(providers-anthropic): SMA-531 <lowercase subject>`. The local `commit-msg` hook runs `convco check` and will reject anything else. `providers-anthropic` is a valid scope in `.versionrc`.
- **Never `git add -A` or `git add .`** — `.env` and `.claude/` are untracked but *not* gitignored in this repo. Always `git add <explicit paths>`, then verify with `git show --stat`.
- **Run `cargo fmt --all` before every commit.** The `pre-commit` hook is a deliberate no-op, so nothing catches formatting until push time.
- **All new fixtures are LF-only.** `.gitattributes:3` already pins `crates/paigasus-helikon-providers-anthropic/tests/fixtures/*.txt text eol=lf`; do not add a `.gitattributes` entry.
- **Every new fixture must end with a blank line (`\n\n`)** so its final SSE event dispatches — *except* `body_cut_inside_message_stop.txt` in Task 3, where the missing terminator is the entire point.
- **Work synchronously and in the foreground.** Do not background `cargo test`/`cargo build` and end your turn; run each command to completion and read its output before proceeding.
- **MSRV is 1.94.** Do not use newer language features.

## File Structure

| File | Status | Responsibility |
| --- | --- | --- |
| `crates/paigasus-helikon-providers-anthropic/src/stream.rs` | Modify | `MessageTranslator`: add `terminal_emitted` field + `finish()`; guard the `MessageStop` arm. Unit tests live in its `#[cfg(test)] mod tests`. |
| `crates/paigasus-helikon-providers-anthropic/src/model.rs` | Modify | Driver: call `translator.finish()` from the stream-exhausted (`None`) arm only. |
| `crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs` | Modify | Integration tests + the shared `assert_exactly_one_terminal_finish` helper. |
| `crates/paigasus-helikon-providers-anthropic/tests/cancellation.rs` | Create | Cancellation coverage — the crate currently has none. |
| `crates/paigasus-helikon-providers-anthropic/tests/fixtures/eof_after_message_delta.txt` | Create | Clean body end after `message_delta`. |
| `crates/paigasus-helikon-providers-anthropic/tests/fixtures/body_cut_inside_message_stop.txt` | Create | Byte-level cut inside the `message_stop` event. |
| `crates/paigasus-helikon-providers-anthropic/tests/fixtures/eof_mid_content_block.txt` | Create | Clean body end with no stop reason ever observed. |
| `crates/paigasus-helikon-providers-anthropic/tests/fixtures/error_after_message_delta.txt` | Create | In-band `error` arriving *after* a stop reason was buffered. |

**Task ordering rationale.** Task 1 installs the regression net before any behaviour changes. Task 2 lands the flush and is knowingly incomplete — it introduces the double-emit hole the spec documents. Task 3's test is therefore genuinely red-first against Task 2's tree, and against `main`. Tasks 4–6 are additive coverage.

---

### Task 1: Backfill the no-regression net on the five existing fixture tests

The spec's "existing fixtures keep their exact behaviour" claim is currently unenforced: `messages_streaming.rs` asserts positionally (`oks[0]`..`oks[4]`) but never asserts the length, and one test asserts only `oks.last()`. A regression that appends a second `Finish` passes both unchanged. Install the net **before** changing any behaviour.

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `fn assert_exactly_one_terminal_finish(oks: &[ModelEvent])` — a private test helper in `messages_streaming.rs`, used by Tasks 2 and 4.

- [ ] **Step 1: Add the shared helper**

Insert immediately after the existing `run_stream` function (after line 54, before `#[tokio::test] async fn text_only_stream_emits_usage_token_deltas_usage_finish`):

```rust
/// Assert the sequence contains exactly one `Finish` and that it is the final
/// event — the core contract at `paigasus_helikon_core::Model::invoke`
/// ("`Finish` is the terminal event; nothing follows it").
///
/// Applied to every well-formed fixture so a regression that drops, doubles,
/// or misplaces the terminal event cannot pass silently.
fn assert_exactly_one_terminal_finish(oks: &[ModelEvent]) {
    let finishes = oks
        .iter()
        .filter(|e| matches!(e, ModelEvent::Finish { .. }))
        .count();
    assert_eq!(finishes, 1, "expected exactly one Finish, got {oks:#?}");
    assert!(
        matches!(oks.last(), Some(ModelEvent::Finish { .. })),
        "Finish must be the last event, got {oks:#?}"
    );
}
```

- [ ] **Step 2: Pin the length and terminal shape in `text_only_stream_emits_usage_token_deltas_usage_finish`**

Replace the final assertion (currently the single `assert!(matches!(&oks[4], ...))` line) with:

```rust
    assert!(matches!(&oks[4], ModelEvent::Finish { reason } if *reason == FinishReason::Stop));
    assert_eq!(
        oks.len(),
        5,
        "well-formed stream must emit exactly these five events, got {oks:#?}"
    );
    assert_exactly_one_terminal_finish(&oks);
```

- [ ] **Step 3: Apply the helper to `parallel_tool_use_stream_emits_two_tool_call_deltas`**

Replace its closing `assert!(matches!(oks.last().unwrap(), ModelEvent::Finish { reason: FinishReason::ToolCalls },));` with:

```rust
    assert!(matches!(
        oks.last().unwrap(),
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls
        },
    ));
    assert_exactly_one_terminal_finish(&oks);
```

- [ ] **Step 4: Apply the helper to `thinking_stream_emits_reasoning_delta_before_text_delta`**

Append as the last statement of the test body (after the `assert!(first_reasoning < first_text, ...)` block):

```rust
    assert_exactly_one_terminal_finish(&oks);
```

- [ ] **Step 5: Assert the error path emits no `Finish` in `stream_error_overloaded_terminates_with_unavailable`**

Replace the whole body after `let events = run_stream(&server, fixture).await;` with:

```rust
    assert!(
        !events
            .iter()
            .any(|r| matches!(r, Ok(ModelEvent::Finish { .. }))),
        "an error stream must emit no Finish, got {events:#?}"
    );
    let last = events.into_iter().last().unwrap();
    assert!(matches!(last, Err(ModelError::Unavailable)));
```

- [ ] **Step 6: Apply the helper to both turns of `multi_turn_tool_use_continuation`**

After the existing turn-1 `assert!(matches!(events1.last().unwrap(), ...))` block add:

```rust
    assert_exactly_one_terminal_finish(&events1);
```

After the turn-2 `assert!(matches!(events2.last().unwrap(), ...))` block add:

```rust
    assert_exactly_one_terminal_finish(&events2);
```

- [ ] **Step 7: Run the suite — everything must still pass**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test messages_streaming`

Expected: PASS, 5 tests. If any fails, the net has caught a *pre-existing* discrepancy — stop and report it rather than loosening the assertion.

- [ ] **Step 8: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs
git commit -m "test(providers-anthropic): SMA-531 pin one-terminal-finish on existing fixtures"
git show --stat
```

Confirm `git show --stat` lists exactly one file.

---

### Task 2: Add the clean-EOF flush

**Files:**
- Create: `crates/paigasus-helikon-providers-anthropic/tests/fixtures/eof_after_message_delta.txt`
- Create: `crates/paigasus-helikon-providers-anthropic/tests/fixtures/body_cut_inside_message_stop.txt`
- Modify: `crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/stream.rs`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/model.rs:112-113`

**Interfaces:**
- Consumes: `assert_exactly_one_terminal_finish(&[ModelEvent])` from Task 1.
- Produces: `MessageTranslator::finish(&mut self) -> Option<Result<ModelEvent, ModelError>>` — `pub(crate)`, used by `model.rs` and by Tasks 3 and 5.

> **This task knowingly lands an incomplete guarantee.** The `finish()` written here has no `terminal_emitted` guard, so a second `message_delta` arriving after `message_stop` will produce a second `Finish`. That hole is closed in Task 3, whose test must be able to go red against this task's tree. Do not pre-empt it.

- [ ] **Step 1: Create `eof_after_message_delta.txt`**

`text_only.txt` with the `message_stop` event removed. The file **must** end with a blank line so `message_delta` dispatches.

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"usage":{"input_tokens":12,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}

```

- [ ] **Step 2: Verify the terminator byte-for-byte**

Run: `tail -c 20 crates/paigasus-helikon-providers-anthropic/tests/fixtures/eof_after_message_delta.txt | xxd`

Expected: the output ends `0a0a` (two newlines). If it ends with a single `0a`, the `message_delta` event never dispatches, `stop_reason` is never buffered, and Step 8's test will stay red *after* the fix — the exact trap the spec warns about. Fix the file, do not "fix" the code.

- [ ] **Step 3: Create `body_cut_inside_message_stop.txt`**

`text_only.txt` cut mid-line inside the final event — what a real byte-level truncation looks like. This file **must not** end with a newline.

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"usage":{"input_tokens":12,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}

event: message_stop
data: {"type":"messa
```

- [ ] **Step 4: Verify the cut**

Run: `tail -c 24 crates/paigasus-helikon-providers-anthropic/tests/fixtures/body_cut_inside_message_stop.txt | xxd`

Expected: ends with the bytes of `{"type":"messa` and **no** trailing `0a`. The partial event is discarded by the SSE parser at EOF, so the translator sees exactly what `eof_after_message_delta.txt` produces.

- [ ] **Step 5: Write both failing integration tests**

Append to `crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs`:

```rust
/// SMA-531: a response body that ends cleanly after `message_delta` — before
/// `message_stop` — must still deliver the terminal `Finish`. Before the fix
/// this stream emitted `[Usage, TokenDelta, TokenDelta, Usage]` and stopped.
#[tokio::test]
async fn clean_eof_after_message_delta_emits_finish() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/eof_after_message_delta.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    assert_eq!(
        oks.len(),
        5,
        "expected Usage, TokenDelta, TokenDelta, Usage, Finish -- got {oks:#?}"
    );
    assert_exactly_one_terminal_finish(&oks);
    assert!(
        matches!(oks.last().unwrap(), ModelEvent::Finish { reason } if *reason == FinishReason::Stop),
        "the buffered end_turn must flush as Finish::Stop, got {oks:#?}"
    );
}

/// SMA-531: the same guarantee for a byte-level cut. The partial `message_stop`
/// event has no blank-line terminator, so the SSE parser discards it at EOF and
/// the translator sees the same prefix as `eof_after_message_delta.txt`.
#[tokio::test]
async fn body_cut_inside_message_stop_emits_finish() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/body_cut_inside_message_stop.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    assert_eq!(
        oks.len(),
        5,
        "the partial message_stop must be discarded, leaving four events plus \
         the flushed Finish -- got {oks:#?}"
    );
    assert_exactly_one_terminal_finish(&oks);
}
```

- [ ] **Step 6: Run both tests and confirm they FAIL for the right reason**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test messages_streaming clean_eof_after_message_delta_emits_finish body_cut_inside_message_stop_emits_finish -- --nocapture`

Expected: both FAIL on the `assert_eq!(oks.len(), 5)` with **`left: 4`**.

`left: 4` is the whole point — it proves the four non-terminal events arrived and only the `Finish` is missing. If either reports `left: 3` or fewer, the fixture's `message_delta` did not dispatch: go back to Step 2 / Step 4 and fix the terminator, not the code. Record the observed numbers before continuing.

- [ ] **Step 7: Add `finish()` to the translator**

In `crates/paigasus-helikon-providers-anthropic/src/stream.rs`, insert immediately **before** `fn finish_or_error`:

```rust
    /// Flush a stop reason buffered from `message_delta` when the response
    /// body ends cleanly before `message_stop` arrived.
    ///
    /// Returns `None` when `message_stop` already drained the buffer (the
    /// well-formed path) or when no stop reason was ever observed. A stream
    /// that ended *before* `message_delta` is never reported as a clean
    /// `Stop`; one that ended *after* it is, because `message_delta`'s
    /// `stop_reason` is the model's own authoritative decision and
    /// `message_stop` is only a frame terminator.
    ///
    /// **Clean-EOF path only.** Never call this on the cancellation or
    /// transport-error paths — see `model.rs`.
    pub(crate) fn finish(&mut self) -> Option<Result<ModelEvent, ModelError>> {
        let reason = self.stop_reason.take()?;
        tracing::warn!(
            target: "paigasus::anthropic::stream",
            stop_reason = %reason,
            "stream body ended without message_stop; flushing buffered stop reason",
        );
        Some(self.finish_or_error(&reason))
    }
```

The `warn` is load-bearing: without it a flushed terminal is indistinguishable from a normal one downstream, leaving truncation rate unmeasurable in production.

- [ ] **Step 8: Call it from the driver's stream-exhausted arm only**

In `crates/paigasus-helikon-providers-anthropic/src/model.rs`, replace line 113 (`None => return,`) with:

```rust
                    // Stream exhausted normally (clean EOF). Flush a stop
                    // reason buffered by `message_delta` when `message_stop`
                    // never arrived, so the consumer always sees a terminal
                    // event. Deliberately NOT done on the cancellation
                    // (`:109`) or transport-error (below) arms.
                    None => {
                        if let Some(terminal) = translator.finish() {
                            yield terminal;
                        }
                        return;
                    }
```

Leave every other arm untouched.

- [ ] **Step 9: Run the two tests — they must now PASS**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test messages_streaming clean_eof_after_message_delta_emits_finish body_cut_inside_message_stop_emits_finish`

Expected: PASS, 2 tests.

- [ ] **Step 10: Run the whole crate — Task 1's net must still be green**

Run: `cargo test -p paigasus-helikon-providers-anthropic`

Expected: PASS. A failure in a Task 1 assertion means the flush is firing on a well-formed stream — investigate, do not relax the assertion.

- [ ] **Step 11: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-anthropic/src/stream.rs \
        crates/paigasus-helikon-providers-anthropic/src/model.rs \
        crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs \
        crates/paigasus-helikon-providers-anthropic/tests/fixtures/eof_after_message_delta.txt \
        crates/paigasus-helikon-providers-anthropic/tests/fixtures/body_cut_inside_message_stop.txt
git commit -m "fix(providers-anthropic): SMA-531 emit finish when the body ends before message_stop"
git show --stat
```

Confirm exactly five files.

---

### Task 3: Enforce at most one terminal event per stream

`Option::take` prevents re-emitting the *same* buffered reason; it does nothing about a *replacement* one. Task 2's flush therefore double-emits on `message_delta` → `message_stop` → `message_delta` → EOF. Close it with an explicit flag.

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/stream.rs`

**Interfaces:**
- Consumes: `MessageTranslator::finish()` from Task 2.
- Produces: field `terminal_emitted: bool` on `MessageTranslator`. No signature changes.

- [ ] **Step 1: Write both failing unit tests**

Append inside the existing `#[cfg(test)] mod tests` block in `src/stream.rs`:

```rust
    /// A second `message_delta` re-arms `stop_reason` after `message_stop`
    /// already emitted the terminal event. The EOF flush must not turn that
    /// into a second `Finish` — `core::Model::invoke` guarantees nothing
    /// follows `Finish`.
    #[test]
    fn second_message_delta_after_message_stop_does_not_double_finish() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        let stop_out = t.consume(AnthropicEvent::MessageStop).unwrap();
        assert_eq!(stop_out.len(), 1, "message_stop emits the terminal Finish");

        // Protocol violation: a second stop reason after the terminal event.
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("max_tokens".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        assert!(
            t.finish().is_none(),
            "a second stop reason must not yield a second terminal event"
        );
    }

    /// The same guard on the inline path. This case is a pre-existing defect,
    /// independent of the EOF flush: today the second `message_stop` emits a
    /// second `Finish`.
    #[test]
    fn repeated_message_stop_emits_one_finish() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        assert_eq!(t.consume(AnthropicEvent::MessageStop).unwrap().len(), 1);

        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        let second_stop = t.consume(AnthropicEvent::MessageStop).unwrap();
        assert!(
            second_stop.is_empty(),
            "a repeated message_stop must not emit a second Finish, got {second_stop:?}"
        );
    }
```

- [ ] **Step 2: Run them and confirm both FAIL**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib second_message_delta_after_message_stop_does_not_double_finish repeated_message_stop_emits_one_finish`

Expected: both FAIL — the first on `a second stop reason must not yield a second terminal event`, the second on `a repeated message_stop must not emit a second Finish`. If either passes, the guard already exists somewhere; stop and report.

- [ ] **Step 3: Add the field**

In the `MessageTranslator` struct definition, after `other_tool_fired: bool,`:

```rust
    terminal_emitted: bool,
```

And in `MessageTranslator::new`, after `other_tool_fired: false,`:

```rust
            terminal_emitted: false,
```

- [ ] **Step 4: Guard the inline emission site**

Replace the whole `AnthropicEvent::MessageStop` arm with:

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

- [ ] **Step 5: Guard the flush site**

Replace the whole of `finish()` (doc comment included, as added in Task 2) with:

```rust
    /// Flush a stop reason buffered from `message_delta` when the response
    /// body ends cleanly before `message_stop` arrived.
    ///
    /// Returns `None` when a terminal event was already emitted — the
    /// well-formed path, where `message_stop` drained the buffer — or when no
    /// stop reason was ever observed. A stream that ended *before*
    /// `message_delta` is never reported as a clean `Stop`; one that ended
    /// *after* it is, because `message_delta`'s `stop_reason` is the model's
    /// own authoritative decision and `message_stop` is only a frame
    /// terminator.
    ///
    /// `terminal_emitted` — not `stop_reason` being `Some` — is the guard.
    /// A second `message_delta` can re-arm the buffer after `message_stop`
    /// already emitted, and that must not produce a second terminal event.
    ///
    /// **Clean-EOF path only.** Never call this on the cancellation or
    /// transport-error paths — see `model.rs`.
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

- [ ] **Step 6: Run both tests — they must now PASS**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib second_message_delta_after_message_stop_does_not_double_finish repeated_message_stop_emits_one_finish`

Expected: PASS, 2 tests.

- [ ] **Step 7: Run the whole crate**

Run: `cargo test -p paigasus-helikon-providers-anthropic`

Expected: PASS.

- [ ] **Step 8: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-anthropic/src/stream.rs
git commit -m "fix(providers-anthropic): SMA-531 guard against a second terminal event"
git show --stat
```

---

### Task 4: Over-firing guards — no `Finish` without a stop reason, none after an error

These two tests pass both before and after the fix. Their value is entirely in the mutation checks: each kills a specific wrong implementation. Run the mutations — a guard nobody proved can fail is the failure mode this ticket exists to correct.

**Files:**
- Create: `crates/paigasus-helikon-providers-anthropic/tests/fixtures/eof_mid_content_block.txt`
- Create: `crates/paigasus-helikon-providers-anthropic/tests/fixtures/error_after_message_delta.txt`
- Modify: `crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing consumed later.

- [ ] **Step 1: Create `eof_mid_content_block.txt`**

Body ends cleanly with no `message_delta` at all, so no stop reason is ever observed. Must end with a blank line.

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"usage":{"input_tokens":12,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

```

- [ ] **Step 2: Create `error_after_message_delta.txt`**

A stop reason **is** buffered, then an in-band `error` arrives. This is what `stream_error.txt` cannot test — it has no `message_delta`, so it never buffers a reason. Must end with a blank line.

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_06","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"usage":{"input_tokens":12,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}

event: error
data: {"type":"error","error":{"type":"overloaded_error","message":"backend overloaded"}}

```

- [ ] **Step 3: Write both tests**

Append to `tests/messages_streaming.rs`:

```rust
/// SMA-531: no stop reason was ever observed, so a body that ends early must
/// NOT be reported as a clean `Stop`. Guards the flush against over-firing.
#[tokio::test]
async fn eof_mid_content_block_emits_no_finish() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/eof_mid_content_block.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    assert!(
        !oks.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
        "a stream with no observed stop reason must emit no Finish, got {oks:#?}"
    );
    assert_eq!(
        oks.len(),
        2,
        "expected Usage + one TokenDelta -- got {oks:#?}"
    );
}

/// SMA-531: a stop reason buffered by `message_delta` must be DISCARDED when a
/// mid-stream error follows, never flushed after the `Err`. The existing
/// `stream_error.txt` cannot catch this — that fixture has no `message_delta`,
/// so nothing is ever buffered there.
#[tokio::test]
async fn error_after_buffered_stop_reason_emits_no_finish() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/error_after_message_delta.txt");
    let events = run_stream(&server, fixture).await;

    assert!(
        !events
            .iter()
            .any(|r| matches!(r, Ok(ModelEvent::Finish { .. }))),
        "a buffered stop reason must be discarded on the error path, got {events:#?}"
    );
    assert!(
        matches!(events.last().unwrap(), Err(ModelError::Unavailable)),
        "the in-band error must be terminal, got {events:#?}"
    );
}
```

- [ ] **Step 4: Run both — they must PASS**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test messages_streaming eof_mid_content_block_emits_no_finish error_after_buffered_stop_reason_emits_no_finish`

Expected: PASS, 2 tests.

- [ ] **Step 5: Mutation A — prove `eof_mid_content_block_emits_no_finish` can fail**

Temporarily replace the first two lines of `finish()` in `src/stream.rs`:

```rust
        if self.terminal_emitted {
            return None;
        }
        let reason = self.stop_reason.take()?;
```

with an unconditional flush:

```rust
        if self.terminal_emitted {
            return None;
        }
        let reason = self
            .stop_reason
            .take()
            .unwrap_or_else(|| "end_turn".to_owned());
```

Run: `cargo test -p paigasus-helikon-providers-anthropic --test messages_streaming eof_mid_content_block_emits_no_finish`

Expected: **FAIL** on "a stream with no observed stop reason must emit no Finish". Record the failure, then **revert the mutation** and re-run to confirm PASS.

- [ ] **Step 6: Mutation B — prove `error_after_buffered_stop_reason_emits_no_finish` can fail**

Temporarily change the `consume` error arm in `src/model.rs` from:

```rust
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
```

to:

```rust
                            Err(e) => {
                                if let Some(terminal) = translator.finish() {
                                    yield terminal;
                                }
                                yield Err(e);
                                return;
                            }
```

Run: `cargo test -p paigasus-helikon-providers-anthropic --test messages_streaming error_after_buffered_stop_reason_emits_no_finish`

Expected: **FAIL** on "a buffered stop reason must be discarded on the error path". Record it, then **revert the mutation** and re-run to confirm PASS.

- [ ] **Step 7: Confirm the tree is clean of both mutations**

Run: `git diff -- crates/paigasus-helikon-providers-anthropic/src/`

Expected: **empty output.** If anything appears, a mutation was not reverted — revert it now.

- [ ] **Step 8: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs \
        crates/paigasus-helikon-providers-anthropic/tests/fixtures/eof_mid_content_block.txt \
        crates/paigasus-helikon-providers-anthropic/tests/fixtures/error_after_message_delta.txt
git commit -m "test(providers-anthropic): SMA-531 guard the flush against over-firing"
git show --stat
```

Confirm exactly three files — **no `src/` file may appear.**

---

### Task 5: Unit-test every `finish()` outcome

`finish_or_error` has four outcomes reachable at EOF. Task 2 covered one indirectly. Cover all four plus the `None` cases directly, matching Bedrock's coverage (`providers-bedrock/src/stream.rs:762-829`).

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/stream.rs` (test module only)

**Interfaces:**
- Consumes: `MessageTranslator::finish()`, the `message_start` test helper already in the module.
- Produces: nothing.

- [ ] **Step 1: Write all six tests**

Append inside `#[cfg(test)] mod tests` in `src/stream.rs`:

```rust
    /// The core flush: a stop reason buffered with no `message_stop` following.
    #[test]
    fn finish_flushes_pending_stop_reason() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: Some(MessageDeltaUsage { output_tokens: 5 }),
            })
            .unwrap();
        match t.finish().expect("a buffered reason must flush") {
            Ok(ModelEvent::Finish { reason }) => assert_eq!(reason, FinishReason::Stop),
            other => panic!("expected Ok(Finish::Stop), got {other:?}"),
        }
    }

    /// The highest-consequence `Ok` variant: the agent loop will execute tool
    /// calls assembled from a stream that was cut short.
    #[test]
    fn finish_flushes_tool_use_as_tool_calls() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("tool_use".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        match t.finish().expect("a buffered reason must flush") {
            Ok(ModelEvent::Finish { reason }) => assert_eq!(reason, FinishReason::ToolCalls),
            other => panic!("expected Ok(Finish::ToolCalls), got {other:?}"),
        }
    }

    /// Truncation before any stop reason: never reported as a clean `Stop`.
    #[test]
    fn finish_is_none_when_no_stop_reason_observed() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::Text,
        });
        assert!(
            t.finish().is_none(),
            "no stop reason was observed; nothing to flush"
        );
    }

    /// The well-formed path: `message_stop` already emitted, so the EOF flush
    /// is a no-op — and stays one on a repeated call.
    #[test]
    fn finish_is_none_after_message_stop_drained_it() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        assert_eq!(t.consume(AnthropicEvent::MessageStop).unwrap().len(), 1);
        assert!(
            t.finish().is_none(),
            "message_stop already emitted the terminal event"
        );
        assert!(t.finish().is_none(), "finish() must be idempotent");
    }

    /// A refusal observed before truncation surfaces as an error, not silence.
    #[test]
    fn finish_surfaces_refusal_as_error() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("refusal".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        match t.finish().expect("a buffered reason must flush") {
            Err(ModelError::Refused { .. }) => {}
            other => panic!("expected Err(Refused), got {other:?}"),
        }
    }

    /// The second `Err` outcome: synthesis mode with both a real and the
    /// synthesized tool fired. Mirrors bedrock's
    /// `finish_surfaces_both_tools_error_without_metadata`.
    #[test]
    fn finish_surfaces_both_tools_error() {
        let mut t = MessageTranslator::new(true);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_s".to_owned(),
                name: SYNTHESIZED_TOOL_NAME.to_owned(),
                input: serde_json::json!({}),
            },
        });
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 1,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_r".to_owned(),
                name: "search".to_owned(),
                input: serde_json::json!({}),
            },
        });
        let _ = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("tool_use".to_owned()),
                },
                usage: None,
            })
            .unwrap();
        match t.finish().expect("a buffered reason must flush") {
            Err(ModelError::Other(_)) => {}
            other => panic!("expected Err(Other), got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run them**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib finish_`

Expected: PASS. This selects every test whose name starts with `finish_` — 6 new plus any pre-existing.

If a name-resolution error appears for `MessageDeltaUsage`, `ContentBlockHead`, or `SYNTHESIZED_TOOL_NAME`, add the missing item to the test module's `use` block — the module already imports `MessageDeltaPayload`, `ContentBlockHead`, `MessageStartPayload`, and `AnthropicErrorPayload` from `crate::sse`, and `SYNTHESIZED_TOOL_NAME` / `MessageDeltaUsage` come in via the `use super::*;` at the top.

- [ ] **Step 3: Run the whole crate**

Run: `cargo test -p paigasus-helikon-providers-anthropic`

Expected: PASS.

- [ ] **Step 4: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-anthropic/src/stream.rs
git commit -m "test(providers-anthropic): SMA-531 cover every finish outcome"
git show --stat
```

---

### Task 6: Cancellation coverage

The crate has **zero** cancellation tests today, so the ticket's "cancellation still emits no `Finish`" criterion is unpinned by anything.

**Files:**
- Create: `crates/paigasus-helikon-providers-anthropic/tests/cancellation.rs`

**Interfaces:**
- Consumes: `AnthropicModel`, `fixtures/text_only.txt`.
- Produces: nothing.

- [ ] **Step 1: Create the file**

```rust
//! Cancellation: the stream must terminate without emitting Finish when the
//! CancellationToken fires mid-flight.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_anthropic::AnthropicModel;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn user(text: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text {
            text: text.to_owned(),
        }],
    }
}

/// SMA-531: cancellation must end the stream without a terminal `Finish`, as
/// `paigasus_helikon_core::Model::invoke` mandates.
///
/// The complementary case (cancel firing AFTER `message_delta` buffered a stop
/// reason but BEFORE EOF) is deliberately not tested here: wiremock's
/// `set_delay` delays the whole response, so that interleaving is unreachable
/// and any test of it would assert whatever the scheduler happened to do. It is
/// guaranteed structurally instead — the `tokio::select!` cancel arm in
/// `model.rs` `return`s without calling `translator.finish()`.
#[tokio::test]
async fn cancellation_before_first_chunk_emits_no_finish() {
    let server = MockServer::start().await;

    // Delay the response so cancellation fires first.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(
                    include_str!("fixtures/text_only.txt"),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_clone.cancel();
    });

    let mut req = ModelRequest::new();
    req.messages = vec![user("hi")];

    // Start the timer before invoke() so a hang inside invoke() is also caught.
    let start = std::time::Instant::now();
    let stream_result = model.invoke(req, cancel).await;

    // Either invoke() returns an error, or the stream ends quickly with no
    // Finish. Both satisfy the Model trait's cancellation contract.
    match stream_result {
        Ok(mut s) => {
            let mut emitted = Vec::new();
            while let Some(item) = s.next().await {
                if let Ok(ev) = item {
                    emitted.push(ev);
                }
            }
            assert!(
                !emitted
                    .iter()
                    .any(|e| matches!(e, ModelEvent::Finish { .. })),
                "stream emitted Finish after cancellation: {emitted:#?}"
            );
        }
        Err(_) => { /* acceptable */ }
    }

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(4),
        "cancellation took too long: {elapsed:?}"
    );
}

/// The control for the test above. Without it, that assertion would pass
/// against a build that emits no events at all — the exact vacuity this
/// ticket's acceptance criteria call out.
#[tokio::test]
async fn uncancelled_stream_emits_exactly_one_finish() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(
                    include_str!("fixtures/text_only.txt"),
                    "text/event-stream",
                ),
        )
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();

    let mut req = ModelRequest::new();
    req.messages = vec![user("hi")];

    let mut s = model
        .invoke(req, CancellationToken::new())
        .await
        .expect("invoke should succeed");

    let mut emitted = Vec::new();
    while let Some(item) = s.next().await {
        emitted.push(item.expect("no error expected"));
    }

    let finishes = emitted
        .iter()
        .filter(|e| matches!(e, ModelEvent::Finish { .. }))
        .count();
    assert_eq!(finishes, 1, "expected exactly one Finish, got {emitted:#?}");
    assert!(
        matches!(emitted.last().unwrap(), ModelEvent::Finish { .. }),
        "Finish must be last, got {emitted:#?}"
    );
}
```

- [ ] **Step 2: Run the new file**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test cancellation`

Expected: PASS, 2 tests.

If `tokio` or `wiremock` is reported as an unresolved crate, check `crates/paigasus-helikon-providers-anthropic/Cargo.toml`'s `[dev-dependencies]` — `messages_streaming.rs` already uses both, so they should be present. Do **not** add a dependency without reporting it first.

- [ ] **Step 3: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-anthropic/tests/cancellation.rs
git commit -m "test(providers-anthropic): SMA-531 add cancellation coverage"
git show --stat
```

---

### Task 7: Full CI-gate verification

Reproduce every gate that runs on the PR, locally, before opening it.

**Files:** none modified (unless a gate fails).

- [ ] **Step 1: Formatting**

Run: `cargo fmt --all -- --check`
Expected: no output, exit 0.

- [ ] **Step 2: Clippy at CI severity**

Run: `cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: exit 0. Fix any warning in the files this branch touched.

- [ ] **Step 3: The full test gate — the exact CI command**

Run: `cargo test --workspace --all-features`

Expected: PASS. Run this exact command, not a per-crate subset — a per-crate run has previously masked a workspace-level failure.

- [ ] **Step 4: Docs**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
Expected: exit 0.

- [ ] **Step 5: Commit-message check**

Run: `convco check origin/main..HEAD`
Expected: all commits valid.

- [ ] **Step 6: Confirm the diff matches the plan**

Run: `git diff --stat origin/main..HEAD`

Expected exactly these files, and nothing else:
- `docs/superpowers/specs/2026-08-16-sma-531-anthropic-eof-finish-flush-design.md`
- `docs/superpowers/plans/2026-08-16-sma-531-anthropic-eof-finish-flush.md`
- `crates/paigasus-helikon-providers-anthropic/src/stream.rs`
- `crates/paigasus-helikon-providers-anthropic/src/model.rs`
- `crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs`
- `crates/paigasus-helikon-providers-anthropic/tests/cancellation.rs`
- 4 new files under `crates/paigasus-helikon-providers-anthropic/tests/fixtures/`

Any `Cargo.toml`, `CHANGELOG.md`, `docs/book/`, or `README.md` in the list is a mistake — revert it.

- [ ] **Step 7: Confirm no debug residue**

Run: `git diff origin/main..HEAD -- crates/ | grep -nE '(dbg!|println!|eprintln!|TODO|FIXME|unwrap\(\) // )'`

Expected: no matches other than legitimate `unwrap()` calls inside `#[cfg(test)]` code. `dbg!`/`println!` anywhere in `src/` is residue — remove it.

---

## Notes for the implementer

**On the `warn` log level.** `tracing::warn!` in `finish()` is intentional and differs from OpenAI's `debug!` on its sibling branch. OpenAI logs when *no* finish reason was observed (unremarkable); this logs when a real stream was cut short (remarkable, and the only way to measure truncation rate in production). Do not downgrade it.

**On what this fix deliberately does not cover.** Only a *clean* body end reaches `model.rs`'s `None` arm. A connection reset or a chunked stream cut without its terminator surfaces as `Some(Err(_))` and keeps yielding `Err(Transport)` with the buffered reason discarded — required by SMA-533's acceptance clause 6 ("a mid-stream error emits no `Finish`") and consistent with all three prior-art providers. If a reviewer asks for a flush on the error arm, point at the spec's "What truncation means here" section rather than adding one.

**Behaviour change to carry into the PR body.** A cleanly-truncated *refusal* completes the run successfully today (no `Finish` → `ModelTurnAccumulator`'s `FinishReason::Stop` default at `core/src/model.rs:558`) and will hard-fail it after this change (`agent.rs:967-974` turns any stream `Err` into `RunFailed`). `RetryingModel` will not rescue it — `runtime-tokio/src/retry.rs:217-233` retries only when the *first* stream item is an `Err`, and Anthropic always emits `Usage` from `message_start` first.
