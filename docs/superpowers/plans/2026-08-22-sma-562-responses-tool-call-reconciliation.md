# SMA-562 Responses Tool-Call Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the OpenAI Responses translator emit a `ToolCallDelta` for every function call the API describes, so `Finish { ToolCalls }` and the emitted tool calls can never disagree.

**Architecture:** One private helper on `ResponsesTranslator` holds the emission rule (register → skip-if-incomplete → skip-if-already-emitted → emit). Two call sites use it: a new `response.output_item.done` arm (the early, per-item point) and the existing `response.completed` arm, which reconciles against `response.output` before building the terminal `[Usage, Finish]` pair. Both share one dedup key (`name_emitted`), so they compose idempotently.

**Tech Stack:** Rust 2024, `async-openai` 0.41.3 (`types::responses`), `tokio`, `wiremock`, `serde_json`.

**Spec:** `docs/superpowers/specs/2026-08-22-sma-562-responses-output-item-done-design.md`

## Global Constraints

- **Crate:** all changes are in `crates/paigasus-helikon-providers-openai` except one comment fix in `tests/provider-stream-conformance/tests/conformance.rs`.
- **MSRV `1.94`**, edition 2024. Workspace inheritance is mandatory — touch no `[package]` metadata.
- **`missing_docs` is `warn` workspace-wide and CI runs `RUSTDOCFLAGS="-D warnings"`.** Every new public item needs a `///`. The helper here is private, so `//` or `///` both compile — use `///`.
- **Commit format:** `<type>(<scope>): SMA-562 <lowercase message>`. Valid scopes come from `.versionrc`; use `providers-openai` for code, `spec`/`plan` for docs. `docs(plans)` is rejected — the scope is `plan` (singular).
- **Never `git add -A`.** `.env` and parts of `.claude/` are untracked but not ignored. Stage explicit paths only, and verify with `git show --stat`.
- **Run `cargo fmt --all` and clippy before every commit.** The `pre-commit` hook is a deliberate no-op; `pre-push` runs fmt + full-workspace clippy and is slow.
- **Do not modify** `terminal_events`' signature or its body. SMA-522's invariant is that `terminal_events` is the sole constructor of `Usage` and always appends `Finish` last. New code emits `ToolCallDelta` only.
- **The raw captures live outside the worktree** at `../captures-sma-562/` (relative to the worktree root). They are reference material and must never be committed.

---

## File Structure

| File | Responsibility | Task |
|------|----------------|------|
| `crates/paigasus-helikon-providers-openai/src/backend/responses.rs` | The translator. Gains one private helper + one new match arm + a rewritten `ResponseCompleted` arm, plus unit tests. | 1–4 |
| `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call_zero_args.txt` | New CAPTURED fixture (§2.1). | 5 |
| `crates/paigasus-helikon-providers-openai/tests/responses_streaming.rs` | Gains the zero-args regression pin. | 5 |
| `crates/paigasus-helikon-providers-openai/tests/live.rs` | Gains the `#[ignore]` live assertion. | 5 |
| `tests/provider-stream-conformance/tests/conformance.rs` | One stale comment corrected (`:2968-2971`). | 6 |

---

## Task 1: The reconciliation helper and the `output_item.done` arm

Delivers spec §4 step A and the helper. After this task the §2.2 shape works; §1's shape does not yet (that is Task 2).

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/responses.rs`
- Test: same file, `#[cfg(test)] mod tests` (starts at `:517`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `impl ResponsesTranslator { fn emit_call_if_unseen(&mut self, item: &OutputItem) -> Option<ModelEvent> }` — private. Task 2 calls it from the `ResponseCompleted` arm.

- [ ] **Step 1: Add the test helpers the new tests need**

Add to `mod tests`, next to the existing `added_event` / `delta_event` helpers (`:526-550`). Note `OutputStatus` must be added to the test module's `use` list.

```rust
    fn function_item(item_id: &str, call_id: &str, name: &str, arguments: &str) -> OutputItem {
        OutputItem::FunctionCall(FunctionToolCall {
            arguments: arguments.to_owned(),
            call_id: call_id.to_owned(),
            namespace: None,
            name: name.to_owned(),
            id: Some(item_id.to_owned()),
            status: Some(OutputStatus::Completed),
        })
    }

    fn done_event(item_id: &str, call_id: &str, name: &str, arguments: &str) -> ResponseStreamEvent {
        ResponseStreamEvent::ResponseOutputItemDone(ResponseOutputItemDoneEvent {
            sequence_number: 2,
            output_index: 0,
            item: function_item(item_id, call_id, name, arguments),
        })
    }
```

Extend the test module's existing `use async_openai::types::responses::{...}` (`:519-522`) to:

```rust
    use async_openai::types::responses::{
        FunctionToolCall, OutputItem, OutputStatus, ResponseFunctionCallArgumentsDeltaEvent,
        ResponseOutputItemAddedEvent, ResponseOutputItemDoneEvent, ResponseStreamEvent,
    };
```

- [ ] **Step 2: Write the failing test**

Add to `mod tests`. This is spec §6 test 1 — the §2.2 captured shape, with the real ids from `../captures-sma-562/resume-raw.txt`.

```rust
    /// SMA-562 §2.2 — a resumed background stream describes a tool call
    /// entirely on `output_item.done`: no `output_item.added`, no argument
    /// deltas. Ids and arguments are transcribed from the 2026-08-22 capture
    /// (`GET /v1/responses/{id}?stream=true&starting_after=9`).
    ///
    /// Before this fix the translator emitted nothing here, because
    /// `ToolCallDelta` came only from the argument-delta path.
    #[test]
    fn done_without_added_or_deltas_emits_tool_call() {
        let mut t = ResponsesTranslator::new();

        let evs = t
            .consume(done_event(
                "fc_0aae0e68b2ddb1af006a89d1c124c487d293f21c9ada8e4e5d",
                "call_lQOsuE9Lx2s6d70xJ88uClEk",
                "get_weather",
                "{\"city\":\"Berlin\"}",
            ))
            .unwrap();

        assert_eq!(evs.len(), 1, "expected one ToolCallDelta, got {evs:?}");
        match &evs[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "call_lQOsuE9Lx2s6d70xJ88uClEk");
                assert_eq!(name.as_deref(), Some("get_weather"));
                assert_eq!(args_delta, "{\"city\":\"Berlin\"}");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
    }

    /// The dedup guard: once the argument deltas have carried the arguments,
    /// `output_item.done` repeats the WHOLE string, so emitting it again would
    /// duplicate them. Passes before the fix only because the arm did not
    /// exist; it must keep passing after.
    #[test]
    fn done_after_deltas_does_not_double_emit() {
        let mut t = ResponsesTranslator::new();

        t.consume(added_event("fc_1", "call_1", "get_weather")).unwrap();
        t.consume(delta_event("fc_1", "{\"city\":")).unwrap();
        t.consume(delta_event("fc_1", "\"Berlin\"}")).unwrap();

        let evs = t
            .consume(done_event("fc_1", "call_1", "get_weather", "{\"city\":\"Berlin\"}"))
            .unwrap();

        assert!(
            evs.is_empty(),
            "done must not re-emit arguments already carried by deltas; got {evs:?}"
        );
    }

    /// A truncated turn can carry `status: "incomplete"` with partial,
    /// unparseable `arguments`. Emitting it would make
    /// `ModelTurnAccumulator::finish` fail the ENTIRE turn on a serde_json
    /// error, which is strictly worse than today's silent drop. See spec §4.3.
    #[test]
    fn incomplete_status_item_is_not_emitted() {
        let mut t = ResponsesTranslator::new();

        let evs = t
            .consume(ResponseStreamEvent::ResponseOutputItemDone(
                ResponseOutputItemDoneEvent {
                    sequence_number: 2,
                    output_index: 0,
                    item: OutputItem::FunctionCall(FunctionToolCall {
                        arguments: "{\"cit".to_owned(),
                        call_id: "call_trunc".to_owned(),
                        namespace: None,
                        name: "get_weather".to_owned(),
                        id: Some("fc_trunc".to_owned()),
                        status: Some(OutputStatus::Incomplete),
                    }),
                },
            ))
            .unwrap();

        assert!(
            evs.is_empty(),
            "an incomplete item must not be emitted; got {evs:?}"
        );
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run:
```bash
cargo test -p paigasus-helikon-providers-openai --lib backend::responses::tests 2>&1 | tail -30
```

Expected: `done_without_added_or_deltas_emits_tool_call` **FAILS** with
`expected one ToolCallDelta, got []` — the `output_item.done` event falls through to the
trailing `other` arm and yields nothing.
`done_after_deltas_does_not_double_emit` and `incomplete_status_item_is_not_emitted`
**PASS** vacuously (same reason). Record the failure text; it goes in the commit message.

- [ ] **Step 4: Write the helper**

Add as a new method inside `impl ResponsesTranslator`, immediately **before** `consume`
(which starts at `:283`):

```rust
    /// Emit a `ToolCallDelta` for `item` unless one has already been emitted
    /// for it.
    ///
    /// This is the single place the reconciliation rule lives; both
    /// `response.output_item.done` and `response.completed` call it, so they
    /// compose idempotently — whichever arrives first emits, the other sees
    /// `name_emitted` and returns `None`.
    ///
    /// Returns `None` for anything that is not a complete function call:
    /// - a non-`FunctionCall` item (`reasoning`, `message`, hosted-tool calls);
    /// - `id: None` — `item_id` is the dedup correlator, and without it we
    ///   cannot tell a fresh call from one whose deltas already streamed;
    /// - `status: Some(Incomplete)` — a truncated turn's `arguments` is a
    ///   partial JSON string, and emitting it would fail the whole turn in
    ///   `ModelTurnAccumulator::finish` rather than dropping one call;
    /// - a call already emitted, per `name_emitted`.
    ///
    /// Emits no `Usage`, so SMA-522's ordering invariant is untouched.
    fn emit_call_if_unseen(&mut self, item: &OutputItem) -> Option<ModelEvent> {
        let OutputItem::FunctionCall(fc) = item else {
            return None;
        };
        let Some(item_id) = fc.id.clone() else {
            tracing::debug!(
                target: "paigasus::openai::responses",
                call_id = %fc.call_id,
                "function_call item carries no `id`; cannot dedup, so not emitting"
            );
            return None;
        };

        // Register regardless of whether we emit, so `has_tool_calls` is right
        // even when `output_item.added` never arrived.
        self.item_to_call
            .entry(item_id.clone())
            .or_insert_with(|| (fc.call_id.clone(), fc.name.clone()));

        if matches!(fc.status, Some(OutputStatus::Incomplete)) {
            tracing::debug!(
                target: "paigasus::openai::responses",
                item_id = %item_id,
                "function_call item is incomplete; arguments may be truncated, not emitting"
            );
            return None;
        }

        if self.name_emitted.contains(&item_id) {
            return None;
        }

        // `done`/`output` carry the COMPLETE arguments string, so a buffer of
        // out-of-order deltas is redundant — except when the terminal item
        // reports empty arguments and the buffer does not, in which case the
        // buffer is the better data.
        let buffered = self.pending_args.remove(&item_id).unwrap_or_default();
        let args_delta = if fc.arguments.is_empty() && !buffered.is_empty() {
            buffered
        } else {
            fc.arguments.clone()
        };

        self.name_emitted.insert(item_id);
        Some(ModelEvent::ToolCallDelta {
            call_id: fc.call_id.clone(),
            name: Some(fc.name.clone()),
            args_delta,
        })
    }
```

Add `OutputStatus` to the file's top-level import (`:9-14`):

```rust
use async_openai::types::responses::{
    CreateResponse, FunctionTool, InputItem, InputParam, OutputItem, OutputStatus,
    ResponseFormatJsonSchema, ResponseStreamEvent, ResponseTextParam, ResponseUsage, Status,
    TextResponseFormatConfiguration, Tool, ToolChoiceOptions, ToolChoiceParam,
};
```

- [ ] **Step 5: Add the `output_item.done` arm**

Insert into the `match` in `consume`, directly **after** the `ResponseOutputItemAdded` arm
(which ends at `:355` with `Ok(vec![])`):

```rust
            // Output item done — the item's authoritative terminal description,
            // carrying `call_id`, `name` and the COMPLETE `arguments` together.
            //
            // This is the earliest point a call that streamed no argument
            // deltas is fully known (SMA-562 §2.2: a resumed background stream
            // has no `output_item.added` and no deltas at all). The dedup in
            // `emit_call_if_unseen` makes it a no-op on the ordinary path,
            // where the deltas already carried the arguments.
            //
            // Assumes `done` is terminal for its item: a delta arriving after
            // it would append to an already-complete args string. Not observed
            // on the wire — a resumed stream truncates a prefix, preserving
            // order.
            ResponseStreamEvent::ResponseOutputItemDone(e) => {
                Ok(self.emit_call_if_unseen(&e.item).into_iter().collect())
            }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run:
```bash
cargo test -p paigasus-helikon-providers-openai --lib backend::responses::tests 2>&1 | tail -30
```

Expected: PASS, all tests in the module, including the three pre-existing
`ordered_added_before_delta` / `out_of_order_delta_before_added` /
`multiple_orphan_deltas_concatenated`.

- [ ] **Step 7: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-openai/src/backend/responses.rs
git commit -m "fix(providers-openai): SMA-562 emit responses tool calls from output_item.done"
git show --stat HEAD
```

---

## Task 2: Reconcile at `response.completed`

Delivers spec §4 step B — the half that closes the invariant. This is the task that fixes the shape SMA-562 literally reports.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/responses.rs` (the `ResponseCompleted` arm, `:397-402` before Task 1's edits shift it)
- Test: same file, `mod tests`

**Interfaces:**
- Consumes: `ResponsesTranslator::emit_call_if_unseen` from Task 1.
- Produces: nothing new.

- [ ] **Step 1: Add the `completed_event` test helper**

Add to `mod tests`. Building a `Response` by struct literal means ~40 fields; deserializing
the minimal JSON is both shorter and closer to the wire. **This exact JSON is verified to
parse** (probe run 2026-08-22): every non-listed field on `Response` is `Option` and
defaults to `None`.

```rust
    /// Build a `response.completed` event carrying `output_json` as its
    /// `output` array.
    ///
    /// Deserialized rather than struct-literal'd: `Response` has six required
    /// fields (`id`, `object`, `created_at`, `model`, `status`, `output`) and
    /// ~40 optional ones, so this is both shorter and closer to the wire.
    fn completed_event(output_json: &str) -> ResponseStreamEvent {
        let raw = format!(
            r#"{{"type":"response.completed","sequence_number":9,"response":{{
                "id":"resp_test","object":"response","created_at":1787416935,
                "model":"gpt-4o-mini","status":"completed","output":{output_json},
                "usage":{{"input_tokens":57,"input_tokens_details":{{"cached_tokens":0}},
                "output_tokens":2,"output_tokens_details":{{"reasoning_tokens":0}},
                "total_tokens":59}}}}}}"#
        );
        serde_json::from_str(&raw).expect("completed_event JSON must parse")
    }
```

- [ ] **Step 2: Write the failing test**

Spec §6 test 2 — the counter-example that a `done`-only fix still fails, plus the parallel
guard (test 4) and the incomplete guard on this path (test 5).

```rust
    /// SMA-562 §3 — the shape the ticket literally reports, and the reason
    /// `output_item.done` alone is not the fix: `output_item.added` registers
    /// the call into `item_to_call`, no argument deltas stream, and NO
    /// `output_item.done` arrives. `terminal_events` sees a non-empty
    /// `item_to_call` and reports `ToolCalls` for a call that was never
    /// emitted.
    ///
    /// This sequence is SYNTHETIC. §2.1's captures could not produce it — a
    /// zero-argument tool streams one `"{}"` delta on every model tried — so
    /// unlike the §2.2 fixture this one is constructed, and is labelled as
    /// such per this crate's fixture-provenance discipline.
    #[test]
    fn added_then_completed_without_deltas_emits_tool_call() {
        let mut t = ResponsesTranslator::new();

        assert!(t.consume(added_event("fc_1", "call_1", "get_weather")).unwrap().is_empty());

        let evs = t
            .consume(completed_event(
                r#"[{"id":"fc_1","type":"function_call","status":"completed",
                     "arguments":"{\"city\":\"Berlin\"}","call_id":"call_1",
                     "name":"get_weather"}]"#,
            ))
            .unwrap();

        let deltas: Vec<_> = evs
            .iter()
            .filter(|e| matches!(e, ModelEvent::ToolCallDelta { .. }))
            .collect();
        assert_eq!(
            deltas.len(),
            1,
            "Finish(ToolCalls) must not be reported without a ToolCallDelta; got {evs:?}"
        );
        match deltas[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(name.as_deref(), Some("get_weather"));
                assert_eq!(args_delta, "{\"city\":\"Berlin\"}");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }

        // The reconciled delta must precede the terminal pair.
        assert!(
            matches!(evs[0], ModelEvent::ToolCallDelta { .. }),
            "ToolCallDelta must come before Usage/Finish; got {evs:?}"
        );
        assert!(
            matches!(
                evs.last(),
                Some(ModelEvent::Finish {
                    reason: FinishReason::ToolCalls
                })
            ),
            "expected Finish(ToolCalls) last, got {evs:?}"
        );
    }

    /// The ordinary path must be unchanged: deltas carried the arguments, so
    /// reconciliation at `completed` finds `name_emitted` set and adds nothing.
    #[test]
    fn completed_after_deltas_emits_only_terminal_pair() {
        let mut t = ResponsesTranslator::new();

        t.consume(added_event("fc_1", "call_1", "get_weather")).unwrap();
        t.consume(delta_event("fc_1", "{\"city\":\"Berlin\"}")).unwrap();

        let evs = t
            .consume(completed_event(
                r#"[{"id":"fc_1","type":"function_call","status":"completed",
                     "arguments":"{\"city\":\"Berlin\"}","call_id":"call_1",
                     "name":"get_weather"}]"#,
            ))
            .unwrap();

        assert!(
            !evs.iter().any(|e| matches!(e, ModelEvent::ToolCallDelta { .. })),
            "completed must not re-emit arguments the deltas already carried; got {evs:?}"
        );
        assert!(matches!(evs[0], ModelEvent::Usage { .. }));
        assert!(matches!(
            evs.last(),
            Some(ModelEvent::Finish {
                reason: FinishReason::ToolCalls
            })
        ));
    }

    /// Parallel tool calls share one stream and are distinguished only by
    /// `item.id`. Every map in the translator keys on it, but nothing asserted
    /// that until now. The cross-provider conformance suite enforces the same
    /// "exactly one named delta per call_id" rule, where a regression would
    /// surface with no unit-level signal.
    #[test]
    fn parallel_calls_emit_one_named_delta_each() {
        let mut t = ResponsesTranslator::new();

        let mut all = Vec::new();
        all.extend(t.consume(added_event("fc_a", "call_a", "get_weather")).unwrap());
        all.extend(t.consume(added_event("fc_b", "call_b", "get_time")).unwrap());
        // Only B streams arguments.
        all.extend(t.consume(delta_event("fc_b", "{}")).unwrap());
        all.extend(t.consume(done_event("fc_a", "call_a", "get_weather", "{\"city\":\"Rome\"}")).unwrap());
        all.extend(t.consume(done_event("fc_b", "call_b", "get_time", "{}")).unwrap());
        all.extend(
            t.consume(completed_event(
                r#"[{"id":"fc_a","type":"function_call","status":"completed",
                     "arguments":"{\"city\":\"Rome\"}","call_id":"call_a","name":"get_weather"},
                    {"id":"fc_b","type":"function_call","status":"completed",
                     "arguments":"{}","call_id":"call_b","name":"get_time"}]"#,
            ))
            .unwrap(),
        );

        let mut named: Vec<(&str, &str)> = all
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id,
                    name: Some(name),
                    ..
                } => Some((call_id.as_str(), name.as_str())),
                _ => None,
            })
            .collect();
        named.sort_unstable();
        assert_eq!(
            named,
            vec![("call_a", "get_weather"), ("call_b", "get_time")],
            "expected exactly one named delta per call_id, got {all:?}"
        );

        // Args must be attributed to the right call, and A's must not be
        // double-counted between its `done` and the terminal reconciliation.
        let a_args: String = all
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta { call_id, args_delta, .. } if call_id == "call_a" => {
                    Some(args_delta.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(a_args, "{\"city\":\"Rome\"}");
    }

    /// §4.3 on the terminal path: an incomplete item in `response.output` must
    /// not be emitted, because its `arguments` may be truncated JSON.
    #[test]
    fn completed_skips_incomplete_output_item() {
        let mut t = ResponsesTranslator::new();

        let evs = t
            .consume(completed_event(
                r#"[{"id":"fc_1","type":"function_call","status":"incomplete",
                     "arguments":"{\"cit","call_id":"call_1","name":"get_weather"}]"#,
            ))
            .unwrap();

        assert!(
            !evs.iter().any(|e| matches!(e, ModelEvent::ToolCallDelta { .. })),
            "an incomplete item must not be emitted; got {evs:?}"
        );
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run:
```bash
cargo test -p paigasus-helikon-providers-openai --lib backend::responses::tests 2>&1 | tail -40
```

Expected: `added_then_completed_without_deltas_emits_tool_call` **FAILS** with
`Finish(ToolCalls) must not be reported without a ToolCallDelta; got [Usage { .. }, Finish { reason: ToolCalls }]`.
`parallel_calls_emit_one_named_delta_each` **FAILS** too — after Task 1 the `done` arm emits
A, so this one may already pass; if it does, say so rather than claiming a red-to-green
transition it did not have. The other two pass. Record the exact output for the commit
message.

- [ ] **Step 4: Rewrite the `ResponseCompleted` arm**

Replace the existing arm (currently `Ok(terminal_events(e.response.usage, e.response.status, None, !self.item_to_call.is_empty()))`):

```rust
            // Terminal: response completed.
            //
            // Reconcile BEFORE building the terminal pair. `response.output`
            // is the authoritative item list and always carries every function
            // call in full (SMA-562 §2.3, confirmed on four captures), so this
            // is the only point at which "did we emit every call we are about
            // to report?" is decidable. Emitting here makes the post-condition
            // an invariant: `Finish { ToolCalls }` iff at least one
            // `ToolCallDelta` was emitted.
            //
            // `terminal_events` stays the sole constructor of `Usage` and
            // still appends `Finish` last (SMA-522).
            ResponseStreamEvent::ResponseCompleted(e) => {
                let mut out: Vec<ModelEvent> = e
                    .response
                    .output
                    .iter()
                    .filter_map(|item| self.emit_call_if_unseen(item))
                    .collect();
                if !self.pending_args.is_empty() {
                    tracing::warn!(
                        target: "paigasus::openai::responses",
                        orphans = self.pending_args.len(),
                        "argument deltas were buffered for item_ids that never resolved; dropping"
                    );
                }
                out.extend(terminal_events(
                    e.response.usage,
                    e.response.status,
                    None,
                    !self.item_to_call.is_empty(),
                ));
                Ok(out)
            }
```

Leave the `ResponseIncomplete` arm exactly as it is — spec §4.3.

- [ ] **Step 5: Run the tests to verify they pass**

Run:
```bash
cargo test -p paigasus-helikon-providers-openai --lib backend::responses::tests 2>&1 | tail -20
```

Expected: PASS, every test in the module.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-openai/src/backend/responses.rs
git commit -m "fix(providers-openai): SMA-562 reconcile unemitted tool calls at response.completed"
git show --stat HEAD
```

---

## Task 3: Verify the whole existing suite is green

A safety task with no new code. The translator's behaviour changed on a path five other test files exercise.

**Files:** none modified.

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces: nothing.

- [ ] **Step 1: Run the crate's full suite**

```bash
cargo test -p paigasus-helikon-providers-openai --all-features 2>&1 | tail -40
```

Expected: PASS. Pay attention to `responses_streaming.rs::tool_call_turn_finishes_with_tool_calls`,
which asserts *exactly one* named delta for `call_D3Tp4UJ6scmDWx6jmfvy2LQo` over
`responses_tool_call.txt` — that fixture contains both an `output_item.done` (line 99) and a
`response.completed`, so it exercises both new code paths and is the strongest existing
double-emit guard.

- [ ] **Step 2: Run the cross-provider conformance suite**

```bash
cargo test -p provider-stream-conformance --all-features 2>&1 | tail -30
```

Expected: PASS. Its `openai_responses` subject builds `output_item.done` frames
(`conformance.rs:2968`) that were previously no-ops to the translator and now are not.

- [ ] **Step 3: If anything fails, STOP**

Do not "fix" a conformance assertion to match new behaviour. A failure here means the
double-emit dedup is wrong; report it with the failing assertion and stop.

---

## Task 4: Doc comments

Spec §7 sites 1–5. No behaviour change.

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/responses.rs`

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces: nothing.

- [ ] **Step 1: Update the `ResponsesTranslator` event-list bullets**

In the struct's doc comment (`:215-221` pre-edit), add after the
`response.function_call_arguments.delta` bullet:

```rust
/// - `response.output_item.done` (when the item is a complete function call) →
///   `ToolCallDelta` carrying the item's complete `arguments`, **if no delta has
///   already been emitted for it**. This is what makes a stream that carries no
///   argument deltas at all — a resumed background response — report its tool
///   calls (SMA-562).
```

and replace the `response.completed` bullet with:

```rust
/// - `response.completed` → any function call in `response.output` that has not
///   yet been emitted, then `Usage` + `Finish { Stop }`, or `Finish { ToolCalls }`
///   when `item_to_call` is non-empty. Reconciling against `response.output`
///   before the terminal pair is what makes `Finish { ToolCalls }` and the
///   emitted `ToolCallDelta`s agree by construction rather than by coincidence
///   (SMA-562). A turn whose sole output is a function call still reports
///   `status: "completed"` on the wire (confirmed against real traffic; see
///   `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call.txt`),
///   so `status` alone cannot distinguish the two cases.
```

- [ ] **Step 2: Update the three field doc comments**

`name_emitted` (`:247-250`) — append:

```rust
    /// Also set by the two SMA-562 reconciliation sites (`output_item.done`
    /// and the `response.completed` sweep), which use it as their dedup key.
```

`item_to_call` (`:251-255`) — replace "Populated by `response.output_item.added` when the
item is a function call." with:

```rust
    /// Populated by `response.output_item.added`, and — since SMA-562 — also by
    /// `response.output_item.done` and by the `response.completed` sweep over
    /// `response.output`, so that `has_tool_calls` is correct even for a stream
    /// that never carried an `added` event.
```

`pending_args` (`:257-262`) — append:

```rust
    /// Since SMA-562 a buffer may instead be *discarded* by
    /// `emit_call_if_unseen`, which prefers the complete `arguments` string the
    /// terminal item carries; the buffer wins only when that string is empty.
    /// Buffers still unresolved at `response.completed` are logged and dropped.
```

- [ ] **Step 3: Verify docs build clean**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-openai --all-features --no-deps 2>&1 | tail -20
```

Expected: no warnings. **Do not** use intra-doc links (`[`Self::emit_call_if_unseen`]`) from
a public item to the private helper — `rustdoc::private_intra_doc_links` fails this gate.
Refer to it in prose.

- [ ] **Step 4: Format and commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-openai/src/backend/responses.rs
git commit -m "docs(providers-openai): SMA-562 document the tool-call reconciliation path"
```

---

## Task 5: The captured fixture, the regression pin, and the live assertion

Spec §6 tests 6 and 7, and §6.1/§6.2.

**Files:**
- Create: `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call_zero_args.txt`
- Modify: `crates/paigasus-helikon-providers-openai/tests/responses_streaming.rs`
- Modify: `crates/paigasus-helikon-providers-openai/tests/live.rs`

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces: nothing.

- [ ] **Step 1: Write the fixture**

Copy the raw capture and prepend the provenance header. The raw bytes are at
`../captures-sma-562/zeroarg-raw.txt` (outside the worktree — read it, do not move it in).

The file is the header block below, then the capture's five events verbatim
(`response.output_item.added`, `response.function_call_arguments.delta`,
`response.function_call_arguments.done`, `response.output_item.done`,
`response.completed`), with `response.created` and `response.in_progress` dropped, matching
every sibling fixture in the directory.

```
: Provenance: CAPTURED against a real https://api.openai.com/v1/responses
: request, model gpt-4o-mini-2024-07-18, on 2026-08-22, with ONE zero-argument
: strict tool:
:
:   {"type":"object","properties":{},"required":[],"additionalProperties":false}
:
: and "tool_choice":"required".
:
: This fixture is the NEGATIVE result for SMA-562. That ticket hypothesised
: that a no-argument tool would stream zero `function_call_arguments.delta`
: frames, leaving `Finish{ToolCalls}` with no `ToolCallDelta` to match it. It
: does not: the API streams exactly one delta carrying the literal `{}`. The
: same shape was confirmed on gpt-5-mini. The zero-delta stream SMA-562 fixes
: is real, but it comes from resuming a stored background response past its
: deltas -- which has no `output_item.added` either, so it cannot be served
: from this directory's POST-only wiremock harness and is driven as typed
: events in `backend/responses.rs`'s test module instead.
:
: So this file is a REGRESSION PIN, not a reproduction. It pins the behaviour
: the translator already had, so that if OpenAI ever elides the `{}` delta the
: assumption is at least written down. Note what a frozen fixture cannot do:
: notice that change happening. `responses_zero_arg_tool_streams_a_delta` in
: `tests/live.rs` is the assertion that can, and is the reason it exists.
:
: Two deliberate edits from the raw capture:
:
: 1. `response.created` and `response.in_progress` are dropped, matching every
:    other fixture in this directory -- `ResponsesTranslator::consume` has no
:    arm for either.
:
: 2. NOTHING is appended. Unlike every sibling fixture here, this file does NOT
:    end with `data: [DONE]`, because the endpoint did not send one -- none of
:    the four 2026-08-22 captures contains it. Verified before transcription:
:    served through this directory's harness both byte-faithful and with the
:    sentinel appended, the stream terminates identically and cleanly either
:    way (async-openai ends the stream on body end). The byte-faithful form is
:    kept so the CAPTURED label stays honest.
:
:    This leaves `responses_tool_call.txt`'s own trailing `[DONE]` unexplained:
:    it claims faithful transcription of the same endpoint. Auditing that
:    fixture's provenance is a follow-up, not this ticket.
:
: `event:` lines are kept, as in `responses_tool_call.txt` and for the same
: reason -- async-openai dispatches purely off the `type` key inside `data:`,
: so they are cosmetic to the parser, but a fixture claiming to be CAPTURED
: should stay faithful to what the wire sent.
```

- [ ] **Step 2: Write the failing test**

In `tests/responses_streaming.rs`, add the const next to the others (`:15-19`):

```rust
const ZERO_ARGS: &str = include_str!("fixtures/responses_tool_call_zero_args.txt");
```

and the test:

```rust
/// SMA-562's negative result, pinned. A zero-argument tool call streams one
/// `function_call_arguments.delta` carrying `"{}"` — it does NOT stream zero
/// deltas, which is what the ticket hypothesised. See the fixture header.
#[tokio::test]
async fn zero_argument_tool_streams_one_delta() {
    let events = run(ZERO_ARGS).await;
    let unwrapped: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    let named: Vec<(&str, &str, &str)> = unwrapped
        .iter()
        .filter_map(|e| match e {
            ModelEvent::ToolCallDelta {
                call_id,
                name: Some(name),
                args_delta,
            } => Some((call_id.as_str(), name.as_str(), args_delta.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        named,
        vec![(
            "call_8xWY1dceitU93bp87yBo1ocG",
            "get_current_time",
            "{}"
        )],
        "expected exactly one named ToolCallDelta carrying `{{}}`, got {unwrapped:?}"
    );

    assert!(
        matches!(
            unwrapped.last(),
            Some(ModelEvent::Finish {
                reason: FinishReason::ToolCalls
            })
        ),
        "expected Finish(ToolCalls) last, got {:?}",
        unwrapped.last()
    );
}
```

- [ ] **Step 3: Run it**

```bash
cargo test -p paigasus-helikon-providers-openai --test responses_streaming 2>&1 | tail -20
```

Expected: PASS. This test pins pre-existing behaviour, so it does not go red-to-green — say
so in the commit message rather than implying otherwise. If it FAILS, the reconciliation is
double-emitting: the fixture has both an `output_item.done` and a `response.completed`
carrying the same call, so a broken dedup shows up here as three named deltas instead of one.

- [ ] **Step 4: Add the live assertion**

In `tests/live.rs`, after `responses_smoke` (`:47-61`):

```rust
/// The assertion a frozen fixture cannot make: that the live API still streams
/// at least one `function_call_arguments.delta` for a zero-argument tool.
///
/// `responses_tool_call_zero_args.txt` pins the 2026-08-22 capture, but a
/// recording cannot notice upstream behaviour changing. If OpenAI ever elides
/// the `"{}"` delta, this is where it surfaces. The translator handles that
/// case correctly since SMA-562 — this test exists so the change is *seen*,
/// not so it breaks anything.
#[tokio::test]
#[ignore]
async fn responses_zero_arg_tool_streams_a_delta() {
    if !key_set() {
        return;
    }
    let model = OpenAiModel::responses("gpt-4o-mini").build().unwrap();
    let mut req = ModelRequest::new();
    req.messages = vec![user("What time is it right now? Use the tool.")];
    req.tools = vec![ToolDef {
        name: "get_current_time".to_owned(),
        description: "Return the current server time. Takes no arguments.".to_owned(),
        schema: serde_json::json!({"type": "object", "properties": {}}),
    }];
    let stream = model.invoke(req, CancellationToken::new()).await.unwrap();
    let events: Vec<_> = stream.collect().await;

    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|r| match r {
            Ok(ModelEvent::ToolCallDelta { args_delta, .. }) => Some(args_delta.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !deltas.is_empty(),
        "a zero-argument tool call emitted no ToolCallDelta at all; got {events:#?}"
    );
}
```

- [ ] **Step 5: Verify it compiles and, if a key is present, run it**

```bash
cargo test -p paigasus-helikon-providers-openai --test live --no-run 2>&1 | tail -5
```

Then, only if `OPENAI_API_KEY` is set in the environment:

```bash
cargo test -p paigasus-helikon-providers-openai --test live -- --ignored responses_zero_arg 2>&1 | tail -20
```

Expected: PASS (or a silent skip with no key). Cost ~$0.0001.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call_zero_args.txt \
        crates/paigasus-helikon-providers-openai/tests/responses_streaming.rs \
        crates/paigasus-helikon-providers-openai/tests/live.rs
git commit -m "test(providers-openai): SMA-562 pin the zero-argument tool-call wire shape"
git show --stat HEAD
```

---

## Task 6: Correct the stale conformance-suite comment

Spec §7 site 6. One comment; no scenario is added.

**Files:**
- Modify: `tests/provider-stream-conformance/tests/conformance.rs:2967-2971`

**Interfaces:**
- Consumes: Tasks 1–2.
- Produces: nothing.

- [ ] **Step 1: Replace the comment**

The current text claims `output_item.done` is a no-op to the translator. Since SMA-562 it is
not. Replace:

```rust
    /// `response.output_item.done`, the completed mirror of
    /// [`output_item_added`]. Matches `responses_tool_call.txt` line 99.
    ///
    /// No longer a no-op to the translator: since SMA-562 this event emits a
    /// `ToolCallDelta` carrying the item's complete `arguments` when no
    /// argument delta has already been emitted for it. In THIS scenario the
    /// deltas always arrive first, so it stays a no-op here specifically —
    /// which is the property the double-emit dedup guarantees, and worth
    /// noticing if this suite ever goes red on a duplicated tool call.
    fn output_item_done(item_id: &str, call_id: &str, name: &str, arguments: &str) -> Vec<u8> {
```

- [ ] **Step 2: Verify**

```bash
cargo test -p provider-stream-conformance --all-features 2>&1 | tail -10
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
cargo fmt --all
git add tests/provider-stream-conformance/tests/conformance.rs
git commit -m "docs(conformance): SMA-562 correct the output_item.done no-op caveat"
```

---

## Task 7: Full CI gate replication

**Files:** none modified (unless a gate fails).

**Interfaces:**
- Consumes: Tasks 1–6.
- Produces: a branch ready for Stage 5.

- [ ] **Step 1: Run every gate CI runs**

Run each; all must be clean. The test gate is the one that matters most — run the **exact**
workspace-wide command, not a per-crate subset (a per-crate run has masked a
`--all-features` interaction before).

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

- [ ] **Step 2: Check the commit messages pass convco**

```bash
convco check "$(git merge-base origin/main HEAD)"..HEAD
```

Expected: clean. Note the baseline is a **merge-base**, not `origin/main`'s tip.

- [ ] **Step 3: Confirm nothing stray is staged or untracked**

```bash
git status --short
git log --oneline "$(git merge-base origin/main HEAD)"..HEAD
```

Expected: a clean tree, and six commits (spec, Task 1, Task 2, Task 4, Task 5, Task 6). No
`.capture/`, no `scratch_*` files, no `.env`.

---

## Self-Review

**Spec coverage:**

| Spec section | Task |
|---|---|
| §4 helper + step A (`output_item.done`) | 1 |
| §4 step B (`response.completed` reconciliation) | 2 |
| §4.1 dedup key | 1 (helper), asserted by Task 2's `completed_after_deltas_emits_only_terminal_pair` |
| §4.2 `id: None` skipped | 1 (helper + doc) |
| §4.3 incomplete not reconciled | 1 (`incomplete_status_item_is_not_emitted`), 2 (`completed_skips_incomplete_output_item`) |
| §4.4 assumptions, orphan warn, verbatim args | 1 (arm comment), 2 (warn) |
| §5 no conformance scenario | 6 (comment only) |
| §6 tests 1–5 | 1, 2 |
| §6 tests 6–7 | 5 |
| §6.1 fixture | 5 |
| §6.2 `[DONE]` decision | 5 (header) |
| §7 doc sites 1–5 | 4 |
| §7 doc site 6 | 6 |
| §8 release | nothing to do — normal release-plz flow |
| §9 capture disposition | Task 7 step 3 verifies nothing leaked |

No gaps.

**Placeholder scan:** none — every step carries the literal code or command.

**Type consistency:** `emit_call_if_unseen(&mut self, item: &OutputItem) -> Option<ModelEvent>`
is defined in Task 1 step 4 and called in Task 1 step 5 and Task 2 step 4 with that exact
signature. Test helpers `function_item` / `done_event` (Task 1 step 1) and `completed_event`
(Task 2 step 1) are each defined before first use; `done_event` is reused in Task 2's
`parallel_calls_emit_one_named_delta_each`. `OutputStatus` is imported in both the module
(Task 1 step 4) and the test module (Task 1 step 1).
