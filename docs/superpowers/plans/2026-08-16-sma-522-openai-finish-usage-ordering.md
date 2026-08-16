# SMA-522 OpenAI `Finish`/`Usage` Ordering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `Finish` the genuinely last event on every OpenAI Chat Completions stream, and replace the fixtures that encode an impossible wire shape with ones that encode the real one.

**Architecture:** `ChatTranslator` stops emitting `Finish` inline from `consume` and instead buffers the mapped finish reason in a struct field. A new `finish()` drains that buffer, called from the single stream-exhausted arm of the driver loop. `Usage` continues to flow through inline as it arrives, so the emitted order becomes `…Usage, Finish`. This mirrors `paigasus-helikon-providers-gemini`.

**Tech Stack:** Rust 2024, `async-openai` 0.41.3, `async-stream`, `tokio`, `wiremock`, `futures-util`.

**Spec:** `docs/superpowers/specs/2026-08-16-sma-522-openai-finish-usage-ordering-design.md`

## Global Constraints

- Work in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-522-openai-finish-usage-order` on branch `feature/sma-522-openai-provider-emits-finish-before-usage-violating-the-core`. Never `cd` to the main checkout.
- **Never `git add -A`** — `.env` and `.claude` are untracked but not gitignored in this repo. Stage explicit paths only.
- **Never use `git stash`** — the stash stack is shared with other worktrees and concurrent sessions.
- Commit prefix: `<type>(<scope>): SMA-522 <message>`, subject lowercase. Scope `providers-openai` is valid. A local `commit-msg` hook runs `convco check`.
- Commits are signed via a 1Password SSH key. If a commit fails with "failed to fill whole buffer", stop and ask the user to unlock their vault — do not bypass signing.
- Run `cargo fmt --all` before every commit. The `pre-commit` hook is a deliberate no-op; `pre-push` runs fmt + full-workspace clippy and takes minutes.
- Only `ChatTranslator::finish` is added to the crate's private surface. `ChatTranslator` is `pub(crate)` inside a private `mod backend`; **no public API changes**, so no README, no mdBook, and no version bump.
- `FinishReason` derives `Debug, Clone, PartialEq, Eq` — comparison and `?`-formatting in `tracing` both work.
- `tracing` is already a dependency of `paigasus-helikon-providers-openai`.
- Fixture provenance comments must be SSE comment lines (`:`-prefixed). A `#` or `//` line parses as an unknown SSE field.

---

### Task 1: Prove the bug with a real-envelope test, then fix it

The existing `chat_wire.rs` happy-path test **already asserts `Finish` is the last event** (lines 87-94). Its body is simply built from the impossible wire shape, which is why it passes today. Reshaping that body to the captured envelope converts an existing passing test into a genuine regression test — this is the cheapest possible mutation check, and it comes first.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/tests/chat_wire.rs:30-41`
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` (struct at 216-233, `consume` at 242-303, driver arm at 76-77)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `ChatTranslator::finish(&mut self) -> Vec<ModelEvent>` — drains the buffered finish reason, returning `vec![ModelEvent::Finish { reason }]` or an empty vec. Tasks 2, 3 and 5 rely on this exact signature. Also produces the private field `ChatTranslator::finish_reason: Option<FinishReason>`.

- [ ] **Step 1: Reshape the `chat_wire.rs` SSE body to the real envelope**

Replace lines 30-41 of `crates/paigasus-helikon-providers-openai/tests/chat_wire.rs` with:

```rust
    // SSE body: a content-delta chunk, then a finish chunk, then a SEPARATE
    // trailing chunk carrying usage — the shape real OpenAI-compatible
    // servers emit with `stream_options.include_usage: true`. Captured from
    // LiteLLM; see the spec's Appendix A. async-openai requires `id`,
    // `created`, `model`, and `object` on every chunk.
    let body = concat!(
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{}}],",
        "\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1,\"total_tokens\":4}}\n\n",
        "data: [DONE]\n\n",
    );
```

Note the deleted sentence — "Usage arrives on the same chunk as `finish_reason`" — was the false claim in prose. It must not survive.

- [ ] **Step 2: Run the test and verify it now FAILS**

```bash
cargo test -p paigasus-helikon-providers-openai --test chat_wire happy_path_text_completion
```

Expected: **FAIL** on the "Finish(Stop) must be the last event" assertion, because the translator emits `Finish` on the finish chunk and then `Usage` on the trailing chunk.

**Record the failure output** — it is the mutation-check evidence quoted in the PR body. Do not proceed until you have seen it fail.

- [ ] **Step 3: Add the `finish_reason` field to `ChatTranslator`**

In `crates/paigasus-helikon-providers-openai/src/backend/chat.rs`, change the struct (currently lines 216-223) to:

```rust
pub(crate) struct ChatTranslator {
    /// index → call_id after the first delta for that tool call.
    tool_calls: HashMap<u32, String>,
    /// Indices for which `name` has already been emitted to the consumer.
    name_emitted: HashSet<u32>,
    /// index → buffered (name, args) that arrived before the call_id was known.
    pending: HashMap<u32, PendingToolCall>,
    /// Finish reason observed so far, emitted only by [`Self::finish`] at
    /// end-of-stream. Last observed value wins.
    finish_reason: Option<FinishReason>,
}
```

and `new()` to:

```rust
    pub(crate) fn new() -> Self {
        Self {
            tool_calls: HashMap::new(),
            name_emitted: HashSet::new(),
            pending: HashMap::new(),
            finish_reason: None,
        }
    }
```

- [ ] **Step 4: Buffer the finish reason instead of emitting it**

In `consume`, delete the local declaration on line 244:

```rust
        let mut finish_event: Option<ModelEvent> = None;
```

Replace the finish-reason block (currently lines 263-276) with:

```rust
            // Buffer the finish reason — emitted by `finish()` at end-of-stream,
            // never inline, because `usage` arrives on a LATER chunk.
            if let Some(reason) = choice.finish_reason {
                let mapped = match reason {
                    OaFinishReason::Stop => FinishReason::Stop,
                    OaFinishReason::Length => FinishReason::Length,
                    OaFinishReason::ToolCalls => FinishReason::ToolCalls,
                    OaFinishReason::ContentFilter => FinishReason::ContentFilter,
                    OaFinishReason::FunctionCall => FinishReason::Other("function_call".to_owned()),
                    // OaFinishReason has no #[non_exhaustive] in 0.41 but guard for robustness.
                    #[allow(unreachable_patterns)]
                    other => FinishReason::Other(format!("{other:?}")),
                };
                if let Some(prev) = self.finish_reason.as_ref() {
                    if *prev != mapped {
                        tracing::debug!(
                            target: "paigasus::openai::chat",
                            previous = ?prev,
                            replacement = ?mapped,
                            "second distinct finish_reason observed; last wins"
                        );
                    }
                }
                self.finish_reason = Some(mapped);
            }
```

Then delete the trailing emission block (currently lines 297-300):

```rust
        // Append Finish last (terminal event).
        if let Some(finish) = finish_event {
            out.push(finish);
        }
```

- [ ] **Step 5: Add the `finish()` method**

Insert immediately after `consume`'s closing brace, before `fn handle_tool_call_chunk`:

```rust
    /// Emit the terminal `Finish` at end-of-stream.
    ///
    /// Returns an empty vec when no `finish_reason` was ever observed, so a
    /// truncated stream is never reported as a clean stop.
    pub(crate) fn finish(&mut self) -> Vec<ModelEvent> {
        let Some(reason) = self.finish_reason.take() else {
            tracing::debug!(
                target: "paigasus::openai::chat",
                "stream ended without a finish_reason; emitting no Finish"
            );
            return Vec::new();
        };
        vec![ModelEvent::Finish { reason }]
    }
```

- [ ] **Step 6: Flush `finish()` from the stream-exhausted arm**

In `invoke`, replace the `None` arm (currently line 77):

```rust
                None => return,
```

with:

```rust
                None => {
                    // `async-openai`'s `create_stream` consumes `[DONE]`
                    // internally and ends iteration, so this is the single
                    // end-of-stream site.
                    for ev in translator.finish() {
                        yield Ok(ev);
                    }
                    return;
                }
```

Leave the `Some(Err(_))` arm and the `tokio::select!` cancellation arms untouched. Both deliberately drop any buffered `Finish`: an errored stream did not complete cleanly, and the core contract requires cancellation to end the stream without `Finish`.

- [ ] **Step 7: Update the module and `consume` doc comments**

Replace the `consume` doc comment (currently lines 235-241) with:

```rust
    /// Consume one upstream SSE chunk and produce zero or more [`ModelEvent`]s.
    ///
    /// `Usage` is emitted inline as it arrives. `Finish` is **never** emitted
    /// here — the finish reason is buffered and released by [`Self::finish`]
    /// at end-of-stream, because `usage` arrives on a chunk *after* the one
    /// carrying `finish_reason`. Only `Finish` is positionally constrained by
    /// the contract in `paigasus_helikon_core::Model::invoke`; `Usage` may
    /// appear anywhere.
```

Also fix the stale version references: `chat.rs:120` and `chat.rs:179` say "async-openai 0.40"; `Cargo.lock` pins 0.41.3. Change both to "0.41".

- [ ] **Step 8: Run the test and verify it PASSES**

```bash
cargo fmt --all
cargo test -p paigasus-helikon-providers-openai --test chat_wire
```

Expected: PASS.

- [ ] **Step 9: Run the whole crate's tests**

```bash
cargo test -p paigasus-helikon-providers-openai --all-features
```

Expected: all pass. The two fixture-driven tests still pass because their fixtures put `usage` on the finish chunk — `consume` emits `[Usage]`, `finish()` emits `[Finish]`, same order as before. Task 4 fixes those fixtures.

- [ ] **Step 10: Commit**

```bash
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs \
        crates/paigasus-helikon-providers-openai/tests/chat_wire.rs
git commit -m "fix(providers-openai): SMA-522 emit finish at end of stream

usage arrives on a chunk after the one carrying finish_reason, so
emitting Finish inline put a Usage event after the terminal event.
Buffer the reason and flush it from the stream-exhausted arm, matching
the gemini provider.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Unit-test the new translator semantics

Pins the behaviour change the spec identified: exactly one `Finish` per stream with last-wins, rather than one per chunk carrying a `finish_reason`.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` (test module starts at line 378)

**Interfaces:**
- Consumes: `ChatTranslator::new()`, `ChatTranslator::consume(CreateChatCompletionStreamResponse) -> Vec<ModelEvent>`, `ChatTranslator::finish() -> Vec<ModelEvent>` from Task 1.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Add a chunk-building helper and the three tests**

Append inside `mod tests` in `crates/paigasus-helikon-providers-openai/src/backend/chat.rs`, after the existing tests:

```rust
    /// Build a stream chunk from raw JSON, so tests state the wire shape
    /// directly rather than constructing async-openai types field by field.
    fn stream_chunk(json: &str) -> CreateChatCompletionStreamResponse {
        serde_json::from_str(json).expect("fixture chunk must deserialize")
    }

    #[test]
    fn finish_is_emitted_only_at_end_of_stream() {
        let mut t = ChatTranslator::new();

        let evs = t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        ));
        assert!(
            !evs.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
            "consume must not emit Finish inline, got {evs:?}"
        );

        let fin = t.finish();
        assert_eq!(fin.len(), 1, "expected exactly one Finish, got {fin:?}");
        assert!(matches!(
            &fin[0],
            ModelEvent::Finish {
                reason: FinishReason::Stop
            }
        ));
    }

    #[test]
    fn repeated_finish_reasons_yield_one_finish_last_wins() {
        let mut t = ChatTranslator::new();

        t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        ));
        let mid = t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":1,"delta":{"content":"x"}}]}"#,
        ));
        assert!(
            mid.iter().any(|e| matches!(e, ModelEvent::TokenDelta { .. })),
            "expected the interleaved TokenDelta, got {mid:?}"
        );
        t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":1,"delta":{},"finish_reason":"length"}]}"#,
        ));

        let fin = t.finish();
        assert_eq!(
            fin.len(),
            1,
            "exactly one Finish per stream, not one per chunk; got {fin:?}"
        );
        assert!(
            matches!(
                &fin[0],
                ModelEvent::Finish {
                    reason: FinishReason::Length
                }
            ),
            "last observed finish_reason must win, got {fin:?}"
        );
    }

    #[test]
    fn truncated_stream_emits_no_finish() {
        let mut t = ChatTranslator::new();
        t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{"content":"partial"}}]}"#,
        ));
        assert!(
            t.finish().is_empty(),
            "a stream with no finish_reason must not report a clean stop"
        );
    }

    #[test]
    fn finish_is_idempotent_after_draining() {
        let mut t = ChatTranslator::new();
        t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        ));
        assert_eq!(t.finish().len(), 1);
        assert!(
            t.finish().is_empty(),
            "finish() takes the buffer; a second call must yield nothing"
        );
    }
```

- [ ] **Step 2: Verify `serde_json` is available as a dev-dependency**

```bash
grep -n "serde_json" crates/paigasus-helikon-providers-openai/Cargo.toml
```

If it appears only under `[dependencies]` that is fine — dependencies are visible to unit tests in the same crate. If it is absent entirely, add `serde_json = { workspace = true }` to `[dev-dependencies]`.

- [ ] **Step 3: Run the unit tests**

```bash
cargo test -p paigasus-helikon-providers-openai --lib
```

Expected: all four new tests pass.

- [ ] **Step 4: Confirm the tests are real regression tests (two distinct mutations)**

> **Corrected during execution.** An earlier draft of this step claimed the inline-`Finish` mutation fails `repeated_finish_reasons_yield_one_finish_last_wins`. It does not: that test discards the return values of the two `consume` calls carrying a `finish_reason`, so a leaked inline `Finish` is never observed and the test still passes. Each test needs the mutation that actually targets it.

**Mutation A — inline `Finish` leak.** In `consume`, add `out.push(ModelEvent::Finish { reason: mapped.clone() });` immediately **before** `self.finish_reason = Some(mapped);`. (It must go before, not after: `FinishReason` is not `Copy`, so `Some(mapped)` moves the value and a later `mapped.clone()` will not compile.) Then:

```bash
cargo test -p paigasus-helikon-providers-openai --lib finish_is_emitted_only_at_end_of_stream
```

Expected: **FAIL** — `consume` emits `Finish` inline, which is exactly what that test asserts against. Record the output and revert the line.

**Mutation B — first-wins instead of last-wins.** In `consume`, guard the buffer write so it only writes when empty: `if self.finish_reason.is_none() { self.finish_reason = Some(mapped); }`. Then:

```bash
cargo test -p paigasus-helikon-providers-openai --lib repeated_finish_reasons_yield_one_finish_last_wins
```

Expected: **FAIL** — the buffered reason stays `Stop` where the test requires `Length`. Record the output and revert the guard.

After both, run `git diff` and confirm only the intended test additions remain, then re-run the full `--lib` suite to confirm green.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs
git commit -m "test(providers-openai): SMA-522 pin one-finish-per-stream semantics

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Add the captured anchor fixtures and their regression test

**Files:**
- Create: `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_text_usage_trailing.txt`
- Create: `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_text_usage_trailing_empty_choices.txt`
- Modify: `crates/paigasus-helikon-providers-openai/tests/chat_streaming.rs`

**Interfaces:**
- Consumes: `ChatTranslator::finish()` behaviour from Task 1; the existing `run(fixture: &str) -> Vec<ModelEvent>` helper at `chat_streaming.rs:25-52`.
- Produces: two fixture files later tasks do not depend on.

- [ ] **Step 1: Create the captured anchor fixture**

Create `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_text_usage_trailing.txt`. The finish chunk, usage chunk, and `[DONE]` are transcribed byte-for-byte from a live LiteLLM capture; the content deltas are reconstructed in the same shape.

```text
: Provenance: captured 2026-08-16 from ghcr.io/berriai/litellm:main-stable
: serving the OpenAI Chat Completions wire shape, keyless mock backend.
: The finish chunk, the trailing usage chunk and [DONE] are byte-for-byte
: from that capture. The ten content deltas are reconstructed in the same
: shape (the captured trace elided them). This is a PROXY capture, not
: api.openai.com -- see the empty-choices variant for the first-party shape.
data: {"id":"chatcmpl-1cc5e8c0","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}

data: {"id":"chatcmpl-1cc5e8c0","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"lo "}}]}

data: {"id":"chatcmpl-1cc5e8c0","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"fro"}}]}

data: {"id":"chatcmpl-1cc5e8c0","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"m t"}}]}

data: {"id":"chatcmpl-1cc5e8c0","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"he "}}]}

data: {"id":"chatcmpl-1cc5e8c0","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"moc"}}]}

data: {"id":"chatcmpl-1cc5e8c0","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"k b"}}]}

data: {"id":"chatcmpl-1cc5e8c0","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"ack"}}]}

data: {"id":"chatcmpl-1cc5e8c0","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"end"}}]}

data: {"id":"chatcmpl-1cc5e8c0","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"."}}]}

data: {"id":"chatcmpl-1cc5e8c0","object":"chat.completion.chunk","created":1786898631,"model":"mock-fast","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"id":"chatcmpl-1cc5e8c0","created":1786898631,"model":"mock-fast","object":"chat.completion.chunk","choices":[{"index":0,"delta":{}}],"usage":{"completion_tokens":6,"prompt_tokens":8,"total_tokens":14,"completion_tokens_details":{"reasoning_tokens":0}}}

data: [DONE]

```

The key-order difference on the usage chunk (`created` before `object`) is what the wire produced; preserve it.

- [ ] **Step 2: Create the first-party empty-choices variant**

Create `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_text_usage_trailing_empty_choices.txt`:

```text
: Provenance: HAND-AUTHORED, not captured. Same trailing-usage envelope as
: chat_text_usage_trailing.txt, but with "choices":[] on the usage chunk --
: the shape api.openai.com emits. async-openai documents the field as "Can
: also be empty for the last chunk if you set stream_options:
: {"include_usage": true}". Both shapes must translate identically.
data: {"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":"hello"}}]}

data: {"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[],"usage":{"prompt_tokens":8,"completion_tokens":6,"total_tokens":14}}

data: [DONE]

```

- [ ] **Step 3: Add the regression tests**

In `crates/paigasus-helikon-providers-openai/tests/chat_streaming.rs`, add the two `include_str!` constants beside the existing ones at lines 14-15:

```rust
const TRAILING_USAGE_FIXTURE: &str = include_str!("fixtures/chat_text_usage_trailing.txt");
const TRAILING_USAGE_EMPTY_CHOICES_FIXTURE: &str =
    include_str!("fixtures/chat_text_usage_trailing_empty_choices.txt");
```

and append these tests:

```rust
/// SMA-522: `usage` arrives on a chunk AFTER the one carrying `finish_reason`.
/// `Finish` must still be the terminal event, with `Usage` before it.
#[tokio::test]
async fn trailing_usage_chunk_still_finishes_last() {
    let events = run(TRAILING_USAGE_FIXTURE).await;

    let finish_count = events
        .iter()
        .filter(|e| matches!(e, ModelEvent::Finish { .. }))
        .count();
    assert_eq!(finish_count, 1, "expected exactly one Finish, got {events:#?}");

    assert!(
        matches!(
            events.last().unwrap(),
            ModelEvent::Finish {
                reason: FinishReason::Stop
            }
        ),
        "Finish(Stop) must be the last event, got {events:#?}"
    );

    let usage_pos = events
        .iter()
        .position(|e| matches!(e, ModelEvent::Usage { .. }))
        .expect("a Usage event must be present");
    let finish_pos = events
        .iter()
        .position(|e| matches!(e, ModelEvent::Finish { .. }))
        .expect("a Finish event must be present");
    assert!(
        usage_pos < finish_pos,
        "Usage must precede Finish; usage at {usage_pos}, finish at {finish_pos}"
    );

    // Assert the real captured counts. Without this a translator emitting a
    // zeroed Usage would satisfy the ordering assertions above.
    assert!(
        events.iter().any(|e| matches!(
            e,
            ModelEvent::Usage {
                input_tokens: 8,
                output_tokens: 6,
                ..
            }
        )),
        "expected Usage {{ input_tokens: 8, output_tokens: 6 }}, got {events:#?}"
    );
}

/// The same envelope with `"choices":[]` on the usage chunk — the shape
/// api.openai.com emits — must translate identically.
#[tokio::test]
async fn trailing_usage_with_empty_choices_finishes_last() {
    let events = run(TRAILING_USAGE_EMPTY_CHOICES_FIXTURE).await;

    assert!(
        matches!(
            events.last().unwrap(),
            ModelEvent::Finish {
                reason: FinishReason::Stop
            }
        ),
        "Finish(Stop) must be the last event, got {events:#?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ModelEvent::Usage {
                input_tokens: 8,
                output_tokens: 6,
                ..
            }
        )),
        "expected Usage {{ input_tokens: 8, output_tokens: 6 }}, got {events:#?}"
    );
}
```

- [ ] **Step 4: Run the tests**

```bash
cargo test -p paigasus-helikon-providers-openai --test chat_streaming
```

Expected: PASS.

- [ ] **Step 5: Confirm both are real regression tests**

Temporarily revert the driver: in `chat.rs`, change the `None` arm back to `None => return,` and add `out.push(ModelEvent::Finish { reason: mapped.clone() });` immediately **before** `self.finish_reason = Some(mapped);` in `consume`. (Before, not after — `FinishReason` is not `Copy`, so `Some(mapped)` moves the value and a trailing `mapped.clone()` will not compile.) Then:

```bash
cargo test -p paigasus-helikon-providers-openai --test chat_streaming trailing_usage
```

Expected: **both FAIL** — `Finish` is not last. Record the output, then restore both edits and re-run to confirm PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-openai/tests/fixtures/chat_text_usage_trailing.txt \
        crates/paigasus-helikon-providers-openai/tests/fixtures/chat_text_usage_trailing_empty_choices.txt \
        crates/paigasus-helikon-providers-openai/tests/chat_streaming.rs
git commit -m "test(providers-openai): SMA-522 add captured trailing-usage fixtures

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Re-shape the two existing fixtures to the real envelope

Both encode the impossible shape. Their existing assertions must keep passing unchanged — only the wire shape moves.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_parallel_tool_calls.txt:13`
- Modify: `crates/paigasus-helikon-providers-openai/tests/fixtures/chat_content_filter.txt:3`

**Interfaces:**
- Consumes: `ChatTranslator::finish()` behaviour from Task 1.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Split the finish chunk in `chat_parallel_tool_calls.txt`**

Replace line 13 (the combined finish+usage chunk) with two chunks, and prepend the provenance header at the top of the file:

```text
: Provenance: HAND-AUTHORED. The trailing-usage envelope matches the live
: capture in chat_text_usage_trailing.txt; the parallel tool-call payload is
: hand-built because a keyless LiteLLM mock cannot emit streamed tool calls
: (mock_tool_calls is ignored on the proxy's streaming path).
```

and the split:

```text
data: {"id":"x","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: {"id":"x","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o","choices":[{"index":0,"delta":{}}],"usage":{"prompt_tokens":10,"completion_tokens":12,"total_tokens":22}}
```

Leave lines 1-11 (`[DONE]` and the tool-call deltas) unchanged.

- [ ] **Step 2: Split the finish chunk in `chat_content_filter.txt`**

Prepend the provenance header:

```text
: Provenance: HAND-AUTHORED. The trailing-usage envelope matches the live
: capture in chat_text_usage_trailing.txt; the content_filter finish reason
: is hand-built because it cannot be provoked on demand from any mock.
```

Replace line 3 with:

```text
data: {"id":"x","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"content_filter"}]}

data: {"id":"x","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o","choices":[{"index":0,"delta":{}}],"usage":{"prompt_tokens":4,"completion_tokens":4,"total_tokens":8}}
```

Leave line 1 (the content delta) and the trailing `[DONE]` unchanged.

- [ ] **Step 3: Run the fixture-driven tests**

```bash
cargo test -p paigasus-helikon-providers-openai --test chat_streaming
```

Expected: PASS, with **no changes to any assertion**. If an assertion needs changing, stop — that means the re-shaping altered translated output, which it must not.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-providers-openai/tests/fixtures/chat_parallel_tool_calls.txt \
        crates/paigasus-helikon-providers-openai/tests/fixtures/chat_content_filter.txt
git commit -m "test(providers-openai): SMA-522 re-shape fixtures to real wire envelope

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Guard the error and cancellation exit paths

These pin decisions the spec made explicit: a buffered `Finish` is discarded on both error and cancellation. They pass before and after the fix — they protect the fix from regressing, and are not evidence the bug existed.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/tests/cancellation.rs`
- Modify: `crates/paigasus-helikon-providers-openai/tests/chat_streaming.rs`

**Interfaces:**
- Consumes: `ChatTranslator::finish()` from Task 1; `run()` from `chat_streaming.rs:25`.
- Produces: nothing.

- [ ] **Step 1: Add the malformed-trailing-chunk test**

Append to `crates/paigasus-helikon-providers-openai/tests/chat_streaming.rs`. It needs a raw-body runner, since `run()` unwraps every item:

```rust
/// SMA-522: when the stream errors AFTER the finish chunk, the buffered
/// Finish is discarded. Yielding `Finish` and then `Err` would place an item
/// after the terminal event — the exact thing this fix exists to prevent.
#[tokio::test]
async fn parse_error_after_finish_chunk_yields_err_and_no_finish() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        // Trailing usage chunk missing the required `object` field.
        "data: {\"id\":\"x\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{}}],\"usage\":{\"prompt_tokens\":1,",
        "\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.as_bytes(), "text/event-stream"))
        .mount(&server)
        .await;

    let model = OpenAiModel::chat("gpt-4o")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();

    let mut req = ModelRequest::new();
    req.messages = vec![user("hi")];

    let items: Vec<_> = model
        .invoke(req, CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await;

    assert!(
        items.iter().any(|r| r.is_err()),
        "expected a transport/parse error, got {items:#?}"
    );
    assert!(
        !items
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .any(|e| matches!(e, ModelEvent::Finish { .. })),
        "buffered Finish must be discarded when the stream errors, got {items:#?}"
    );
}
```

If the malformed chunk turns out to be tolerated by `async-openai` rather than producing an `Err`, do **not** weaken the assertion — instead make the chunk unambiguously invalid (e.g. `data: {not json}`) and keep both assertions.

- [ ] **Step 2: Assert exactly-one-`Finish` on the normal path, and document why the cancel race is not integration-testable**

The spec asked for a "cancel fires after the finish chunk" test. **It is not reachable with wiremock**: `ResponseTemplate::set_delay` delays the *entire* response, so there is no way to deliver the finish chunk and then stall before EOF. Writing one anyway would produce a test whose timing decides what it asserts — a flaky gate, which is worse than no gate.

Do **not** write that test. Instead add a deterministic one that pins the adjacent guarantee, appended to `crates/paigasus-helikon-providers-openai/tests/cancellation.rs`:

```rust
/// SMA-522: the finish reason is buffered until end-of-stream, so a stream
/// that completes normally must still emit exactly one Finish — the buffer
/// is neither dropped nor double-flushed.
///
/// The complementary case (cancel firing AFTER the finish chunk but BEFORE
/// EOF) is deliberately not tested here: wiremock's `set_delay` delays the
/// whole response, so that interleaving is unreachable and any test of it
/// would assert whatever the scheduler happened to do. It is guaranteed
/// structurally instead — the `tokio::select!` cancel arms in
/// `backend/chat.rs` `return` without calling `translator.finish()`.
#[tokio::test]
async fn uncancelled_stream_emits_exactly_one_finish() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n",
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"gpt-4o\",",
        "\"choices\":[{\"index\":0,\"delta\":{}}],\"usage\":{\"prompt_tokens\":1,",
        "\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.as_bytes(), "text/event-stream"))
        .mount(&server)
        .await;

    let model = OpenAiModel::chat("gpt-4o")
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

This test needs `ModelEvent` in scope; `cancellation.rs:6` already imports it.

- [ ] **Step 3: Run the tests**

```bash
cargo test -p paigasus-helikon-providers-openai --test chat_streaming --test cancellation
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-openai/tests/chat_streaming.rs \
        crates/paigasus-helikon-providers-openai/tests/cancellation.rs
git commit -m "test(providers-openai): SMA-522 guard error and cancel exit paths

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Correct the misread contract comments and pin fixture line endings

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/responses.rs:274-278`
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/responses.rs:441-447` (doc on `terminal_events`)
- Modify: `.gitattributes`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing.

- [ ] **Step 1: Fix the Responses `consume` doc comment**

Replace lines 274-278 of `crates/paigasus-helikon-providers-openai/src/backend/responses.rs`:

```rust
    /// Event ordering: `Usage` and `Finish` are built together from a single
    /// terminal event's own data (see [`terminal_events`]), so they cannot be
    /// split across chunks the way the Chat Completions backend's could.
    /// Per `paigasus_helikon_core::Model::invoke`, only `Finish` is
    /// positionally constrained — `Usage` may appear anywhere.
```

- [ ] **Step 2: Record the durable invariant on `terminal_events`**

Append to the doc comment on `terminal_events` (before the `fn` line at 448):

```rust
/// **Invariant (SMA-522):** `Usage` is constructed *only* here, and this
/// function unconditionally appends `Finish` before returning. That — not the
/// incidental fact that both derive from one event — is what keeps the
/// backend's ordering correct. A future arm emitting `Usage` elsewhere would
/// break it.
```

- [ ] **Step 3: Pin the OpenAI fixture directory to LF**

Append to `.gitattributes`:

```gitattributes
# OpenAI SSE fixtures: served byte-for-byte to a real SSE parser by wiremock.
# CRLF would still parse (SSE accepts \r\n), so this is consistency and
# defence for a future test that splits on literal \n -- not a fix for a
# present failure. See SMA-522.
crates/paigasus-helikon-providers-openai/tests/fixtures/*.txt text eol=lf
```

- [ ] **Step 4: Verify the docs gate passes**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-openai --all-features --no-deps
```

Expected: clean. Intra-doc links from a `pub(crate)` item to another `pub(crate)` item are fine; do not add a `[`link`]` from any `pub` item to a private one.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-openai/src/backend/responses.rs .gitattributes
git commit -m "docs(providers-openai): SMA-522 correct usage/finish ordering comments

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Full CI gate parity

**Files:** none modified unless a gate fails.

- [ ] **Step 1: Run every CI gate exactly as CI does**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Run `cargo test --workspace --all-features` **exactly as written** — not per-crate. Feature unification across the workspace has previously surfaced failures that per-crate runs hide.

- [ ] **Step 2: Interpret a macOS bedrock failure correctly**

If ~48 bedrock tests fail with a `NATIVE_ROOTS` / TLS error, that is a known environment artefact of the checkout path, not a regression from this work. Confirm by checking whether the failures mention bedrock TLS root loading and whether they reproduce on `main`. Report it; do not "fix" it.

- [ ] **Step 3: Commit any fixes**

If a gate required changes, commit them with a `fix(providers-openai): SMA-522 …` or `style(providers-openai): SMA-522 …` message.

---

## Self-Review

**Spec coverage:**

| Spec item | Task |
| --- | --- |
| Buffer finish reason; add `finish()` | 1 |
| Driver flushes on stream-exhausted arm only | 1 |
| `tracing::debug!` on empty `finish()` | 1 |
| `tracing::debug!` on second distinct finish reason | 1 |
| Stale "async-openai 0.40" sweep | 1 |
| `chat.rs:237-241` comment correction | 1 (Step 7) |
| One `Finish` per stream, last-wins | 2 |
| Truncated stream emits no `Finish` | 2 |
| Captured anchor fixture + provenance | 3 |
| `"choices":[]` first-party variant | 3 |
| Regression test with real token counts | 3 |
| Mutation check (observed failing) | 1 (Step 2), 2 (Step 4), 3 (Step 5) |
| Re-shape both existing fixtures | 4 |
| `chat_wire.rs` third instance | 1 (Step 1) |
| Error arm discards buffered `Finish` | 5 |
| Cancel-after-finish guard | 5 — **deviation:** not integration-testable with wiremock (`set_delay` delays the whole response), so it is guaranteed structurally by the untouched cancel arms and replaced with a deterministic exactly-one-`Finish` test. Flagged rather than silently dropped. |
| `responses.rs:274-278` correction | 6 |
| `terminal_events` invariant comment | 6 |
| `.gitattributes` LF pin | 6 |
| Full CI gate parity | 7 |
| No version bump / README / mdBook | Global Constraints |

Follow-up Linear issues (Anthropic truncation gap, Bedrock comments, cross-provider conformance) are filed at PR time, not implemented here.

**Placeholder scan:** No TBD/TODO. Every code step carries literal code. Two steps are conditional, and both state a concrete decision rule rather than deferring judgement: Task 2 Step 2 (`serde_json` visibility — check, add only if absent) and Task 5 Step 1 (if `async-openai` tolerates the malformed chunk, make it unambiguously invalid rather than weakening the assertion).

**Type consistency:** `ChatTranslator::finish(&mut self) -> Vec<ModelEvent>` is defined in Task 1 and used identically in Tasks 2, 3, 5. `finish_reason: Option<FinishReason>` is the only new field. `run(fixture: &str) -> Vec<ModelEvent>` is the pre-existing helper at `chat_streaming.rs:25`, used unchanged in Task 3. `stream_chunk(&str) -> CreateChatCompletionStreamResponse` is defined in Task 2 and used only there.
