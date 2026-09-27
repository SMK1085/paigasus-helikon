# SMA-617 Responses Call-Id Name Gating Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `ResponsesTranslator` emit exactly one name-carrying `ToolCallDelta` per non-blank `call_id`, also when two function-call items share one `call_id`.

**Architecture:** Split the item-keyed `name_emitted` set into `emitted_items` (per-item dedup, as before) and `named_calls` (per-`call_id` name gate). One private helper, `claim_name`, decides the `name` field at all three emission sites. Argument bytes do not change: a second item's arguments still go to the shared `call_id` (merge, approved by Sven at GATE 1).

**Tech Stack:** Rust (edition from workspace, MSRV 1.94), `async-openai` Responses types, `tracing`.

**Spec:** `docs/superpowers/specs/2026-09-27-sma-617-responses-call-id-name-gating-design.md`

## Global Constraints

- Worktree root: `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-617`. Every path below is relative to it. Use absolute paths with this prefix in Write/Edit.
- Branch: `feature/sma-617-openairesponses-can-emit-two-name-carrying-deltas-for-one`. Never run `git checkout`, `git switch`, `git reset`, `git rebase`, or `git stash`.
- Commit format: `<type>(<scope>): SMA-617 <lowercase subject>`. Allowed scopes include `providers-openai` and `providers`.
- End every commit message with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Before each commit, run `cargo fmt --all` and `cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings`.
- Work synchronously in the foreground. Do not end your turn until every command in your task has finished and the commit exists.
- No public API change. No mdBook or README edit (spec §2).
- The log target is exactly `paigasus::openai::responses`.

## Review Focus

1. A blank `call_id` on two parallel items: each item must keep its own name (core non-merge rule). Pinned by Task 1 test 5.
2. An alias item that already streamed its deltas, then appears on `output_item.done` and in the `response.completed` sweep: nothing may be re-emitted. Pinned by Task 1 test 6.
3. An `incomplete` or `in_progress` item after `added`: `Finish { Stop }` and no delta, as before. Pinned by the two edited existing tests in Task 1.
4. The warning must fire exactly once per alias item, not once per delta, and never for the owner. Pinned by Task 2 test 7 (two deltas on the alias item).
5. An ordinary single-item stream must be unchanged. Pinned by every existing test in `responses.rs` and the SMA-533 conformance subject, which run unedited in Task 1 step 6 and Task 3 step 3.

---

### Task 1: Gate name emission on the non-blank `call_id`

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/responses.rs` (struct ~274-319, `emit_call_if_unseen` ~354-402, `consume` arms ~461-601, docs ~225-273 and ~652-677, tests from ~723)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: fields `emitted_items: HashSet<String>` and `named_calls: HashMap<String, (String, String)>` on `ResponsesTranslator`; private method `fn claim_name(&mut self, item_id: &str, call_id: &str, name: &str) -> Option<String>`; test helpers `fn names_for<'a>(evs: &'a [ModelEvent], call_id: &str) -> Vec<&'a str>` and `fn args_for(evs: &[ModelEvent], call_id: &str) -> String` in the `tests` module. Task 2 adds the warning inside the `Occupied` arm of `claim_name` and reuses the test helpers.

- [ ] **Step 1: Write the failing tests**

Append to the end of `mod tests` in `responses.rs` (before its closing `}`). These use only `consume` and the existing helpers, so they compile against the current translator.

```rust
    /// Every `name` carried by a `ToolCallDelta` for `call_id`, in emission order.
    fn names_for<'a>(evs: &'a [ModelEvent], call_id: &str) -> Vec<&'a str> {
        evs.iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id: c,
                    name: Some(n),
                    ..
                } if c == call_id => Some(n.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Every `args_delta` for `call_id`, concatenated in emission order.
    fn args_for(evs: &[ModelEvent], call_id: &str) -> String {
        evs.iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta {
                    call_id: c,
                    args_delta,
                    ..
                } if c == call_id => Some(args_delta.as_str()),
                _ => None,
            })
            .collect()
    }

    /// SMA-617 acceptance test. Two `output_item.added` events carry
    /// different `item.id`s but one `call_id`. The core contract allows
    /// exactly one name-carrying `ToolCallDelta` per non-blank `call_id`.
    /// The arguments of both items still reach the call, in wire order
    /// (merge, spec §4.2).
    ///
    /// SYNTHETIC: the shape is malformed and has no capture (SMA-533
    /// provenance rule), so it is a unit test, not a conformance fixture.
    #[test]
    fn two_items_one_call_id_emit_one_name() {
        let mut t = ResponsesTranslator::new();
        let mut all = Vec::new();
        all.extend(t.consume(added_event("fc_1", "call_A", "get_weather")).unwrap());
        all.extend(t.consume(added_event("fc_2", "call_A", "get_weather")).unwrap());
        all.extend(t.consume(delta_event("fc_1", "{\"a\":")).unwrap());
        all.extend(t.consume(delta_event("fc_2", "1}")).unwrap());

        assert_eq!(
            names_for(&all, "call_A"),
            vec!["get_weather"],
            "exactly one name-carrying delta per call_id; got {all:?}"
        );
        assert_eq!(args_for(&all, "call_A"), "{\"a\":1}");
    }

    /// SMA-617 §4.1: `output_item.done` for a second item of an already
    /// named call carries that item's arguments, with no name.
    #[test]
    fn alias_item_reconciled_on_done_carries_no_name() {
        let mut t = ResponsesTranslator::new();
        let mut all = Vec::new();
        all.extend(t.consume(added_event("fc_1", "call_A", "get_weather")).unwrap());
        all.extend(t.consume(delta_event("fc_1", "{\"a\":1}")).unwrap());

        let done = t
            .consume(done_event("fc_2", "call_A", "get_weather", "{\"b\":2}"))
            .unwrap();
        assert_eq!(done.len(), 1, "got {done:?}");
        match &done[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "call_A");
                assert!(name.is_none(), "the call is already named; got {name:?}");
                assert_eq!(args_delta, "{\"b\":2}");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
        all.extend(done);
        assert_eq!(names_for(&all, "call_A"), vec!["get_weather"]);
    }

    /// SMA-617 §4.1: the `response.completed` sweep names a call shared by
    /// two items once, and delivers both items' arguments in output order.
    #[test]
    fn completed_sweep_names_a_shared_call_once() {
        let mut t = ResponsesTranslator::new();
        let evs = t
            .consume(completed_event(
                r#"[{"id":"fc_1","type":"function_call","status":"completed",
                     "arguments":"{\"a\":1}","call_id":"call_A","name":"get_weather"},
                    {"id":"fc_2","type":"function_call","status":"completed",
                     "arguments":"{\"b\":2}","call_id":"call_A","name":"get_weather"}]"#,
            ))
            .unwrap();

        assert_eq!(names_for(&evs, "call_A"), vec!["get_weather"], "got {evs:?}");
        assert_eq!(args_for(&evs, "call_A"), "{\"a\":1}{\"b\":2}");
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

    /// SMA-617: arguments buffered for a second item before its
    /// `output_item.added` are flushed with no name when the call is
    /// already named.
    #[test]
    fn buffered_alias_flush_carries_no_name() {
        let mut t = ResponsesTranslator::new();
        let mut all = Vec::new();
        all.extend(t.consume(added_event("fc_1", "call_A", "get_weather")).unwrap());
        all.extend(t.consume(delta_event("fc_1", "{}")).unwrap());
        assert!(t.consume(delta_event("fc_2", "{\"b\":2}")).unwrap().is_empty());

        let flush = t
            .consume(added_event("fc_2", "call_A", "get_weather"))
            .unwrap();
        assert_eq!(flush.len(), 1, "got {flush:?}");
        match &flush[0] {
            ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                assert_eq!(call_id, "call_A");
                assert!(name.is_none(), "the call is already named; got {name:?}");
                assert_eq!(args_delta, "{\"b\":2}");
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }
        all.extend(flush);
        assert_eq!(names_for(&all, "call_A"), vec!["get_weather"]);
    }

    /// SMA-617 §3.3: a blank `call_id` is not an identity. The core
    /// contract forbids merging two parallel blank-id calls, so each item
    /// keeps its own name. Passes before and after the fix (regression
    /// guard).
    #[test]
    fn blank_call_ids_are_not_merged() {
        let mut t = ResponsesTranslator::new();
        let mut all = Vec::new();
        all.extend(t.consume(added_event("fc_1", "", "get_weather")).unwrap());
        all.extend(t.consume(added_event("fc_2", "", "get_time")).unwrap());
        all.extend(t.consume(delta_event("fc_1", "{}")).unwrap());
        all.extend(t.consume(delta_event("fc_2", "{}")).unwrap());

        let mut names = names_for(&all, "");
        names.sort_unstable();
        assert_eq!(names, vec!["get_time", "get_weather"], "got {all:?}");
    }

    /// SMA-617 §4.1: reconciliation dedup stays per item. An alias item
    /// that already streamed its deltas must not be re-emitted by its
    /// `done` or by the `completed` sweep. Kills the mutant "dedup keyed
    /// on name ownership", which passes every other test here.
    #[test]
    fn streamed_alias_is_not_re_emitted_by_reconciliation() {
        let mut t = ResponsesTranslator::new();
        let mut all = Vec::new();
        all.extend(t.consume(added_event("fc_1", "call_A", "get_weather")).unwrap());
        all.extend(t.consume(added_event("fc_2", "call_A", "get_weather")).unwrap());
        all.extend(t.consume(delta_event("fc_1", "{\"a\":1}")).unwrap());
        all.extend(t.consume(delta_event("fc_2", "{\"b\":2}")).unwrap());

        let mut tail = Vec::new();
        tail.extend(
            t.consume(done_event("fc_2", "call_A", "get_weather", "{\"b\":2}"))
                .unwrap(),
        );
        tail.extend(
            t.consume(completed_event(
                r#"[{"id":"fc_1","type":"function_call","status":"completed",
                     "arguments":"{\"a\":1}","call_id":"call_A","name":"get_weather"},
                    {"id":"fc_2","type":"function_call","status":"completed",
                     "arguments":"{\"b\":2}","call_id":"call_A","name":"get_weather"}]"#,
            ))
            .unwrap(),
        );

        assert!(
            !tail
                .iter()
                .any(|e| matches!(e, ModelEvent::ToolCallDelta { .. })),
            "items whose deltas already streamed must not be re-emitted; got {tail:?}"
        );
        assert!(
            matches!(
                tail.last(),
                Some(ModelEvent::Finish {
                    reason: FinishReason::ToolCalls
                })
            ),
            "expected Finish(ToolCalls) last, got {tail:?}"
        );
        all.extend(tail);
        assert_eq!(names_for(&all, "call_A"), vec!["get_weather"]);
    }
```

- [ ] **Step 2: Run the new tests to verify the pre-fix result**

Run: `cargo test -p paigasus-helikon-providers-openai --lib backend::responses::tests 2>&1 | grep -E "^test |test result"`

Expected (record the actual lines in the task report):
- `two_items_one_call_id_emit_one_name` FAILED (two names)
- `alias_item_reconciled_on_done_carries_no_name` FAILED (name is `Some`)
- `completed_sweep_names_a_shared_call_once` FAILED (two names)
- `buffered_alias_flush_carries_no_name` FAILED (name is `Some`)
- `blank_call_ids_are_not_merged` ok
- `streamed_alias_is_not_re_emitted_by_reconciliation` FAILED (two names)
- every pre-existing test: ok

If any of the five expected failures passes, or fails for another reason (panic text not about names), stop and report it. Do not change the test to make it fail.

- [ ] **Step 3: Replace the field and add `claim_name`**

In the `ResponsesTranslator` struct, replace the `name_emitted` field and its doc comment (~lines 275-281) with:

```rust
    /// Item ids (the internal correlator) whose arguments were delivered in
    /// at least one `ToolCallDelta`.
    ///
    /// This is the reconciliation dedup key of the two SMA-562 sites
    /// (`output_item.done` and the `response.completed` sweep): "did this
    /// item's arguments already reach the consumer?" is a per-item question.
    /// It is also what `response.completed`'s `has_tool_calls` reads. Every
    /// insertion is paired, in the same branch, with the `ToolCallDelta` for
    /// that item, and every emitted `ToolCallDelta` inserts its item, so
    /// `!emitted_items.is_empty()` is exactly "a `ToolCallDelta` was
    /// emitted".
    ///
    /// Every emission site checks this set before it asks
    /// [`Self::claim_name`] for a name, so the name decision runs at most
    /// once per item (SMA-617 spec §3.1).
    emitted_items: HashSet<String>,
    /// Non-blank `call_id` → `(item_id, name)` of the item that emitted the
    /// call's name.
    ///
    /// The name gate. The core contract allows exactly one name-carrying
    /// `ToolCallDelta` per non-blank `call_id`, and two items with different
    /// `item.id`s can carry one `call_id` (SMA-617). Keying the gate on the
    /// item would emit a name per item. A blank `call_id` is never recorded
    /// here: it is not an identity, and the core contract forbids merging
    /// two parallel blank-id calls.
    named_calls: HashMap<String, (String, String)>,
```

In `new()`, replace `name_emitted: HashSet::new(),` with:

```rust
            emitted_items: HashSet::new(),
            named_calls: HashMap::new(),
```

Add this method directly after `new()`:

```rust
    /// Decide the `name` field of an item's first `ToolCallDelta`.
    ///
    /// Returns `Some(name)` when this delta is the first to name the call,
    /// and `None` when another item already named the same non-blank
    /// `call_id`. A blank `call_id` always returns `Some(name)` and records
    /// nothing (see `named_calls`).
    ///
    /// `call_id` and `name` MUST be exactly the values the emitted
    /// `ToolCallDelta` carries: gating on a different `call_id` could
    /// suppress the only name a `call_id` would ever get.
    ///
    /// Touches only `named_calls`. The caller checks and inserts into
    /// `emitted_items`, so this runs at most once per item.
    fn claim_name(&mut self, item_id: &str, call_id: &str, name: &str) -> Option<String> {
        if call_id.is_empty() {
            return Some(name.to_owned());
        }
        match self.named_calls.entry(call_id.to_owned()) {
            Entry::Vacant(slot) => {
                slot.insert((item_id.to_owned(), name.to_owned()));
                Some(name.to_owned())
            }
            Entry::Occupied(_) => None,
        }
    }
```

Change the import at the top of the file (line 7) to:

```rust
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
```

- [ ] **Step 4: Route the three emission sites and `has_tool_calls` through the new fields**

**(a) `emit_call_if_unseen`.** Replace

```rust
        if self.name_emitted.contains(&item_id) {
            return None;
        }
```

with

```rust
        if self.emitted_items.contains(&item_id) {
            return None;
        }
```

and replace the tail

```rust
        self.name_emitted.insert(item_id);
        Some(ModelEvent::ToolCallDelta {
            call_id: fc.call_id.clone(),
            name: Some(fc.name.clone()),
            args_delta,
        })
```

with

```rust
        // Gate on exactly the call_id and name this delta carries.
        let name = self.claim_name(&item_id, &fc.call_id, &fc.name);
        self.emitted_items.insert(item_id);
        Some(ModelEvent::ToolCallDelta {
            call_id: fc.call_id.clone(),
            name,
            args_delta,
        })
```

**(b) `ResponseOutputItemAdded` buffered flush.** Replace

```rust
                            if !buffered.is_empty() {
                                // First (and only) ToolCallDelta for these buffered args:
                                // emit name here since this is the first time we know the
                                // call_id; mark name_emitted so it won't repeat.
                                self.name_emitted.insert(item_id.clone());
                                return Ok(vec![ModelEvent::ToolCallDelta {
                                    call_id,
                                    name: Some(name),
                                    args_delta: buffered,
                                }]);
                            }
```

with

```rust
                            if !buffered.is_empty() {
                                // First ToolCallDelta for this item. The item cannot be
                                // in `emitted_items` yet: `pending_args` only grows while
                                // the item is absent from `item_to_call`, and every
                                // emitted item is in `item_to_call`. The name is `None`
                                // when another item already named this call_id (SMA-617).
                                let name = self.claim_name(&item_id, &call_id, &name);
                                self.emitted_items.insert(item_id.clone());
                                return Ok(vec![ModelEvent::ToolCallDelta {
                                    call_id,
                                    name,
                                    args_delta: buffered,
                                }]);
                            }
```

**(c) `ResponseFunctionCallArgumentsDelta`.** Replace

```rust
                let already_emitted = self.name_emitted.contains(&e.item_id);
                if let Some((call_id, fn_name)) = self.item_to_call.get(&e.item_id) {
                    let name = if already_emitted {
                        None
                    } else {
                        self.name_emitted.insert(e.item_id.clone());
                        Some(fn_name.clone())
                    };
                    Ok(vec![ModelEvent::ToolCallDelta {
                        call_id: call_id.clone(),
                        name,
                        args_delta: e.delta,
                    }])
                } else if e.delta.is_empty() {
```

with

```rust
                if let Some((call_id, fn_name)) = self.item_to_call.get(&e.item_id) {
                    // Cloned out so `claim_name` can borrow `self` mutably.
                    let (call_id, fn_name) = (call_id.clone(), fn_name.clone());
                    // Only an item's first delta asks for a name (spec §3.1).
                    let name = if self.emitted_items.insert(e.item_id.clone()) {
                        self.claim_name(&e.item_id, &call_id, &fn_name)
                    } else {
                        None
                    };
                    Ok(vec![ModelEvent::ToolCallDelta {
                        call_id,
                        name,
                        args_delta: e.delta,
                    }])
                } else if e.delta.is_empty() {
```

**(d) `ResponseCompleted`.** Replace `!self.name_emitted.is_empty(),` with `!self.emitted_items.is_empty(),`.

- [ ] **Step 5: Edit the two existing tests that read the removed field**

In `completed_skips_incomplete_output_item`, replace

```rust
        assert!(
            t.name_emitted.is_empty(),
            "an incomplete item must never be marked as emitted"
        );
```

with

```rust
        assert!(
            t.emitted_items.is_empty() && t.named_calls.is_empty(),
            "an incomplete item must never be marked as emitted"
        );
```

In `completed_skips_in_progress_output_item`, replace

```rust
        assert!(
            t.name_emitted.is_empty(),
            "an in-progress item must never be marked as emitted"
        );
```

with

```rust
        assert!(
            t.emitted_items.is_empty() && t.named_calls.is_empty(),
            "an in-progress item must never be marked as emitted"
        );
```

Keep both tests' `Finish { Stop }` assertions unchanged.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-providers-openai --all-features 2>&1 | grep -E "FAILED|panicked|test result"`
Expected: every `test result:` line reports `0 failed`. No `FAILED` line.

- [ ] **Step 7: Update every remaining `name_emitted` reference in docs and comments**

Run: `grep -n name_emitted crates/paigasus-helikon-providers-openai/src/backend/responses.rs`

Rewrite each hit so it is true after the change. Rules:
- Where the text is about dedup or "was anything emitted" (the `response.completed` bullet of the struct doc, the `item_to_call` field doc, the `emit_call_if_unseen` doc bullets, the `response.completed` arm comment, the `terminal_events` doc, and the test docs of `terminal_events_completed_with_tool_calls_maps_to_tool_calls`, `completed_after_deltas_emits_only_terminal_pair`, `completed_skips_incomplete_output_item`, `added_then_completed_omitting_the_call_reports_stop`, and the test near line 1032), say `emitted_items`.
- Where the text is about the name, say `named_calls` / `claim_name`.
- In the struct doc, change the `function_call_arguments.delta` bullet to: "`ToolCallDelta` with name-emission gating: the name is emitted once per non-blank `call_id` (`named_calls`, SMA-617), and once per item for a blank `call_id`, then `None`."
- Keep the existing SMA-562 reasoning; only the field names and the per-item/per-call wording change.

Then run the grep again. Expected: no output.

- [ ] **Step 8: Format, lint, test, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-providers-openai --all-features 2>&1 | grep -E "FAILED|test result"
git add crates/paigasus-helikon-providers-openai/src/backend/responses.rs
git commit -m "fix(providers-openai): SMA-617 gate responses tool-call names on the call_id

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Expected: clippy clean, `0 failed` everywhere, one new commit.

---

### Task 2: Warn once when a second item aliases onto a named call

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/responses.rs` (`claim_name`, tests module)

**Interfaces:**
- Consumes: `claim_name`, `named_calls`, the test helpers `added_event` and `delta_event` from Task 1 / the existing module.
- Produces: one `tracing::warn!` on target `paigasus::openai::responses` whose fields contain the `call_id`, both `item_id`s, and both names.

- [ ] **Step 1: Write the failing test**

Append to `mod tests`:

```rust
    /// SMA-617 §3.2: the alias shape is malformed, so it is logged. Exactly
    /// one WARN per alias item (not per delta, and never for the owner),
    /// naming both items. Uses the crate-wide capture: see
    /// `crate::test_tracing` for why a per-test subscriber is not allowed.
    #[test]
    fn alias_item_warns_once() {
        use crate::test_tracing;

        test_tracing::start();
        let mut t = ResponsesTranslator::new();
        t.consume(added_event("fc_1", "call_A", "get_weather")).unwrap();
        t.consume(added_event("fc_2", "call_A", "get_weather")).unwrap();
        t.consume(delta_event("fc_1", "{\"a\":")).unwrap();
        t.consume(delta_event("fc_2", "1")).unwrap();
        t.consume(delta_event("fc_2", "}")).unwrap();

        let alias_warns: Vec<_> = test_tracing::captured()
            .into_iter()
            .filter(|(target, fields)| {
                target == "paigasus::openai::responses"
                    && fields.contains("fc_1")
                    && fields.contains("fc_2")
            })
            .collect();
        assert_eq!(
            alias_warns.len(),
            1,
            "expected exactly one alias warning naming both items; got {alias_warns:?}"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-providers-openai --lib backend::responses::tests::alias_item_warns_once 2>&1 | grep -E "^test |panicked|test result"`
Expected: FAILED, with `got []` in the panic message.

- [ ] **Step 3: Add the warning**

In `claim_name`, replace

```rust
            Entry::Occupied(_) => None,
```

with

```rust
            Entry::Occupied(owner) => {
                // Runs at most once per item (the caller gates on
                // `emitted_items`), so this warns once per alias item, and
                // never for the owner, which reached `Vacant`.
                let (owner_item, owner_name) = owner.get();
                tracing::warn!(
                    target: "paigasus::openai::responses",
                    %call_id,
                    owner_item_id = %owner_item,
                    owner_name = %owner_name,
                    item_id = %item_id,
                    name = %name,
                    "two function_call items share one call_id; the name is emitted \
                     once, and the arguments of both items go to the one call"
                );
                None
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-providers-openai --all-features 2>&1 | grep -E "FAILED|panicked|test result"`
Expected: `0 failed` everywhere.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-openai --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-openai/src/backend/responses.rs
git commit -m "fix(providers-openai): SMA-617 warn when two responses items share a call_id

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Correct the cross-file docs that describe the old behaviour

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs:689-692`
- Modify: `tests/provider-stream-conformance/src/check.rs:82-85`
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (the `openai_responses` module doc, starts ~line 2767)

**Interfaces:**
- Consumes: the behaviour from Tasks 1-2 and the test names `two_items_one_call_id_emit_one_name` and its siblings.
- Produces: doc comments only. No code change.

- [ ] **Step 1: Edit `chat.rs`**

In the doc comment of `handle_tool_call_chunk`, replace

```rust
    /// `u32`; unticketed and left as-is. And this crate's sibling `responses`
    /// translator has no blank-id handling whatsoever; its own name-dedup
    /// defect is open as SMA-617.
```

with

```rust
    /// `u32`; unticketed and left as-is. This crate's sibling `responses`
    /// translator gates names on the non-blank `call_id` too, and does not
    /// merge blank-id calls either (SMA-617).
```

- [ ] **Step 2: Edit `check.rs` and `conformance.rs`**

In `tests/provider-stream-conformance/src/check.rs`, replace

```rust
    // identify a call, so both first-party chat translators refuse to merge
    // parallel blank-id calls and each such call carries its own name under
    // "" (SMA-566, SMA-616). Two of them therefore report `count: 2` here.
```

with

```rust
    // identify a call, so both first-party chat translators and the OpenAI
    // Responses translator refuse to merge parallel blank-id calls, and each
    // such call carries its own name under "" (SMA-566, SMA-616, SMA-617).
    // Two of them therefore report `count: 2` here.
```

In `tests/provider-stream-conformance/tests/conformance.rs`, find the `openai_responses` module doc (search for `checked against the OpenAI Responses`). Directly before its `/// # Fixture provenance` heading, insert:

```rust
/// # SMA-617's alias regression coverage lives in the crate's own unit tests
///
/// Every function call this module scripts has its own `call_id`, and no two
/// items share one. The two-items-one-`call_id` shape that
/// `ResponsesTranslator` gates on the call (SMA-617) therefore never arises
/// from these bytes, and assertion 7 passes here without exercising it. Under
/// this suite's fixture-provenance rule the shape has no capture anywhere in
/// the repo and must not be invented, so it is not registered as a scenario.
///
/// Read this subject's `conforms` test as confirming the translator behaves
/// correctly on the wire shapes OpenAI is actually observed to send, **not**
/// as a standing regression guard for the SMA-617 fix. That guard is
/// `two_items_one_call_id_emit_one_name` and its siblings in
/// `crates/paigasus-helikon-providers-openai/src/backend/responses.rs`.
///
```

Before you save, confirm the first sentence is true: read the `openai_responses` module in `conformance.rs` and check that no two scripted function-call items share a `call_id`. If one pair does, rewrite that sentence to state what the module actually scripts, and report it. Also confirm that "assertion 7" is the name-per-`call_id` assertion by reading the numbered list in `check.rs`; if the number differs, use the correct number.

- [ ] **Step 3: Verify and commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-provider-stream-conformance --all-features 2>&1 | grep -E "FAILED|test result"
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-openai --all-features --no-deps
git add crates/paigasus-helikon-providers-openai/src/backend/chat.rs tests/provider-stream-conformance/src/check.rs tests/provider-stream-conformance/tests/conformance.rs
git commit -m "docs(providers): SMA-617 record responses call-id name gating in sibling docs

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Expected: clippy clean, `0 failed`, docs build clean.
