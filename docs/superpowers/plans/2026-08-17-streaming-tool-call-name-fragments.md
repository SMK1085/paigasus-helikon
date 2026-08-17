# SMA-547 Streaming Tool-Call Name Fragments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop dropping tool-call `name` fragments that arrive after the call id resolves, in `providers-openai`'s chat backend and `providers-litellm`, by deferring name emission until the translator can establish the name is complete.

**Architecture:** Each translator already buffers pre-id fragments in a per-call `PendingToolCall` / `Pending` struct that is drained wholesale on the first post-id delta. That struct's two fields acquire different lifecycles: `args` stays drain-once, `name` accumulates across every delta and is cleared only by a flush. A name flushes when either completion signal appears — this delta carried a non-empty `arguments` fragment, or this delta carried no `name` fragment — and any name still buffered at end-of-stream flushes from `finish()`, ordered before `Finish`.

**Tech Stack:** Rust 2024, MSRV 1.94. `async-openai` typed chunks (openai) vs. hand-rolled serde types over `eventsource-stream` (litellm). Tests: in-module `#[test]` units plus `wiremock`-driven fixture tests.

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-08-17-sma-547-streaming-tool-call-name-fragments-design.md`. Read it before starting; this plan implements it and does not restate its rationale.
- **`crates/paigasus-helikon-core/` is NOT touched.** No file under it may be edited. The contract wording is deferred to SMA-533. Touching core triggers a version cascade this ticket deliberately avoids.
- **The two translators must stay behaviourally aligned.** Any divergence needs a stated reason in a code comment.
- **Commit scope must be `providers`** — `providers-litellm` is absent from `.versionrc:18`'s `scopeRegex`, so `feat(providers-litellm)` is rejected by the local `commit-msg` hook. `providers-openai` is allowed but use `providers` for cross-crate commits.
- **Commit subject must start lowercase** after `SMA-547 ` and use a Conventional Commits type.
- **`tracing` target uses the colon form** — `target: "…"`, never `target = "…"` (that is the SMA-543 defect).
- **No version numbers are edited by hand.** Both crates are already released; release-plz bumps them and cascades the facade itself.
- **Run `cargo fmt --all` and clippy before every commit.** The pre-commit hook is a deliberate no-op, so nothing catches formatting until push time.
- **Work synchronously.** Do not background `cargo test` / `cargo build` and end your turn — run them in the foreground and wait for a terminal result.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` | `ChatTranslator` state + flush rule + `finish()` flush + late-fragment warn + its in-module tests | 1 |
| `crates/paigasus-helikon-providers-litellm/src/stream.rs` | Same rule for `ChatTranslator`, plus narrowing `warn_unresolved_pending` | 2 |
| `crates/paigasus-helikon-providers-litellm/tests/fixtures/tool_call_stream.txt` | Captured normal-shape tool-call stream | 3 |
| `crates/paigasus-helikon-providers-litellm/tests/fixtures/tool_call_stream_fragmented_name.txt` | Captured fragmented-name stream | 3 |
| `crates/paigasus-helikon-providers-litellm/tests/streaming.rs` | End-to-end assertions over both fixtures | 3 |
| `docs/book/src/concepts/agent-loop.md` | User-facing note on name buffering | 4 |
| `crates/paigasus-helikon-providers-openai/README.md` | New `## Streaming` section | 4 |
| `crates/paigasus-helikon-providers-litellm/README.md` | New bullet under `## Limitations` | 4 |

Tasks 1 and 2 are independent (different crates, no shared code — the duplication is deliberate per SMA-451 D6). Task 3 depends on Task 2. Task 4 depends on 1 and 2.

---

## Task 1: `providers-openai` chat translator

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs` — struct fields (`:224-245`), `finish()` (`:328-337`), `handle_tool_call_chunk()` (`:339-409`), in-module tests (`:412+`)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: nothing other tasks import. Task 4 documents the behaviour this task creates.

### Background you need

`handle_tool_call_chunk` today resolves a `call_id` from `tc.index`, drains the whole `pending` entry with `.remove()`, concatenates buffered + current name, and emits unconditionally. `name_emitted: HashSet<u32>` suppresses the name on later deltas — which is the bug: it suppresses *fragments*, not just repeats.

Three state changes:
1. `name_emitted: HashSet<u32>` → `HashMap<u32, String>` (index → the name that was emitted). The stored name is needed to tell a late *repeat* from a late *fragment*.
2. New `warned_late_name: HashSet<u32>` so the late-fragment warning fires at most once per call.
3. `pending` entries are no longer removed wholesale on the first post-id delta.

- [ ] **Step 1: Write the failing regression test**

Add to the `mod tests` block in `chat.rs`, after `orphan_name_concatenates_with_id_bearing_name`:

```rust
/// SMA-547: a name fragmented across deltas that arrive AFTER the id
/// resolves must be assembled, not truncated at the first fragment.
///
/// Confirmed to FAIL against the pre-fix code: the `name_emitted` guard
/// suppressed "weather", yielding a tool named `get_`.
#[test]
fn name_fragments_after_id_are_assembled() {
    let mut t = ChatTranslator::new();
    let mut out = Vec::new();

    // Delta 1: id + first name fragment, empty args → held.
    t.handle_tool_call_chunk(&make_chunk(0, Some("call_abc"), Some("get_"), Some("")), &mut out);
    assert!(out.is_empty(), "name is incomplete; nothing to emit yet");

    // Delta 2: second name fragment, still empty args → still held.
    t.handle_tool_call_chunk(&make_chunk(0, None, Some("weather"), Some("")), &mut out);
    assert!(out.is_empty(), "name may still be growing");

    // Delta 3: args arrive, no name fragment → the name is complete.
    t.handle_tool_call_chunk(&make_chunk(0, None, None, Some("{\"city\":\"Berlin\"}")), &mut out);
    assert_eq!(out.len(), 1, "expected exactly one ToolCallDelta");
    match &out[0] {
        ModelEvent::ToolCallDelta { call_id, name, args_delta } => {
            assert_eq!(call_id, "call_abc");
            assert_eq!(name.as_deref(), Some("get_weather"), "fragments must concatenate");
            assert_eq!(args_delta, "{\"city\":\"Berlin\"}");
        }
        other => panic!("expected ToolCallDelta, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run it and confirm it fails for the right reason**

Run: `cargo test -p paigasus-helikon-providers-openai --lib name_fragments_after_id_are_assembled`

Expected: FAIL. The pre-fix code emits on delta 1, so `out` is non-empty at the first assert. **Record the exact failure output** — the PR body must show this test failing pre-fix.

- [ ] **Step 3: Change the translator state**

In `chat.rs`, replace the `name_emitted` field and add `warned_late_name`:

```rust
pub(crate) struct ChatTranslator {
    /// index → call_id after the first delta for that tool call.
    tool_calls: HashMap<u32, String>,
    /// index → the tool name already emitted to the consumer.
    ///
    /// Holds the *value*, not just the index, so a late fragment can be
    /// told apart from a backend that repeats the whole name on every
    /// delta (SMA-547 §3).
    name_emitted: HashMap<u32, String>,
    /// Indices for which the late-name-fragment warning has already fired,
    /// so a chatty backend cannot produce one warn per argument chunk.
    warned_late_name: HashSet<u32>,
    /// index → buffered name/args.
    ///
    /// `args` is drain-once: taken on the first delta after the call_id is
    /// known and never re-prepended. `name` accumulates across every delta
    /// for the call and is cleared only when the name flushes (SMA-547 §1).
    pending: HashMap<u32, PendingToolCall>,
    /// Finish reason observed so far, emitted only by [`Self::finish`] at
    /// end-of-stream. Last observed value wins.
    finish_reason: Option<FinishReason>,
}
```

and in `new()`:

```rust
            name_emitted: HashMap::new(),
            warned_late_name: HashSet::new(),
```

- [ ] **Step 4: Rewrite `handle_tool_call_chunk`**

Replace the whole function body (`:339-409`) with:

```rust
    fn handle_tool_call_chunk(
        &mut self,
        tc: &ChatCompletionMessageToolCallChunk,
        out: &mut Vec<ModelEvent>,
    ) {
        let index = tc.index;
        // Both signals test the post-`unwrap_or("")` effective fragment, so a
        // backend sending `"name": ""` behaves like one omitting the field.
        let name_frag = tc
            .function
            .as_ref()
            .and_then(|f| f.name.as_deref())
            .unwrap_or("");
        let args_frag = tc
            .function
            .as_ref()
            .and_then(|f| f.arguments.as_deref())
            .unwrap_or("");

        // Resolve or register the call_id.
        let call_id = if let Some(id) = self.tool_calls.get(&index) {
            id.clone()
        } else if let Some(id) = tc.id.as_deref() {
            self.tool_calls.insert(index, id.to_owned());
            id.to_owned()
        } else {
            // No call_id yet — buffer both fields so neither is dropped.
            let entry = self.pending.entry(index).or_default();
            entry.name.push_str(name_frag);
            entry.args.push_str(args_frag);
            return;
        };

        // A name fragment arriving after the name was emitted cannot be
        // recovered — the event is already downstream. Warn once per call,
        // and not at all when the fragment merely repeats what was emitted.
        if let Some(emitted) = self.name_emitted.get(&index) {
            if !name_frag.is_empty()
                && !emitted.starts_with(name_frag)
                && self.warned_late_name.insert(index)
            {
                tracing::warn!(
                    target: "paigasus::openai::chat",
                    %call_id,
                    fragment = %name_frag,
                    emitted = %emitted,
                    "tool-call name fragment arrived after the name was emitted; \
                     it cannot be recovered and is dropped"
                );
            }
        }

        let entry = self.pending.entry(index).or_default();
        entry.name.push_str(name_frag);
        // `args` is drain-once: buffered pre-id args are prepended exactly
        // once and never re-emitted on later deltas.
        let mut args_out = std::mem::take(&mut entry.args);
        args_out.push_str(args_frag);

        // The name is complete when either signal appears: this delta carried
        // arguments, or this delta carried no name fragment. Note this tests
        // `args_frag` (this delta's own contribution), not `args_out`.
        let flush = !self.name_emitted.contains_key(&index)
            && !entry.name.is_empty()
            && (!args_frag.is_empty() || name_frag.is_empty());

        let name_to_emit = if flush {
            let name = std::mem::take(&mut entry.name);
            self.name_emitted.insert(index, name.clone());
            Some(name)
        } else {
            None
        };

        if entry.name.is_empty() && entry.args.is_empty() {
            self.pending.remove(&index);
        }

        // Suppress a wholly empty event. This tests `args_out`, NOT
        // `args_frag` — testing the latter would discard buffered pre-id
        // arguments on a bare id-carrying delta.
        if name_to_emit.is_none() && args_out.is_empty() {
            return;
        }

        out.push(ModelEvent::ToolCallDelta {
            call_id,
            name: name_to_emit,
            args_delta: args_out,
        });
    }
```

If the borrow checker objects to holding `entry` across the `self.name_emitted` accesses, split the flush decision out first (compute `flush` and `entry.name.is_empty()` into locals, drop `entry`, then re-borrow). Do not restructure further than that.

- [ ] **Step 5: Add the end-of-stream flush**

Add this method to `impl ChatTranslator`, directly above `finish()`:

```rust
    /// Emit any tool name still buffered at end-of-stream.
    ///
    /// Reached by the zero-argument shape, where no `arguments` fragment ever
    /// arrives to signal the name is complete. This is a correctness
    /// requirement, not a diagnostic: the agent loop dispatches on the
    /// presence of an `Item::ToolCall` and reads the tool to run from its
    /// `name`, so a name never emitted becomes an empty name that resolves to
    /// no tool.
    ///
    /// Only calls whose id resolved are flushed — an entry with no `call_id`
    /// has nothing to emit under. Emitted names are recorded, so a second
    /// call yields nothing.
    fn flush_buffered_names(&mut self) -> Vec<ModelEvent> {
        let mut indices: Vec<u32> = self.pending.keys().copied().collect();
        indices.sort_unstable();

        let mut out = Vec::new();
        for index in indices {
            if self.name_emitted.contains_key(&index) {
                continue;
            }
            let Some(call_id) = self.tool_calls.get(&index).cloned() else {
                continue;
            };
            let Some(entry) = self.pending.get_mut(&index) else {
                continue;
            };
            if entry.name.is_empty() {
                continue;
            }
            let name = std::mem::take(&mut entry.name);
            self.name_emitted.insert(index, name.clone());
            out.push(ModelEvent::ToolCallDelta {
                call_id,
                name: Some(name),
                args_delta: String::new(),
            });
        }
        out
    }
```

Then replace `finish()` (`:328-337`) with:

```rust
    /// Emit any buffered tool name, then the terminal `Finish`.
    ///
    /// Returns only the flushed names when no `finish_reason` was ever
    /// observed, so a truncated stream is never reported as a clean stop, and
    /// nothing at all when an earlier call already drained both buffers.
    /// `Finish` stays last, preserving core's terminal-event contract.
    pub(crate) fn finish(&mut self) -> Vec<ModelEvent> {
        let mut out = self.flush_buffered_names();
        let Some(reason) = self.finish_reason.take() else {
            tracing::debug!(
                target: "paigasus::openai::chat",
                "stream ended without a finish_reason; emitting no Finish"
            );
            return out;
        };
        out.push(ModelEvent::Finish { reason });
        out
    }
```

- [ ] **Step 6: Run the regression test — expect PASS**

Run: `cargo test -p paigasus-helikon-providers-openai --lib name_fragments_after_id_are_assembled`
Expected: PASS.

- [ ] **Step 7: Run the whole crate and fix the one known breakage**

Run: `cargo test -p paigasus-helikon-providers-openai`

Expected: `orphan_name_concatenates_with_id_bearing_name` FAILS. That is anticipated — the spec says so. It feeds `"sea"` (no id) then `id + "rch"` with no args, so the name is now held rather than emitted. The property it pins is still correct; rewrite it to consume both deltas and then call `finish()`:

```rust
    /// Chunk 1: first name fragment ("sea"), no id.
    /// Chunk 2: id arrives AND carries a name continuation ("rch").
    /// Both fragments must be concatenated in order → "search".
    ///
    /// Since SMA-547 the name is held while fragments keep arriving, so the
    /// emission moves to `finish()` — neither delta carries arguments, and
    /// both carry a name fragment, so no mid-stream signal ever says the name
    /// is complete.
    #[test]
    fn orphan_name_concatenates_with_id_bearing_name() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        t.handle_tool_call_chunk(&make_chunk(0, None, Some("sea"), None), &mut out);
        assert!(out.is_empty(), "no emission until id arrives");

        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), Some("rch"), None), &mut out);
        assert!(out.is_empty(), "name may still be growing");

        let fin = t.finish();
        assert_eq!(fin.len(), 1, "flushed name only; no finish_reason was seen");
        match &fin[0] {
            ModelEvent::ToolCallDelta { call_id, name, args_delta } => {
                assert_eq!(call_id, "c1");
                assert_eq!(
                    name.as_deref(),
                    Some("search"),
                    "buffered + id-chunk name must be concatenated"
                );
                assert_eq!(args_delta, "");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
```

Re-run `cargo test -p paigasus-helikon-providers-openai`. If any *other* test fails, stop and report it — the spec claims only this one breaks, and a second failure means the analysis was incomplete.

- [ ] **Step 8: Add the remaining unit tests**

```rust
    /// A single delta carrying id, name and non-empty args emits immediately —
    /// the args signal means the name cannot still be growing.
    #[test]
    fn complete_single_delta_emits_name_with_no_added_latency() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();
        t.handle_tool_call_chunk(
            &make_chunk(0, Some("c1"), Some("get_time"), Some("{}")),
            &mut out,
        );
        assert_eq!(out.len(), 1);
        match &out[0] {
            ModelEvent::ToolCallDelta { name, args_delta, .. } => {
                assert_eq!(name.as_deref(), Some("get_time"));
                assert_eq!(args_delta, "{}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A zero-argument call whose sole delta carries no arguments at all has
    /// no mid-stream completion signal; the name flushes from finish(),
    /// ordered BEFORE Finish.
    #[test]
    fn zero_argument_call_flushes_name_before_finish() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();
        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), Some("ping"), None), &mut out);
        assert!(out.is_empty(), "no completion signal yet");

        t.consume(stream_chunk(
            r#"{"id":"x","object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ));

        let fin = t.finish();
        assert_eq!(fin.len(), 2, "flushed name then Finish");
        match &fin[0] {
            ModelEvent::ToolCallDelta { call_id, name, args_delta } => {
                assert_eq!(call_id, "c1");
                assert_eq!(name.as_deref(), Some("ping"));
                assert_eq!(args_delta, "");
            }
            other => panic!("expected the flushed name first, got {other:?}"),
        }
        assert!(
            matches!(fin[1], ModelEvent::Finish { .. }),
            "Finish must stay terminal, got {:?}",
            fin[1]
        );
    }

    /// A truncated stream still flushes the name — the tool call is real, and
    /// the caller learns of the truncation from the ABSENT Finish, not from a
    /// missing name.
    #[test]
    fn truncated_stream_flushes_name_but_emits_no_finish() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();
        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), Some("ping"), None), &mut out);

        let fin = t.finish();
        assert_eq!(fin.len(), 1, "the flushed name, and no Finish");
        assert!(matches!(
            &fin[0],
            ModelEvent::ToolCallDelta { name: Some(n), .. } if n == "ping"
        ));
    }

    /// finish() takes its buffers, so a second call is a no-op.
    #[test]
    fn finish_name_flush_is_idempotent() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();
        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), Some("ping"), None), &mut out);
        assert_eq!(t.finish().len(), 1);
        assert!(t.finish().is_empty(), "a second finish() must yield nothing");
    }

    /// Buffered pre-id args followed by a bare id-carrying delta must still
    /// emit those args. Testing the emit-nothing guard against this delta's
    /// own fragment (rather than the combined value) would swallow them.
    #[test]
    fn buffered_args_survive_a_bare_id_delta() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        t.handle_tool_call_chunk(&make_chunk(0, None, None, Some("{\"a\":1}")), &mut out);
        assert!(out.is_empty(), "buffered until the id arrives");

        // Bare id: no name, no args.
        t.handle_tool_call_chunk(&make_chunk(0, Some("c1"), None, None), &mut out);
        assert_eq!(out.len(), 1, "the buffered args must not be swallowed");
        match &out[0] {
            ModelEvent::ToolCallDelta { call_id, name, args_delta } => {
                assert_eq!(call_id, "c1");
                assert_eq!(name.as_deref(), None, "no name fragment was ever sent");
                assert_eq!(args_delta, "{\"a\":1}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A late fragment cannot be recovered, but a backend that merely repeats
    /// the whole name on every delta is not an error and must not warn.
    #[test]
    fn repeated_whole_name_is_not_treated_as_a_late_fragment() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        t.handle_tool_call_chunk(
            &make_chunk(0, Some("c1"), Some("search"), Some("{\"q\":")),
            &mut out,
        );
        t.handle_tool_call_chunk(&make_chunk(0, None, Some("search"), Some("1}")), &mut out);

        assert_eq!(out.len(), 2);
        match &out[1] {
            ModelEvent::ToolCallDelta { name, args_delta, .. } => {
                assert_eq!(name.as_deref(), None, "name is emitted once per call");
                assert_eq!(args_delta, "1}");
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(
            t.warned_late_name.is_empty(),
            "a repeat of the emitted name must not warn"
        );
    }

    /// A genuine late fragment is dropped (it cannot be un-emitted) but is
    /// recorded so the loss is visible, and only once per call.
    #[test]
    fn late_name_fragment_warns_once() {
        let mut t = ChatTranslator::new();
        let mut out = Vec::new();

        t.handle_tool_call_chunk(
            &make_chunk(0, Some("c1"), Some("get_"), Some("{\"a\":")),
            &mut out,
        );
        t.handle_tool_call_chunk(&make_chunk(0, None, Some("weather"), Some("1")), &mut out);
        t.handle_tool_call_chunk(&make_chunk(0, None, Some("weather"), Some("}")), &mut out);

        assert!(t.warned_late_name.contains(&0), "the loss must be recorded");
        assert_eq!(t.warned_late_name.len(), 1, "at most one warning per call");
        assert_eq!(
            t.name_emitted.get(&0).map(String::as_str),
            Some("get_"),
            "the already-emitted name is not retroactively changed"
        );
    }
```

- [ ] **Step 9: Run the full crate suite plus fmt and clippy**

```bash
cargo test -p paigasus-helikon-providers-openai
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
```

All must be clean. `clippy` will flag the `HashSet` import if `warned_late_name` was the only remaining user of something — check the `use` line at the top of the file still matches what is used.

- [ ] **Step 10: Commit**

```bash
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs
git commit -m "fix(providers-openai): SMA-547 defer tool-call name until it is complete

Name fragments arriving after the call id resolved were suppressed by
the name_emitted guard, so a backend splitting a function name across
post-id deltas produced a tool named get_ rather than get_weather.

The name now accumulates across deltas and is emitted when either
completion signal appears -- this delta carried arguments, or it carried
no name fragment -- with any name still buffered at end-of-stream
flushed from finish() before Finish. A fragment arriving after the flush
cannot be recovered and is warned about once per call.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `providers-litellm` stream translator

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` — `Key` derive (`:29`), struct fields (`:55-78`), `handle_tool_call()` (`:146-229`), `finish()` (`:237-250`), `warn_unresolved_pending()` (`:259-269`), in-module tests (`:271+`)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: the behaviour Task 3's fixture tests assert against. No new public items.

### Background you need

Structurally the same fix as Task 1, with three litellm-specific differences:

1. **Correlation is by `Key`, not `u32`.** `Key::Index(i)` when the delta carries `index`, else `Key::Id(id)`. One `call_id` can therefore be reached under two different keys, so the end-of-stream flush must additionally skip any call whose `call_id` already emitted a name. (This dual-keying is pre-existing and out of scope to fix; the flush must merely not make it worse.)
2. **`warn_unresolved_pending()` iterates all of `pending`** and says the ids "were never resolved". Once `pending` legitimately holds resolved entries, that warning becomes false on healthy streams. It must be narrowed and moved after the flush.
3. **litellm already suppresses empty events** (`:220-222`) and already tests the *combined* args, so it needs no change there.

- [ ] **Step 1: Write the failing regression test**

Add to `mod tests` in `stream.rs`:

```rust
    /// SMA-547: a name fragmented across deltas that arrive AFTER the id
    /// resolves must be assembled, not truncated at the first fragment.
    ///
    /// Confirmed to FAIL against the pre-fix code: the `name_emitted` guard
    /// suppressed "weather", yielding a tool named `get_`.
    #[test]
    fn name_fragments_after_id_are_assembled() {
        let mut t = ChatTranslator::new();
        let mut evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call_abc", "function": {"name": "get_", "arguments": ""}}
            ]}}]
        })));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "weather", "arguments": ""}}
            ]}}]
        }))));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "{\"city\":\"Berlin\"}"}}
            ]}}]
        }))));

        let calls: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta { call_id, name, args_delta } => {
                    Some((call_id.clone(), name.clone(), args_delta.clone()))
                }
                _ => None,
            })
            .collect();

        assert_eq!(calls.len(), 1, "one delta, once the name is known complete");
        assert_eq!(calls[0].0, "call_abc");
        assert_eq!(
            calls[0].1,
            Some("get_weather".to_owned()),
            "fragments must concatenate"
        );
        assert_eq!(calls[0].2, "{\"city\":\"Berlin\"}");
    }
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p paigasus-helikon-providers-litellm --lib name_fragments_after_id_are_assembled`

Expected: FAIL — pre-fix emits `Some("get_")` on the first delta. **Record the exact failure output.**

- [ ] **Step 3: Make `Key` orderable and change the translator state**

`Key` needs a total order so the end-of-stream flush is deterministic. Extend its derive at `:29`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Key {
```

Then the struct:

```rust
pub(crate) struct ChatTranslator {
    /// Resolved call ids, keyed by correlation [`Key`].
    tool_calls: HashMap<Key, String>,
    /// Key → the tool name already emitted to the consumer.
    ///
    /// Holds the *value*, not just the key, so a late fragment can be told
    /// apart from a backend that repeats the whole name on every delta
    /// (SMA-547 §3).
    name_emitted: HashMap<Key, String>,
    /// Keys for which the late-name-fragment warning has already fired, so a
    /// chatty backend cannot produce one warn per argument chunk.
    warned_late_name: HashSet<Key>,
    /// Name/args fragments buffered per call.
    ///
    /// `args` is drain-once: taken on the first delta after the call's `id`
    /// is known and never re-prepended. `name` accumulates across every delta
    /// and is cleared only when the name flushes (SMA-547 §1).
    pending: HashMap<Key, Pending>,
    /// The most recent `finish_reason` observed, buffered until [`Self::finish`].
    finish_reason: Option<String>,
    /// Whether the multi-choice warning has already fired for this stream.
    warned_multi_choice: bool,
}
```

and in `new()`:

```rust
            name_emitted: HashMap::new(),
            warned_late_name: HashSet::new(),
```

- [ ] **Step 4: Rewrite the tail of `handle_tool_call`**

Keep the `Key` resolution block (`:153-180`) and the `name_frag`/`args_frag` extraction unchanged, but make the fragments non-optional to match Task 1's effective-fragment rule. Replace everything from `let name_frag = …` (`:182`) to the end of the function with:

```rust
        // Both signals test the effective fragment, so a backend sending
        // `"name": ""` behaves like one omitting the field.
        let name_frag = tc
            .function
            .as_ref()
            .and_then(|f| f.name.as_deref())
            .unwrap_or("");
        let args_frag = tc
            .function
            .as_ref()
            .and_then(|f| f.arguments.as_deref())
            .unwrap_or("");

        if let Some(id) = tc.id.as_deref() {
            self.tool_calls
                .entry(key.clone())
                .or_insert_with(|| id.to_owned());
        }

        let Some(call_id) = self.tool_calls.get(&key).cloned() else {
            // No id yet — buffer both fragments.
            let slot = self.pending.entry(key).or_default();
            slot.name.push_str(name_frag);
            slot.args.push_str(args_frag);
            return;
        };

        // A name fragment arriving after the name was emitted cannot be
        // recovered — the event is already downstream. Warn once per call,
        // and not at all when the fragment merely repeats what was emitted.
        if let Some(emitted) = self.name_emitted.get(&key) {
            if !name_frag.is_empty()
                && !emitted.starts_with(name_frag)
                && self.warned_late_name.insert(key.clone())
            {
                tracing::warn!(
                    target: "paigasus::litellm::stream",
                    %call_id,
                    fragment = %name_frag,
                    emitted = %emitted,
                    "tool-call name fragment arrived after the name was emitted; \
                     it cannot be recovered and is dropped"
                );
            }
        }

        let slot = self.pending.entry(key.clone()).or_default();
        slot.name.push_str(name_frag);
        // `args` is drain-once.
        let mut args = std::mem::take(&mut slot.args);
        args.push_str(args_frag);

        // The name is complete when either signal appears: this delta carried
        // arguments, or this delta carried no name fragment.
        let flush = !self.name_emitted.contains_key(&key)
            && !slot.name.is_empty()
            && (!args_frag.is_empty() || name_frag.is_empty());

        let emit_name = if flush {
            let name = std::mem::take(&mut slot.name);
            self.name_emitted.insert(key.clone(), name.clone());
            Some(name)
        } else {
            None
        };

        if slot.name.is_empty() && slot.args.is_empty() {
            self.pending.remove(&key);
        }

        if emit_name.is_none() && args.is_empty() {
            return;
        }

        out.push(ModelEvent::ToolCallDelta {
            call_id,
            name: emit_name,
            args_delta: args,
        });
```

- [ ] **Step 5: Add the flush, and narrow the unresolved-pending warning**

Add above `finish()`:

```rust
    /// Emit any tool name still buffered at end-of-stream.
    ///
    /// Reached by the zero-argument shape, where no `arguments` fragment ever
    /// arrives to signal the name is complete. Correctness, not diagnostics:
    /// the agent loop dispatches on the presence of an `Item::ToolCall` and
    /// reads the tool to run from its `name`.
    ///
    /// Skips entries whose `id` never resolved (nothing to emit under) and
    /// entries whose resolved `call_id` already emitted a name. The latter is
    /// not redundant with `name_emitted`: a call reached under both
    /// `Key::Index` and `Key::Id` has two entries for one `call_id`, and the
    /// contract allows only one name-carrying delta per call.
    fn flush_buffered_names(&mut self) -> Vec<ModelEvent> {
        let mut keys: Vec<Key> = self.pending.keys().cloned().collect();
        keys.sort();

        let mut already: std::collections::HashSet<String> = self
            .name_emitted
            .keys()
            .filter_map(|k| self.tool_calls.get(k).cloned())
            .collect();

        let mut out = Vec::new();
        for key in keys {
            if self.name_emitted.contains_key(&key) {
                continue;
            }
            let Some(call_id) = self.tool_calls.get(&key).cloned() else {
                continue;
            };
            if !already.insert(call_id.clone()) {
                continue;
            }
            let Some(slot) = self.pending.get_mut(&key) else {
                continue;
            };
            if slot.name.is_empty() {
                continue;
            }
            let name = std::mem::take(&mut slot.name);
            self.name_emitted.insert(key, name.clone());
            out.push(ModelEvent::ToolCallDelta {
                call_id,
                name: Some(name),
                args_delta: String::new(),
            });
        }
        out
    }
```

Replace `finish()` with:

```rust
    /// Emit any buffered tool name, then the terminal `Finish`, if a
    /// `finish_reason` was ever observed.
    ///
    /// Emits no `Finish` on a truncated stream: fabricating `Finish::Stop`
    /// would make a dropped connection indistinguishable from a clean
    /// completion, and `ModelTurnAccumulator` defaults to `Stop`, so the
    /// truncated text would be committed to session history as final. A
    /// buffered *name* is still flushed — the tool call is real, and the
    /// truncation is signalled by the absent `Finish`.
    pub(crate) fn finish(&mut self) -> Vec<ModelEvent> {
        let mut out = self.flush_buffered_names();
        // After the flush, so entries it drained are not re-reported as lost.
        self.warn_unresolved_pending();
        let Some(raw) = self.finish_reason.take() else {
            return out;
        };
        let reason = match raw.as_str() {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::Length,
            "tool_calls" | "function_call" => FinishReason::ToolCalls,
            "content_filter" => FinishReason::ContentFilter,
            other => FinishReason::Other(other.to_owned()),
        };
        out.push(ModelEvent::Finish { reason });
        out
    }
```

Replace `warn_unresolved_pending()` with the narrowed version:

```rust
    /// Warn when tool-call fragments were buffered but never flushed.
    ///
    /// A call whose `id` never arrived stays in `pending` forever — a backend
    /// that never sends an `id` at all silently drops the call, so this makes
    /// the loss loud instead of indistinguishable from "the model didn't call
    /// a tool."
    ///
    /// Only entries with **no resolved `call_id`** qualify. Since SMA-547
    /// `pending` also holds entries for calls whose id *is* known (their name
    /// is still accumulating), and reporting those would fire a false warning
    /// on every healthy stream that buffers a name.
    fn warn_unresolved_pending(&self) {
        let keys: Vec<String> = self
            .pending
            .keys()
            .filter(|k| !self.tool_calls.contains_key(k))
            .map(|k| format!("{k:?}"))
            .collect();
        if keys.is_empty() {
            return;
        }
        tracing::warn!(
            target: "paigasus::litellm::stream",
            keys = ?keys,
            "stream ended with buffered tool-call fragments whose id was never resolved; dropping them"
        );
    }
```

- [ ] **Step 6: Run the regression test — expect PASS**

Run: `cargo test -p paigasus-helikon-providers-litellm --lib name_fragments_after_id_are_assembled`
Expected: PASS.

- [ ] **Step 7: Run the crate suite**

Run: `cargo test -p paigasus-helikon-providers-litellm`

`tool_call_id_arriving_late_does_not_lose_name_or_args` must still pass: its third delta is a bare id, which carries no name fragment, so the flush signal fires and it emits `Some("search")` with the buffered args — exactly what it asserts. If anything fails, stop and report rather than adjusting assertions to match.

- [ ] **Step 8: Add the mirrored unit tests**

Mirror Task 1's suite so the two crates cannot drift. Same six behaviours, written against `consume()` and `chunk(json!(…))`:

```rust
    /// A single delta carrying id, name and non-empty args emits immediately.
    #[test]
    fn complete_single_delta_emits_name_with_no_added_latency() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "get_time", "arguments": "{}"}}
            ]}}]
        })));
        assert!(matches!(
            &evs[0],
            ModelEvent::ToolCallDelta { name: Some(n), args_delta, .. }
                if n == "get_time" && args_delta == "{}"
        ));
    }

    /// A zero-argument call flushes its name from finish(), before Finish.
    #[test]
    fn zero_argument_call_flushes_name_before_finish() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "ping"}}
            ]}, "finish_reason": "tool_calls"}]
        })));
        assert!(evs.is_empty(), "no completion signal during the stream");

        let fin = t.finish();
        assert_eq!(fin.len(), 2, "flushed name then Finish");
        assert!(matches!(
            &fin[0],
            ModelEvent::ToolCallDelta { call_id, name: Some(n), args_delta }
                if call_id == "c1" && n == "ping" && args_delta.is_empty()
        ));
        assert!(
            matches!(fin[1], ModelEvent::Finish { .. }),
            "Finish must stay terminal"
        );
    }

    /// A truncated stream flushes the name but reports no clean stop.
    #[test]
    fn truncated_stream_flushes_name_but_emits_no_finish() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "ping"}}
            ]}}]
        })));
        let fin = t.finish();
        assert_eq!(fin.len(), 1);
        assert!(matches!(
            &fin[0],
            ModelEvent::ToolCallDelta { name: Some(n), .. } if n == "ping"
        ));
    }

    /// finish() takes its buffers, so a second call is a no-op.
    #[test]
    fn finish_is_idempotent_after_draining() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "ping"}}
            ]}, "finish_reason": "stop"}]
        })));
        assert_eq!(t.finish().len(), 2);
        assert!(t.finish().is_empty(), "a second finish() must yield nothing");
    }

    /// A repeat of the whole name is not a lost fragment and must not warn.
    #[test]
    fn repeated_whole_name_is_not_treated_as_a_late_fragment() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "search", "arguments": "{\"q\":"}}
            ]}}]
        })));
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "search", "arguments": "1}"}}
            ]}}]
        })));
        assert!(
            t.warned_late_name.is_empty(),
            "a repeat of the emitted name must not warn"
        );
    }

    /// A genuine late fragment is recorded once and does not rewrite the
    /// already-emitted name.
    #[test]
    fn late_name_fragment_warns_once() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "get_", "arguments": "{\"a\":"}}
            ]}}]
        })));
        for frag in ["weather", "weather"] {
            t.consume(chunk(serde_json::json!({
                "choices": [{"index": 0, "delta": {"tool_calls": [
                    {"index": 0, "function": {"name": frag, "arguments": "1"}}
                ]}}]
            })));
        }
        assert_eq!(t.warned_late_name.len(), 1, "at most one warning per call");
        assert_eq!(
            t.name_emitted.get(&Key::Index(0)).map(String::as_str),
            Some("get_"),
            "the already-emitted name is not retroactively changed"
        );
    }

    /// An entry whose id never resolved is not flushed — there is no call_id
    /// to emit under — and stays the domain of the unresolved-pending warning.
    #[test]
    fn unresolved_entries_are_not_flushed() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "orphan"}}
            ]}, "finish_reason": "stop"}]
        })));
        let fin = t.finish();
        assert_eq!(fin.len(), 1, "only Finish; the orphan has no call_id");
        assert!(matches!(fin[0], ModelEvent::Finish { .. }));
    }
```

- [ ] **Step 9: Run the full suite plus fmt and clippy**

```bash
cargo test -p paigasus-helikon-providers-litellm
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-litellm --all-features --all-targets -- -D warnings
```

- [ ] **Step 10: Commit**

```bash
git add crates/paigasus-helikon-providers-litellm/src/stream.rs
git commit -m "fix(providers): SMA-547 defer litellm tool-call name until it is complete

Mirrors the providers-openai fix so the two OpenAI-compatible chat
translators keep identical streaming semantics: the tool name
accumulates across deltas and is emitted once a completion signal
appears, with any name still buffered at end-of-stream flushed from
finish() before Finish.

Narrows warn_unresolved_pending to entries with no resolved call_id and
moves it after the flush -- pending now legitimately holds resolved
entries whose name is still accumulating, which would otherwise fire a
false warning on every healthy stream.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Captured LiteLLM fixtures and end-to-end tests

**Files:**
- Create: `crates/paigasus-helikon-providers-litellm/tests/fixtures/tool_call_stream.txt`
- Create: `crates/paigasus-helikon-providers-litellm/tests/fixtures/tool_call_stream_fragmented_name.txt`
- Modify: `crates/paigasus-helikon-providers-litellm/tests/streaming.rs`

**Interfaces:**
- Consumes: Task 2's translator behaviour. Uses the existing `events_for(fixture: &str) -> Vec<ModelEvent>` helper in `streaming.rs` — do not write a new harness.
- Produces: nothing.

### Background you need

The crate has no tool-call streaming fixture at all, so nothing pins the normal path Task 2 also changed. Both fixtures below are **real captures** from LiteLLM 1.98.0 fronting a local fake OpenAI-compatible upstream (see the spec's §Evidence for the method). Do not hand-edit the payloads — they are verbatim proxy output, including LiteLLM's key reordering and the `"type":"function"` it adds to continuation deltas.

`.gitattributes:4` already pins `providers-litellm/tests/fixtures/*.txt` to `text eol=lf`, so these are covered on creation. No `.gitattributes` change is needed.

- [ ] **Step 1: Create the normal-shape fixture**

Create `crates/paigasus-helikon-providers-litellm/tests/fixtures/tool_call_stream.txt` with exactly this content (the `: ` lines are SSE comments, ignored by `eventsource-stream`):

```
: Provenance: CAPTURED from LiteLLM 1.98.0 (ghcr.io/berriai/litellm:main-latest)
: proxying a local fake OpenAI-compatible upstream over api_base. A keyless
: mock_response proxy cannot emit streamed tool calls -- mock_tool_calls is
: ignored on the streaming path -- so the upstream, not the mock, supplies the
: shape. See the SMA-547 design doc, section "Evidence".
:
: Normal shape: the whole function name arrives in the delta carrying the id.

data: {"id":"chatcmpl-fake","created":1786999072,"model":"shape-normal","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"id":"call_abc","function":{"arguments":"","name":"get_weather"},"type":"function","index":0}]}}]}

data: {"id":"chatcmpl-fake","created":1786999072,"model":"shape-normal","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"function":{"arguments":"{\"city\":"},"type":"function","index":0}]}}]}

data: {"id":"chatcmpl-fake","created":1786999072,"model":"shape-normal","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"function":{"arguments":"\"Berlin\"}"},"type":"function","index":0}]}}]}

data: {"id":"chatcmpl-fake","object":"chat.completion.chunk","created":1786999072,"model":"shape-normal","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: {"id":"chatcmpl-fake","created":1786999072,"model":"shape-normal","object":"chat.completion.chunk","choices":[{"index":0,"delta":{}}],"usage":{"completion_tokens":27,"prompt_tokens":11,"total_tokens":38}}

data: [DONE]

```

- [ ] **Step 2: Create the fragmented-name fixture**

Create `crates/paigasus-helikon-providers-litellm/tests/fixtures/tool_call_stream_fragmented_name.txt`:

```
: Provenance: CAPTURED from LiteLLM 1.98.0 (ghcr.io/berriai/litellm:main-latest)
: proxying a local fake OpenAI-compatible upstream over api_base. See the
: SMA-547 design doc, section "Evidence", for the capture method.
:
: The SMA-547 shape: the function name is split across two deltas that BOTH
: arrive after the id. This capture is what proves LiteLLM passes name
: fragments through verbatim rather than reassembling them -- the defect is
: reachable through a real proxy, not hypothetical.

data: {"id":"chatcmpl-fake","created":1786999074,"model":"shape-fragment","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"id":"call_abc","function":{"arguments":"","name":"get_"},"type":"function","index":0}]}}]}

data: {"id":"chatcmpl-fake","created":1786999074,"model":"shape-fragment","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"function":{"arguments":"","name":"weather"},"type":"function","index":0}]}}]}

data: {"id":"chatcmpl-fake","created":1786999074,"model":"shape-fragment","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"function":{"arguments":"{\"city\":\"Berlin\"}"},"type":"function","index":0}]}}]}

data: {"id":"chatcmpl-fake","object":"chat.completion.chunk","created":1786999074,"model":"shape-fragment","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: {"id":"chatcmpl-fake","created":1786999074,"model":"shape-fragment","object":"chat.completion.chunk","choices":[{"index":0,"delta":{}}],"usage":{"completion_tokens":26,"prompt_tokens":11,"total_tokens":37}}

data: [DONE]

```

- [ ] **Step 3: Add the end-to-end tests**

Append to `crates/paigasus-helikon-providers-litellm/tests/streaming.rs`:

```rust
/// Helper: collect the (call_id, name, args) of every ToolCallDelta.
fn tool_calls(evs: &[ModelEvent]) -> Vec<(String, Option<String>, String)> {
    evs.iter()
        .filter_map(|e| match e {
            ModelEvent::ToolCallDelta { call_id, name, args_delta } => {
                Some((call_id.clone(), name.clone(), args_delta.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The normal captured shape: one name-carrying delta, args concatenating to
/// the whole JSON object, Usage before a terminal Finish.
#[tokio::test]
async fn captured_tool_call_stream_assembles_one_named_call() {
    let evs = events_for(include_str!("fixtures/tool_call_stream.txt")).await;
    let calls = tool_calls(&evs);

    let named: Vec<_> = calls.iter().filter(|c| c.1.is_some()).collect();
    assert_eq!(named.len(), 1, "exactly one delta carries the name, got {calls:?}");
    assert_eq!(named[0].1.as_deref(), Some("get_weather"));
    assert!(calls.iter().all(|c| c.0 == "call_abc"), "one call_id throughout");

    let args: String = calls.iter().map(|c| c.2.clone()).collect();
    assert_eq!(args, "{\"city\":\"Berlin\"}");

    // Usage must precede Finish, but need NOT be adjacent to it: the
    // end-of-stream name flush can sit between them (SMA-547 §2).
    let usage_pos = evs
        .iter()
        .position(|e| matches!(e, ModelEvent::Usage { .. }))
        .expect("Usage must be emitted");
    let finish_pos = evs
        .iter()
        .position(|e| matches!(e, ModelEvent::Finish { .. }))
        .expect("Finish must be emitted");
    assert!(usage_pos < finish_pos, "Usage must precede Finish, got {evs:?}");
    assert_eq!(finish_pos, evs.len() - 1, "Finish is terminal");
}

/// SMA-547 regression, end to end over a real captured LiteLLM stream: the
/// name is split across two post-id deltas and must assemble to `get_weather`,
/// not truncate to `get_`.
#[tokio::test]
async fn captured_fragmented_name_stream_assembles_the_whole_name() {
    let evs = events_for(include_str!("fixtures/tool_call_stream_fragmented_name.txt")).await;
    let calls = tool_calls(&evs);

    let named: Vec<_> = calls.iter().filter_map(|c| c.1.as_deref()).collect();
    assert_eq!(
        named,
        vec!["get_weather"],
        "the name must assemble from both fragments, and be emitted once"
    );

    let args: String = calls.iter().map(|c| c.2.clone()).collect();
    assert_eq!(args, "{\"city\":\"Berlin\"}");
}
```

If `ModelEvent` is not already imported in `streaming.rs`, add it to the existing `use paigasus_helikon_core::{…}` line rather than adding a second `use`.

- [ ] **Step 4: Run the new tests**

Run: `cargo test -p paigasus-helikon-providers-litellm --test streaming`
Expected: PASS, including the pre-existing tests.

- [ ] **Step 5: Confirm the fragmented test fails against pre-fix code**

Do **not** use `git stash push -u` for this: `-u` stashes untracked files, which
at this point in Task 3 includes the new fixtures and the new
`tests/streaming.rs` — swap in the pre-fix `stream.rs` with those stashed away
and the filtered test simply does not exist, so `cargo test` can exit 0 having
run **zero** tests, reading as a false "verified" result. The stash stack is
also shared with other worktrees and other Claude sessions, which is a second,
independent reason never to use bare `git stash` / `git stash pop` here.

Use a path-limited checkout instead — it does not move `HEAD` and does not
touch the new test files, only the one source file under test:

```bash
git status --short          # must be empty before you start
git checkout origin/main -- crates/paigasus-helikon-providers-litellm/src/stream.rs
cargo test -p paigasus-helikon-providers-litellm --test streaming captured_fragmented_name
git checkout HEAD -- crates/paigasus-helikon-providers-litellm/src/stream.rs
git status --short          # must be empty again
```

Expected: FAIL, with `name` observed as `Some("get_")` rather than
`Some("get_weather")`. The output must show **exactly one test ran and
failed** — `0 passed; 0 failed; ... 0 filtered out` (or any count other than
one test executed) means the filter matched nothing, the pre-fix `stream.rs`
swap didn't take effect, or the new test wasn't compiled in, and proves
nothing about pre-fix behaviour. Re-check the filter and the checkout before
trusting the result.

**Record the failure output for the PR body.**

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-providers-litellm/tests/fixtures/tool_call_stream.txt \
        crates/paigasus-helikon-providers-litellm/tests/fixtures/tool_call_stream_fragmented_name.txt \
        crates/paigasus-helikon-providers-litellm/tests/streaming.rs
git commit -m "test(providers): SMA-547 add captured litellm tool-call stream fixtures

The crate had no tool-call streaming fixture, so nothing pinned the
normal path. Both fixtures are captured from LiteLLM 1.98.0 proxying a
local fake OpenAI-compatible upstream -- the api_base route works where
a keyless mock_response proxy cannot, since mock_tool_calls is ignored
on the streaming path.

The fragmented capture is the evidence that LiteLLM passes name
fragments through verbatim, making the SMA-547 defect reachable through
a real proxy rather than hypothetical.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Documentation

**Files:**
- Modify: `docs/book/src/concepts/agent-loop.md:57`
- Modify: `crates/paigasus-helikon-providers-openai/README.md` (new section between `## Example` and `## Links`)
- Modify: `crates/paigasus-helikon-providers-litellm/README.md` (new bullet under `## Limitations`, `:118`)

**Interfaces:**
- Consumes: the behaviour Tasks 1–2 implement.
- Produces: nothing.

### Background you need

CLAUDE.md makes both the mdBook and crate READMEs hard requirements for user-facing changes, and this change alters observable event timing. **Word all three as provider behaviour, never as a core guarantee** — the guarantee is SMA-533's to state, and the docs must not get ahead of the contract.

Neither README has a streaming section today, so the exact placement is specified rather than left to judgement.

- [ ] **Step 1: Update the book**

In `docs/book/src/concepts/agent-loop.md`, find line 57:

```markdown
- Raw deltas (for low-latency UIs): `TokenDelta { text }`, `ReasoningDelta { text }`, `ToolCallDelta { call_id, name, args_delta }`.
```

Add immediately after it:

```markdown
  A provider whose wire format splits a tool name across several deltas buffers
  the fragments, so `name` is `Some` on the first delta the provider can
  establish the whole name from — usually the one carrying the first arguments
  chunk — and `None` on the rest. A tool call that never carries arguments has
  its name emitted at end-of-stream.
```

- [ ] **Step 2: Verify the book still builds**

Run: `mdbook build docs/book`
Expected: clean. `[output.linkcheck] warning-policy = "error"`, so a broken link fails the build.

If `mdbook` is not installed, say so and skip this step rather than installing it — CI's `book-build` job is the gate.

- [ ] **Step 3: Add the openai README section**

In `crates/paigasus-helikon-providers-openai/README.md`, insert between the `## Example` section and `## Links`:

```markdown
## Streaming tool calls

Chat Completions streams a tool call's function name as a per-delta fragment,
and does not guarantee the whole name arrives in the delta carrying the call id.
The translator therefore buffers name fragments and emits `ToolCallDelta.name`
once, on the first delta it can establish the name is complete from — in
practice the delta carrying the first arguments chunk. A tool call that never
carries arguments has its name emitted at end-of-stream, before `Finish`.
```

- [ ] **Step 4: Add the litellm README bullet**

In `crates/paigasus-helikon-providers-litellm/README.md`, under `## Limitations` (`:118`), append:

```markdown
- **Tool-call names are buffered until complete.** LiteLLM passes a backend's
  function-name fragments through verbatim rather than reassembling them, so the
  translator buffers them and emits `ToolCallDelta.name` once, on the first
  delta it can establish the name is complete from. One shape is not
  recoverable: a backend that emits a further name fragment *after* arguments
  have already begun. That fragment is dropped and logged at `warn`, because the
  name-carrying event has already been yielded downstream.
```

- [ ] **Step 5: Commit**

```bash
git add docs/book/src/concepts/agent-loop.md \
        crates/paigasus-helikon-providers-openai/README.md \
        crates/paigasus-helikon-providers-litellm/README.md
git commit -m "docs(providers): SMA-547 document tool-call name buffering

Notes the observable timing change in the book and both provider
READMEs. Worded as provider behaviour rather than a core guarantee --
the contract wording is SMA-533's to state, alongside the conformance
suite that enforces it.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Full-workspace verification

**Files:** none modified — this task only runs gates.

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces: the evidence the PR body needs.

- [ ] **Step 1: Reproduce every CI gate locally**

Run each, in order, and do not proceed past a failure:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

`cargo test --workspace --all-features` is the exact gate — do not substitute per-crate runs, which have previously masked cross-crate feature interactions.

- [ ] **Step 2: Confirm no core file was touched**

```bash
git diff --stat origin/main...HEAD -- crates/paigasus-helikon-core/
```

That command only compares committed history — it would miss an uncommitted
or unstaged edit under `core/`. Also check the working tree directly:

```bash
git diff --name-only origin/main -- crates/paigasus-helikon-core/
git status --short -- crates/paigasus-helikon-core/
```

Expected: **empty output from all three commands.** Any change here violates
a global constraint — the spec defers all core edits to SMA-533. If any of
them prints anything, stop and report.

- [ ] **Step 3: Confirm the two translators stayed aligned**

Read both `handle_tool_call_chunk` and `handle_tool_call` side by side. The flush condition, the emit-nothing guard, the late-fragment warn condition, and the end-of-stream flush must be behaviourally identical. Any difference must be a litellm-specific one the spec names (the `Key` dual-keying guard, the narrowed `warn_unresolved_pending`) and must carry a code comment saying so.

- [ ] **Step 4: Confirm no other test asserts on the changed behaviour**

```bash
grep -rn "ToolCallDelta" --include="*.rs" \
  crates/paigasus-helikon-providers-openai \
  crates/paigasus-helikon-providers-litellm
```

Read every hit. Any test asserting on delta counts, name timing, or empty `args_delta` that this change affects must already have been updated. The spec found two this way and missed one on the first pass, so do not skip this.

- [ ] **Step 5: Verify the branch is clean and report**

```bash
git status --short
git log --oneline origin/main..HEAD
```

Report: the commit list, the two recorded pre-fix failure outputs (Task 1 Step 2, Task 2 Step 2, Task 3 Step 5), and confirmation that all four gates in Step 1 passed.

---

## Self-Review

**Spec coverage:**

| Spec section | Task |
|---|---|
| §1 flush rule, `Pending` split lifecycle, `args_out` guard, effective-fragment rule | 1 (openai), 2 (litellm) |
| §1 observable change for providers-openai (6→4 events) | 1 Step 7 verifies via the existing suite |
| §2 end-of-stream flush, correctness framing, call_id resolution, unresolved entries skipped, `Key` dedup, `warn_unresolved_pending` narrowing, truncated streams | 1 Step 5, 2 Step 5 |
| §2 paths that never reach `finish()` | Accepted, no code; no task needed |
| §3 late-fragment warn, once per call, prefix suppression, `target:` form | 1 Step 4, 2 Step 4 |
| §4 core wording deferred to SMA-533 | Global constraint + Task 5 Step 2 enforces it |
| §5 captured fixtures, provenance comment form, `usage_pos < finish_pos` | 3 |
| §5 unit tests 1–8, mirrored across crates | 1 Step 8, 2 Step 8 |
| §5 demonstrated failing pre-fix | 1 Step 2, 2 Step 2, 3 Step 5 |
| §5 regressions incl. the broken openai test | 1 Step 7 |
| §5 grep for other affected tests | 5 Step 4 |
| §5 line endings — no change needed | Noted in Task 3 background |
| §6 book + both READMEs, worded as provider behaviour | 4 |
| §6 no manual version work, `providers` commit scope | Global constraints |

No gaps.

**Placeholder scan:** every code step carries real code; no "TBD", no "handle edge cases", no "similar to Task N" (Task 2's tests are written out in full rather than referring back to Task 1's).

**Type consistency:** `name_emitted` is `HashMap<u32, String>` in Task 1 and `HashMap<Key, String>` in Task 2, and every read site uses `contains_key` / `get`, never `contains`. `warned_late_name` is `HashSet<u32>` / `HashSet<Key>`. `flush_buffered_names(&mut self) -> Vec<ModelEvent>` has the same name and signature in both crates. `tool_calls(&[ModelEvent])` in Task 3 does not collide with the `tool_calls` *field* on either translator — it is a free function in the integration-test file, which has no access to translator internals.
