# SMA-621 Blank-`call_id` Accumulator Refusal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ModelTurnAccumulator::finish()` fails with a clear `ambiguous tool calls:` error when two or more name-carrying deltas arrive under `call_id: ""`, instead of merging them.

**Architecture:** `observe` counts name-carrying deltas under `""` and keeps their names. `finish()` checks that count before it parses any arguments. Then the docs, the litellm pinning test and conformance assertion 7 change to match.

**Tech Stack:** Rust 1.94, `serde_json`, `tracing` (already a `core` dependency).

**Spec:** `docs/superpowers/specs/2026-11-21-blank-call-id-accumulator-design.md`

## Global Constraints

- No public signature change. `finish()` stays `pub fn finish(self) -> Result<ModelTurn, String>`.
- No hand-bumped crate version and no CHANGELOG edit. release-plz does both.
- Error prefix, exactly: `ambiguous tool calls:`.
- Names in the message and the warn: `{:?}` formatted, at most 8 listed then `and N more`, each cut to 64 characters before formatting. Never argument text.
- `Some("")` counts as a name. Equal names count separately.
- Count 0 or 1 under `""`, and every non-blank id: behaviour stays exactly as on `main`.
- Commit format: `<type>(<scope>): SMA-621 <lowercase subject>`. Scopes used here: `core` and `providers` (`tests` is not in the `.versionrc` allowlist). Stage explicit paths only, never `git add -A` (`.env` is not ignored).
- Work in `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-621`. Guard every `cd` with `|| exit 1`. Never run `git checkout`, `git switch`, `git reset`, `git stash` or `git rebase`.
- Run long `cargo` commands in the foreground and wait for the result.
- Before each commit: `cargo fmt --all` and `cargo clippy -p <crate> --all-features --all-targets -- -D warnings`.

## Review Focus

1. A turn with an ambiguous `""` bucket **and** text **and** a valid non-blank call: the whole turn must fail (pinned in Task 1 `ambiguous_blank_bucket_fails_the_whole_turn`).
2. A model that sends a very long or control-character tool name: the message stays bounded and escaped (pinned in Task 1 `ambiguity_message_bounds_and_escapes_names`).
3. Nine or more blank-id calls: the message lists 8 and says `and 1 more` (same test).
4. Argument text that holds a secret: it never appears in the message (pinned in Task 1 `ambiguity_message_omits_argument_text`).
5. One blank-id call whose nameless args delta arrives before its name: it still succeeds (pinned in Task 1 `one_blank_call_with_args_before_name_is_ok`).

---

### Task 1: Refuse ambiguous blank-id buckets in `core`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs` — `ToolCallDelta` docs (lines 183-210), `ModelTurn` docs (456-469), `ModelTurnAccumulator` struct, `new`, `observe` (585-601), `finish` (622-631), and `model_turn_tests` (634-720).

**Interfaces:**
- Consumes: nothing new.
- Produces: the error text `ambiguous tool calls: {N} parallel tool calls arrived under a blank call_id (names: {list}); their arguments cannot be attributed to a call`. Later tasks assert only on the prefix `ambiguous tool calls:`.

- [ ] **Step 1: Write the failing tests** in `model_turn_tests`. Add a helper and these tests. Each test whose result differs on `main` carries the doc line `/// Confirmed to FAIL against `main`.` plus one line saying what `main` returns.

```rust
fn tc(id: &str, name: Option<&str>, args: &str) -> ModelEvent {
    ModelEvent::ToolCallDelta { call_id: id.into(), name: name.map(Into::into), args_delta: args.into() }
}
fn run(evs: &[ModelEvent]) -> Result<ModelTurn, String> {
    let mut acc = ModelTurnAccumulator::new("a1");
    for e in evs { acc.observe(e); }
    acc.finish()
}

#[test] // main: serde_json trailing-characters error
fn parallel_blank_calls_with_args_fail_as_ambiguous() {
    let e = run(&[tc("", Some("alpha"), ""), tc("", Some("beta"), ""),
                  tc("", None, "{\"p\":1}"), tc("", None, "{\"q\":2}")]).unwrap_err();
    assert!(e.starts_with("ambiguous tool calls:"), "{e}");
    assert!(e.contains("2 parallel tool calls"), "{e}");
    assert!(e.contains("\"alpha\"") && e.contains("\"beta\""), "{e}");
}
#[test] // main: Ok with one call
fn parallel_blank_calls_without_args_fail_as_ambiguous() {
    let e = run(&[tc("", Some("alpha"), ""), tc("", Some("beta"), "")]).unwrap_err();
    assert!(e.starts_with("ambiguous tool calls:"), "{e}");
}
#[test] // main: Ok, delete_all runs with search's args
fn blank_call_misattribution_fails_as_ambiguous() {
    let e = run(&[tc("", Some("delete_all"), ""), tc("", Some("search"), "{\"q\":1}")]).unwrap_err();
    assert!(e.starts_with("ambiguous tool calls:"), "{e}");
}
#[test] // main: Ok with one call
fn equal_blank_names_count_as_two_calls() {
    let e = run(&[tc("", Some("alpha"), "{}"), tc("", Some("alpha"), "{}")]).unwrap_err();
    assert!(e.contains("2 parallel tool calls"), "{e}");
}
#[test] // main: Ok with one call
fn empty_name_counts_as_a_name() {
    assert!(run(&[tc("", Some(""), ""), tc("", Some("beta"), "")]).unwrap_err()
        .starts_with("ambiguous tool calls:"));
}
#[test] // main: Ok with two items
fn ambiguous_blank_bucket_fails_the_whole_turn() {
    let e = run(&[ModelEvent::TokenDelta { text: "hi".into() },
                  tc("c1", Some("ok_tool"), "{}"),
                  tc("", Some("alpha"), ""), tc("", Some("beta"), "")]).unwrap_err();
    assert!(e.starts_with("ambiguous tool calls:"), "{e}");
}
#[test] // main: the c1 serde_json error
fn ambiguity_is_reported_before_any_argument_parse() {
    let e = run(&[tc("c1", Some("t"), "{not json"),
                  tc("", Some("alpha"), ""), tc("", Some("beta"), "")]).unwrap_err();
    assert!(e.starts_with("ambiguous tool calls:"), "{e}");
}
#[test] // passes on main too (the serde_json error has no argument text); it pins the rule against a later change
fn ambiguity_message_omits_argument_text() {
    let e = run(&[tc("", Some("a"), "{\"token\":\"SECRET-123\"}"), tc("", Some("b"), "{}")]).unwrap_err();
    assert!(!e.contains("SECRET-123"), "{e}");
}
#[test] // main: Ok
fn ambiguity_message_bounds_and_escapes_names() {
    let long = "x".repeat(200);
    let mut evs = vec![tc("", Some("bad\nname"), ""), tc("", Some(&long), "")];
    for i in 0..7 { evs.push(tc("", Some(&format!("t{i}")), "")); }
    let e = run(&evs).unwrap_err();
    assert!(e.contains("9 parallel tool calls"), "{e}");
    assert!(e.contains("\"bad\\nname\""), "{e}");      // escaped, no raw newline
    assert!(!e.contains('\n'), "{e}");
    assert!(!e.contains(&"x".repeat(65)), "{e}");      // cut to 64 chars
    assert!(e.contains("and 1 more"), "{e}");          // 8 listed of 9
}
```

Controls (pass on `main` and after; no "Confirmed to FAIL" line):

```rust
#[test]
fn one_blank_call_is_accumulated() {
    let t = run(&[tc("", Some("alpha"), "{\"p\""), tc("", None, ":1}")]).unwrap();
    assert!(matches!(&t.items[..], [crate::Item::ToolCall { call_id, name, args }]
        if call_id.is_empty() && name == "alpha" && *args == serde_json::json!({"p":1})));
}
#[test]
fn one_blank_call_with_args_before_name_is_ok() {
    let t = run(&[tc("", None, "{\"p\":"), tc("", Some("alpha"), "1}")]).unwrap();
    assert_eq!(t.items.len(), 1);
}
#[test]
fn two_non_blank_calls_give_two_items() {
    let t = run(&[tc("c1", Some("a"), "{}"), tc("c2", Some("b"), "{}")]).unwrap();
    assert_eq!(t.items.len(), 2);
}
```

- [ ] **Step 2: Run the tests and confirm the expected failures**

Run: `cargo test -p paigasus-helikon-core --lib model_turn_tests`
Expected: eight new tests FAIL (some with `unwrap_err` on an `Ok`, some on the prefix or count assertion). `ambiguity_message_omits_argument_text`, the three controls and the four existing tests PASS. If any other new test passes here, stop and report: it does not pin the change.

- [ ] **Step 3: Implement the detection**

- Add a field to `ModelTurnAccumulator`: `blank_names: Vec<String>`. Initialize it empty in `new`.
- In `observe`, `ToolCallDelta` arm: if `call_id.is_empty()` and `name` is `Some(n)`, push `n` to `blank_names`. Keep the existing bucket logic unchanged for all ids.
- In `finish`, before `build_items`: if `blank_names.len() >= 2`, build the message, emit `tracing::warn!(count, names = %list, "…")` with the same bounded list, and return `Err(message)`.
- Put the list formatting in a private `fn format_blank_names(names: &[String]) -> String`: take the first 8, cut each to 64 chars (by `char`, not bytes), format with `{:?}`, join with `", "`, then append `, and N more` when more than 8.

- [ ] **Step 4: Update the docs in `model.rs`**

- `ToolCallDelta` (lines 190-197 and the `name` field doc at 201-206): state that each blank-id call carries **exactly one** name-carrying delta under `""`. Replace "This crate's own `ModelTurnAccumulator` does not follow that advice…" with: the accumulator merges the deltas of a single blank-id call, and refuses the turn (`finish()` returns an `ambiguous tool calls:` error) when two or more name-carrying deltas arrive under `""`, because the arguments cannot be attributed.
- `ModelTurn` struct doc (456-462): add that two or more blank-id calls make `finish()` fail instead of producing items.
- `finish` doc (622-623): `Err(String)` is invalid JSON in tool-call args, **or** an ambiguous blank-id bucket (checked first, before any parse).

- [ ] **Step 5: Run the tests and lints**

Run: `cargo test -p paigasus-helikon-core --lib model_turn_tests`, then `cargo fmt --all`, then `cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings`, then `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --no-deps`.
Expected: all tests PASS; clippy and doc clean.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/model.rs
git commit -m "fix(core): SMA-621 refuse a turn with parallel blank call_id tool calls"
```

### Task 2: Update the litellm pinning test and the temporal doc

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/src/stream.rs` — test at ~2391 and its doc comment (~2352-2390), assertion text at ~2424-2429.
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activities.rs:80-81`.
- Read only, confirm no edit: `crates/paigasus-helikon-providers-openai/src/backend/chat.rs:692`, `crates/paigasus-helikon-providers-openai/src/backend/responses.rs:409, 1166`.

**Interfaces:**
- Consumes: Task 1's prefix `ambiguous tool calls:`.

- [ ] **Step 1: Change the test.** Rename `withheld_upgrades_keep_parallel_blank_calls_merged` to `withheld_upgrades_make_parallel_blank_calls_fail_loudly`. Replace the final `assert!(accumulated(&evs).is_err(), …)` with:

```rust
let err = accumulated(&evs).expect_err("two blank-id calls must not reassemble");
assert!(
    err.starts_with("ambiguous tool calls:"),
    "the accumulator refuses parallel blank call_ids by name, not by a \
     serde_json trailing-characters error (SMA-621): {err}"
);
```

- [ ] **Step 2: Run the test.** On `main` the new prefix assertion fails, because the error there is the `serde_json` trailing-characters message. Task 1 Step 2 already proved that for the same shape in `core`, so do not revert any file to prove it again. Add `/// Confirmed to FAIL against `main` (serde_json error, not the ambiguity error).` to the test's doc comment.

Run: `cargo test -p paigasus-helikon-providers-litellm --lib withheld_upgrade`
Expected: PASS (all tests matching the filter).

- [ ] **Step 3: Rewrite the stale prose.** In the doc comment above the test (and the `withheld_upgrade` text at ~2352-2369 and ~2386-2389), replace every claim that the accumulator merges blank `call_id`s and that the turn fails on concatenated JSON with: since SMA-621 the accumulator refuses two name-carrying deltas under `""` with an `ambiguous tool calls:` error; the turn still fails, and the failure now names the cause. Keep the reasoning about why the gate keeps the calls under `""`. Leave the text at ~2228 as it is (it describes one call). Grep the file for `merges blank`, `concatenate`, `SMA-619 §2.2` and fix any other stale statement about the accumulator.

- [ ] **Step 4: Temporal doc.** In `activities.rs:80-81`, change "(invalid tool-call-argument JSON)" to "(invalid tool-call-argument JSON, or parallel tool calls under a blank `call_id`)".

- [ ] **Step 5: Confirm the openai comments.** Read `chat.rs:686-695` and `responses.rs:405-412, 1162-1170`. If a line states that the accumulator merges blank ids, fix it with the same wording as Step 3. Otherwise make no edit and say so in the report.

- [ ] **Step 6: Lint and commit**

Run: `cargo fmt --all`, `cargo clippy -p paigasus-helikon-providers-litellm -p paigasus-helikon-runtime-temporal --all-features --all-targets -- -D warnings`, `cargo test -p paigasus-helikon-providers-litellm --lib`.
Expected: clean, all PASS.

```bash
git add crates/paigasus-helikon-providers-litellm/src/stream.rs crates/paigasus-helikon-runtime-temporal/src/activities.rs
git commit -m "test(providers): SMA-621 pin the ambiguity error for withheld blank call_id upgrades"
```

(Add the openai files to `git add` only if Step 5 edited them.)

### Task 3: Limit conformance assertion 7 for blank ids

**Files:**
- Modify: `tests/provider-stream-conformance/src/check.rs:75-108` and its `tests` module (from line 114).

**Interfaces:**
- Consumes: the existing `Violation::ToolNameNotExactlyOnce { call_id, count }` and the test helper `tool(id, name, args)` in the `tests` module.

- [ ] **Step 1: Write the tests**

```rust
/// Two blank-id calls each carry their own name, as the contract requires.
/// Confirmed to FAIL against `main` (reported count: 2).
#[test]
fn two_blank_id_calls_with_one_name_each_conform() {
    let evs = vec![tool("", Some("alpha"), "{}"), tool("", Some("beta"), "{}"),
                   Ok(ModelEvent::Finish { reason: FinishReason::ToolCalls })];
    assert_eq!(classify(&evs, Scenario::ToolCallCleanStop, false), None);
}

/// A blank-id group with no name is still a translator defect.
#[test]
fn blank_id_with_no_name_is_a_violation() {
    let evs = vec![tool("", None, "{}"),
                   Ok(ModelEvent::Finish { reason: FinishReason::ToolCalls })];
    assert_eq!(classify(&evs, Scenario::ToolCallCleanStop, false),
               Some(Violation::ToolNameNotExactlyOnce { call_id: String::new(), count: 0 }));
}
```

- [ ] **Step 2: Run and confirm**

Run: `cargo test -p paigasus-helikon-provider-stream-conformance --lib check::tests`
Expected: `two_blank_id_calls_with_one_name_each_conform` FAILS; `blank_id_with_no_name_is_a_violation` PASSES (it is a control).

- [ ] **Step 3: Implement.** In the final loop, a group with `call_id.is_empty()` is a violation only when `count == 0`; every other group keeps `count != 1`. Replace the comment at lines 82-90 with the spec's reason (spec "Updates" item 6): assertion 7 grades translators; two names under `""` are what the contract requires for two blank-id calls; a nameless blank-id group is still a translator defect; refusing the ambiguous turn is a consumer decision that `core`'s `model_turn_tests` pin (SMA-621).

- [ ] **Step 4: Run, lint, commit**

Run: `cargo test -p paigasus-helikon-provider-stream-conformance`, `cargo fmt --all`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`.
Expected: all PASS, clean.

```bash
git add tests/provider-stream-conformance/src/check.rs
git commit -m "test(providers): SMA-621 scope conformance assertion 7 to non-blank call ids"
```

### Task 4: Full-gate check

- [ ] **Step 1:** Run `cargo test --workspace --all-features` and `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`. Expected: PASS, clean.
- [ ] **Step 2:** Run `git grep -n "merges blank-id calls\|deliberately merges blank\|accumulator merges blank"` . Expected: matches only in `docs/superpowers/` (spec history), none in `crates/` or `tests/`.
- [ ] **Step 3:** Run `bash scripts/check-breaking-cascade.sh` if it runs locally without a PR context. Expected: pass (no breaking bump). If it needs a PR context, skip and say so.
