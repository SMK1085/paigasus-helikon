# SMA-854 one position for each Anthropic text block — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Anthropic replay sends each text block of a response at its own
position, so a thinking block between two text blocks keeps its prefix.

**Architecture:** The stream records `{"kind":"text","index":N,"bytes":B}` for
each text block. The request translator cuts the one joined `Text` part of the
core into pieces with those byte lengths. It cuts late, in
`WireMessage::into_value`, after the keep rule and the drop pass, so every path
that drops the thinking builds exactly as today.

**Tech stack:** Rust 1.94, `serde_json`, `tracing`, `reqwest` (live test only).

**Spec:** `docs/superpowers/specs/2026-11-22-anthropic-per-text-block-positions-design.md`.
Read it before each task. The spec wins over this plan if they disagree.

All paths below are relative to the worktree root
`/Users/smaschek/dev/paigasus/paigasus-helikon/.worktrees/sma-854`. Write and
edit files only under that root. `A` = `crates/paigasus-helikon-providers-anthropic`.

## Global constraints

- Work only in the worktree. Never run `git checkout`, `git switch`, `git reset`,
  `git rebase`, `git stash` or `git merge`. Never move HEAD off
  `feature/sma-854-anthropic-stream-records-a-position-only-for`.
- Work in the foreground. Do not end a task until every command has a final
  status.
- Commit format: `<type>(<scope>): SMA-854 <lowercase subject>`. Allowed scopes
  for this plan: `providers`, `core`, `specs`. End each message with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Stage files by explicit path. Never `git add -A` or `git add .`.
- Run `cargo fmt --all` before each commit.
- No log line and no assertion message holds text content, a signature or
  redacted data. Lengths, indexes and counts only.
- No change to public API. `AnthropicData` stays `pub(crate)`.
- No change to `is_blank` behaviour outside the split path.
- Test values that look like signatures use the `SIG_FAKE_n` form.

## Review focus

1. A hand-off transcript, a history that lost its calls, or a last assistant
   message, all with `bytes` data, must build byte-equal to
   `without_anthropic` (Task 3 tests 9 to 11).
2. A hand-built or reordered part list must never reorder text (Task 2
   refusals `NotIncreasing` and `IndexCollision`).
3. A corrupt stored `bytes` (`u64::MAX`, a value that cuts a `char`) must never
   panic in `build_body` (Task 2 tests).
4. A whitespace-only joined text must not change `only_tool_uses`,
   `thinking_only` or the "no blocks" decision in `Translator::step` (Task 3
   test 8).
5. A turn with structured output (synthesized tool JSON in the text) must fall
   back, not split (Task 3 test 12).

---

### Task 1: the stream records one position for each text block

**Files:**

- Modify: `A/src/provider_data.rs` (the `AnthropicData::Text` variant and its doc
  at about line 85, the shapes test)
- Modify: `A/src/stream.rs` (`BlockState::Text`, `text_position_recorded`,
  `ContentBlockDelta::TextDelta`, `stop_block`, tests)
- Modify: every test that builds `AnthropicData::Text { index: n }`
  (`A/src/stream.rs`, `A/src/translate/request.rs`, `A/src/translate/mod.rs`,
  `A/src/live_round_trip.rs`). Find them with
  `grep -rn "AnthropicData::Text" A/src`.

**Interfaces:**

- Produces: `AnthropicData::Text { index: u32, bytes: Option<u64> }` with
  `#[serde(default, skip_serializing_if = "Option::is_none")]` on `bytes`.
  Serialized: `{"kind":"text","index":3,"bytes":5}`. With `None`:
  `{"kind":"text","index":3}`.
- Produces: the stream emits one `Text` position for each text block that
  stops, through `position()`, with `bytes: Some(sum of the UTF-8 lengths of its
  text_delta texts)`.

- [ ] **Step 1: write the failing tests**

In `provider_data.rs` tests:

- `data_serializes_to_the_documented_shapes`: change the text case to
  `AnthropicData::Text { index: 2, bytes: Some(5) }` →
  `json!({"kind":"text","index":2,"bytes":5})`.
- New `text_position_without_bytes_parses`:
  `AnthropicData::deserialize(json!({"kind":"text","index":2}))` equals
  `AnthropicData::Text { index: 2, bytes: None }`, and serializing that value
  gives `json!({"kind":"text","index":2})`.

In `stream.rs` tests (use the existing helpers `start`, `stop`, `block`,
`provider_data`, and a text delta event):

- New `each_text_block_records_its_position_and_bytes`: events
  `[thinking(0) with signature SIG_FAKE_1, text(1) "A", thinking(2) with
  SIG_FAKE_2, text(3) "é ü", tool_use(4) "tu_1"]`. `provider_data(&events)`
  equals, in order: `block(0,…)`, `(Text{index:1,bytes:Some(1)}, None)`,
  `block(2,…)`, `(Text{index:3,bytes:Some(5)}, None)`,
  `(ToolUse{index:4}, Some("tu_1"))`.
- New `text_before_first_kept_block_waits_then_flushes_in_order`: events
  `[text(0) "A", thinking(1) SIG_FAKE_1, text(2) "B"]` give
  `[block(1,…), Text{0,Some(1)}, Text{2,Some(1)}]`.
- New `text_delta_for_a_block_that_is_not_open_counts_nowhere`: events
  `[thinking(0) SIG_FAKE_1, start text(1), text_delta(9) "XX", text_delta(1)
  "A", stop(1)]`. The `Text` position is `{index:1, bytes:Some(1)}`, and the
  `TokenDelta` events still hold `"XX"` and `"A"`.
- Existing tests that expect `Text { index: n }`: set `bytes` to the byte length
  of that block's text in the test.

- [ ] **Step 2: run the tests, and check that they fail**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib provider_data`, then
`cargo test -p paigasus-helikon-providers-anthropic --lib stream` (cargo accepts
one test-name filter per command).
Expected: compile error (`bytes` field missing), or FAIL.

- [ ] **Step 3: implement**

- `provider_data.rs`: add the field. Change the doc from "The position of the
  first text block of the response" to "The position of one text block of the
  response. `bytes` is the UTF-8 byte length of its text. Data from before
  SMA-854 has no `bytes`."
- `stream.rs`: `BlockState::Text { bytes: u64 }`, starting at 0. On
  `TextDelta`, if `self.blocks.get_mut(&index)` is `Some(BlockState::Text {
  bytes })`, add `text.len() as u64` with `saturating_add`. Else log
  `tracing::debug!(target: "paigasus::anthropic::stream", index, "text_delta for
  a block that is not an open text block")`. Always push the `TokenDelta`, as
  today. Remove `text_position_recorded` and its doc. In `stop_block`, every
  `BlockState::Text { bytes }` calls `self.position(AnthropicData::Text { index,
  bytes: Some(bytes) }.into_event(None), out)`. Update the `stop_block` doc to
  say "of each text block".
- The other test sites from the grep: add `bytes: None` where the test is about
  old data, else the byte length of its text. In `request.rs` tests, change the
  helper `text_pos(i)` to give `bytes: None`, and add
  `text_pos_bytes(i: u32, bytes: u64) -> ContentPart`.

- [ ] **Step 4: run the tests, and check that they pass**

Run: `cargo test -p paigasus-helikon-providers-anthropic`
Expected: PASS, except `two_text_blocks_join_at_first_text_position`. That test
still asserts `"AB"`, and it must still pass here: the replay does not split
yet. If any other test fails, stop and report.

- [ ] **Step 5: commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-anthropic/src
git commit -m "fix(providers): SMA-854 record a position for each anthropic text block" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: the pure split-plan function

**Files:**

- Modify: `A/src/translate/request.rs` (new items next to `assistant_blocks`,
  new test module `split_plan_tests` at the end of the file)

**Interfaces:**

- Produces:

```rust
/// Why a message with two or more `Text` positions does not split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitRefusal {
    MissingBytes,
    NotIncreasing,
    IndexCollision,
    SumMismatch,
    NotCharBoundary,
}

/// One piece: the content-block index and the byte range in the joined text.
type Cut = (u32, std::ops::Range<usize>);

/// `positions`: the `(index, bytes)` of each `Text` position, in part order.
/// `other_indexes`: the indexes of the `Block` and `ToolUse` positions of the
/// message. Returns one cut per position, in part order, including empty ones.
fn split_plan(
    text: &str,
    positions: &[(u32, Option<u64>)],
    other_indexes: &[u32],
) -> Result<Vec<Cut>, SplitRefusal>;
```

- The caller (Task 3) calls `split_plan` only with two or more positions.
- Checks, in this order: every `bytes` is `Some` (`MissingBytes`); indexes
  strictly increase (`NotIncreasing`); no index is in `other_indexes`
  (`IndexCollision`); the sum, with `checked_add` and `usize::try_from`, equals
  `text.len()`, and an overflow is `SumMismatch`; each cut end is
  `text.is_char_boundary(end)` (`NotCharBoundary`). No slicing in this
  function.

- [ ] **Step 1: write the failing tests** (in `split_plan_tests`)

- `splits_in_part_order`: `split_plan("AB", &[(1,Some(1)),(3,Some(1))], &[0,2,4])`
  is `Ok(vec![(1,0..1),(3,1..2)])`.
- `multi_byte_split`: `split_plan("éü", &[(1,Some(2)),(3,Some(2))], &[])` is
  `Ok(vec![(1,0..2),(3,2..4)])`.
- `empty_piece_keeps_its_cut`: `split_plan("B", &[(1,Some(0)),(4,Some(1))], &[])`
  is `Ok(vec![(1,0..0),(4,0..1)])`.
- `missing_bytes_refuses` with control: `&[(1,None),(3,Some(1))]` →
  `Err(MissingBytes)`. Control is `splits_in_part_order`.
- `not_increasing_refuses`: `&[(3,Some(1)),(1,Some(1))]` → `Err(NotIncreasing)`.
  `&[(1,Some(1)),(1,Some(1))]` → `Err(NotIncreasing)`.
- `index_collision_refuses`: `split_plan("AB", &[(1,Some(1)),(2,Some(1))], &[2])`
  → `Err(IndexCollision)`.
- `sum_mismatch_refuses`: `split_plan("ABC", &[(1,Some(1)),(3,Some(1))], &[])` →
  `Err(SumMismatch)`.
- `overflow_refuses_without_panic`:
  `split_plan("AB", &[(1,Some(u64::MAX)),(3,Some(3))], &[])` →
  `Err(SumMismatch)`.
- `cut_inside_a_char_refuses`: `split_plan("éü", &[(1,Some(1)),(3,Some(3))], &[])`
  → `Err(NotCharBoundary)`.

- [ ] **Step 2: run, and check that they fail**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib split_plan_tests`
Expected: compile error (`split_plan` not defined).

- [ ] **Step 3: implement `split_plan`** as the Interfaces block says. Add
  `#[allow(dead_code)]` only until Task 3 uses it, or let Task 3 remove it.

- [ ] **Step 4: run, and check that they pass**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib split_plan_tests`
Expected: PASS (10 tests).

- [ ] **Step 5: commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-anthropic/src/translate/request.rs
git commit -m "fix(providers): SMA-854 add the anthropic text split plan" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: the replay sends each text piece at its own position

**Files:**

- Modify: `A/src/translate/request.rs`: `WireBlock`, `Slot::is_kept_by`,
  `WireMessage::into_value`, `assistant_blocks`, the `is_blank` comment, the
  module doc (lines 7-12), the comments at about lines 701 and 729.
- Modify: `A/src/translate/mod.rs` tests: invert
  `two_text_blocks_join_at_first_text_position`, extend
  `session_projection_round_trip`.
- Test: `A/src/translate/request.rs` module `thinking_round_trip_tests`.

**Interfaces:**

- Consumes: `split_plan`, `SplitRefusal`, `Cut` (Task 2);
  `AnthropicData::Text { index, bytes }` (Task 1).
- Produces: `WireBlock` gets the field `pieces: Vec<(SortKey, String)>`
  (`Default` gives an empty list). `WireBlock::plain` sets it empty.

**Rules** (from the spec, "Replay" and "When a message is splittable"):

1. In `assistant_blocks`, collect every `Text` position of the message in part
   order as `(index, bytes)`, and the `Block` and `ToolUse` indexes as
   `other_indexes`. The key of the joined text stays as today: the first `Text`
   position, or `BeforeFirstToolUse`.
2. Find the text parts. If there is exactly one `ContentPart::Text` with
   non-empty text, a source, and two or more `Text` positions, call
   `split_plan`. On `Err(reason)`, log
   `tracing::debug!(target: "paigasus::anthropic::translate", ?reason, positions
   = n, text_bytes = len, "anthropic text does not split; one joined block")`
   and build as today. With more than one non-empty text part and two or more
   positions, log the same line with the reason `"several text parts"` as a
   string field, and build as today.
3. On `Ok(cuts)`, make the pieces: for each cut with a non-empty range,
   `(SortKey::At(u64::from(index), 1), text[range].to_owned())`. Skip empty
   ranges (the API rejects an empty text block).
4. Non-blank joined text: its text `WireBlock` is built as today, and gets
   `pieces`.
5. Blank (whitespace-only) joined text: no text block, as today. The first
   thinking `WireBlock` of the item (the first `AnthropicData::Block` part)
   gets `pieces`. If the item has no thinking block, the pieces are discarded.
6. `Slot::is_kept_by` is unchanged. The pieces are not blocks yet, so they count
   nowhere.
7. In `WireMessage::into_value`, between `drop_thinking_without_own_block` and
   `sort_groups`, expand. For each block with non-empty `pieces` and
   `slot: Some(slot)`: each piece becomes
   `WireBlock { value: json!({"type":"text","text":piece}), slot:
   Some(Slot { key, ..slot }), pieces: vec![] }`. A text carrier is replaced by
   its pieces. A thinking carrier stays, and its pieces go right after it. A
   block with `slot: None` drops its `pieces` and stays as it is.
8. `into_output` is unchanged. It clears the slots of the last assistant
   message and removes its thinking blocks before `into_value`, so that message
   never expands.
9. `is_blank` comment: "The API rejects an empty text block (HTTP 400). The
   SMA-854 probe showed that it accepts a whitespace-only text block in an
   assistant message that is not the last one. This filter still drops both,
   except for the split pieces of `assistant_blocks`."
10. Module doc and the two comments: say "each text block", not "the first
    text block". Point to SMA-854 for the split.

- [ ] **Step 1: write the failing tests** (in `thinking_round_trip_tests`; use
  `text_pos_bytes`, `thinking`, `tool_pos`, `asst`, `user`, `result`,
  `messages`, `without_anthropic`, `amsg`, `umsg`, `wthinking`, `wtext`, `tu`,
  `tr`, `utext`)

1. `split_replay_restores_the_issue_example`: items `[user("q"), asst([text("AB"),
   thinking(0,"",SIG_FAKE_1), text_pos_bytes(1,1), thinking(2,"",SIG_FAKE_2),
   text_pos_bytes(3,1), use_part("tu_1"), tool_pos(4,"tu_1")]), result("tu_1"),
   user("next")]`. The assistant message is
   `[wthinking("",SIG_FAKE_1), wtext("A"), wthinking("",SIG_FAKE_2), wtext("B"),
   tu("tu_1")]`. Red today: today gives `"AB"` before `SIG_FAKE_2`.
2. `text_before_first_thinking_splits`: positions `Text(0,1)`, `Block(1)`,
   `Text(2,1)`, `ToolUse(3)`, text `"AB"` → `[wtext("A"), wthinking(…1),
   wtext("B"), tu]`.
3. `empty_first_text_block_moves_text_to_its_own_position`: text `"B"`,
   `[Block(0) SIG_FAKE_1, Text(1, bytes 0), ToolUse(2) tu_x, Block(3)
   SIG_FAKE_2, Text(4, bytes 1), ToolUse(5) tu_y]` with both calls →
   `[th1, tu_x, th2, wtext("B"), tu_y]`. Red today (today `"B"` sits at index 1).
4. `multi_byte_pieces_split`: text `"éü"`, `Text(1,2)`, `Block(2)`, `Text(3,2)`,
   `ToolUse(4)` → `[wtext("é"), th, wtext("ü"), tu]`.
5. `whitespace_piece_is_sent`: text `"\n\nB"`, `Block(0)`, `Text(1,2)`,
   `Block(2)`, `Text(3,1)`, `ToolUse(4)` → `[th1, wtext("\n\n"), th2, wtext("B"),
   tu]`.
6. Fallback with control, each next to its control in the same test:
   `missing_bytes_falls_back`, `sum_mismatch_falls_back`,
   `cut_inside_char_falls_back`, `not_increasing_falls_back`,
   `index_collision_falls_back`, `huge_bytes_falls_back_without_panic`
   (`u64::MAX`), `two_text_parts_fall_back` (two `text(...)` parts). Each
   asserts that the bad data gives the same messages as today's form (all text
   joined at the first position, which is `messages()` of the same items with
   `bytes: None` positions), **and** that the same items with correct `bytes`
   split.
7. `whitespace_joined_text_expands_when_thinking_is_kept`: text `" \n"`,
   `[Block(0), Text(1,1), Block(2), Text(3,1), ToolUse(4) tu_1]` with the call →
   `[th1, wtext(" "), th2, wtext("\n"), tu]`.
8. `whitespace_joined_text_does_not_change_the_item_decisions`: the same item
   with its calls lost (a later plain `user` turn, no `result`) builds equal to
   `without_anthropic` (no text block, no thinking). And an item
   `asst([text("  "), thinking(0,…), text_pos_bytes(1,1),
   thinking(2,…), text_pos_bytes(3,1)])` as the last item builds equal to
   `without_anthropic`.
9. `split_data_hand_off_builds_as_without_anthropic`: the shape of the existing
   hand-off test (about `request.rs:2760`) but with two text positions with
   `bytes` and a thinking block between them. `out.messages ==
   base.messages` and `out.trailing_message_dropped ==
   base.trailing_message_dropped`.
10. `split_data_lost_calls_builds_as_without_anthropic`: the shape of
    `trimmed_history_that_lost_all_calls_drops_thinking` with split data.
    Assert equality with `without_anthropic`, and that the assistant message
    holds one joined text block.
11. `split_data_last_assistant_message_builds_as_without_anthropic`: the split
    item is the last item. Assert equality with `without_anthropic`.

In `translate/mod.rs`:

12. Rename `two_text_blocks_join_at_first_text_position` to
    `two_text_blocks_go_back_at_their_own_positions`. Expected shape:
    `[thinking SIG_FAKE_1, text "A", tool_use tu_x, thinking SIG_FAKE_2, text
    "B", tool_use tu_y]`. Add `structured_output_text_falls_back`: a
    `MessageTranslator::new(true)` stream with `[thinking(0), text(1) "A",
    thinking(2), tool_use(3) named SYNTHESIZED_TOOL_NAME with input_json_delta
    "{}"]` and a second text block, built through `items_of` and `turn2_body`.
    The assistant content holds one joined text block (no split), because the
    synthesized JSON makes the sum short.
13. `session_projection_round_trip`: add a third case, built from the event
    list of test 12 (`two_text_blocks_go_back_at_their_own_positions`), with the
    same assertions. So a session keeps `bytes` and the projected body splits.

- [ ] **Step 2: run, and check which fail**

Run: `cargo test -p paigasus-helikon-providers-anthropic`
Expected: tests 1, 2, 3, 4, 5, 7, 12 (renamed) and 13 FAIL. Tests 6 (the bad-data
halves), 8, 9, 10, 11 and `structured_output_text_falls_back` can pass on
today's code. Their job is to stay green after Step 3. Record which ones failed
in the report.

- [ ] **Step 3: implement** the rules above.

- [ ] **Step 4: run, and check that all pass**

Run: `cargo test -p paigasus-helikon-providers-anthropic`
Expected: PASS, with no other test changed.

- [ ] **Step 5: mutation check of the guards**

Disable each guard in `split_plan` in turn (return `Ok` early for that
check), and run `cargo test -p paigasus-helikon-providers-anthropic --lib`.
Each run must fail at least one test of Task 2 or Task 3. Then disable step 7's
`slot: Some` condition (expand also with `slot: None`): tests 9, 10 and 11 must
fail. Revert each mutation. Run `git diff --stat` to check that only the
intended files changed. Report the result of each mutation.

- [ ] **Step 6: commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-anthropic/src/translate/request.rs crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs
git commit -m "fix(providers): SMA-854 send each anthropic text block at its own position" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: the live test of the API facts

**Files:**

- Modify: `A/src/live_round_trip.rs` (one new ignored test, and a line in the
  module doc)

**Interfaces:**

- Consumes: `make_model`, `first_request`, `BASE_PROMPT`, `run_turn`,
  `second_request`, `tool_calls`, `signatures`, `blocks_of_items` (existing),
  `build_body` (existing), `crate::http::{build_headers, messages_url}`,
  `model.cfg.http` (`reqwest::Client`).

- [ ] **Step 1: write `live_text_block_api_facts`** (`#[tokio::test]`,
  `#[ignore]`, returns early when `!has_key()`)

1. Turn 1 with `make_model(None)` and `first_request(BASE_PROMPT)`. Assert a
   tool call, a signature, and a non-empty text in the `AssistantMessage`.
2. `let mut body = build_body(&model.cfg, &second_request(&first,
   &turn1.items)).unwrap().body;` then `body["stream"] = json!(false)`.
3. Find the first assistant message. Make four bodies from it:
   - `exact`: unchanged.
   - `text_before_thinking`: insert `{"type":"text","text":"Noted."}` at content
     index 0.
   - `whitespace_text`: insert `{"type":"text","text":"\n\n"}` right after the
     first text block.
   - `split_text`: replace the first text block with two text blocks, cut at the
     largest `char` boundary at or below half its byte length (skip the variant
     with a message when the text has one `char`).
4. Send each with `model.cfg.http.post(messages_url(&model.cfg))
   .headers(build_headers(&model.cfg)).json(&body)`. Assert HTTP 200. Count the
   entries of `input_transformations` with `"type": "thinking_dropped"`.
5. Assert `text_before_thinking` count ≥ 1, with the message "the prefix check
   is not active; the other results mean nothing". Assert 0 for the other three,
   with the variant name in the message. No message holds text or a signature.

Add to the module doc: "`live_text_block_api_facts` checks the API facts of the
SMA-854 design: a text block before a thinking block drops it, and a
whitespace-only or split text block after it does not."

- [ ] **Step 2: compile**

Run: `cargo test -p paigasus-helikon-providers-anthropic --no-run`
Expected: builds. Then `cargo test -p paigasus-helikon-providers-anthropic
live_text_block_api_facts` gives `1 ignored`.

The live run needs the key. Do not try to read `.env`. The coordinator asks
Sven to run:
`! cd <worktree> && set -a && . /Users/smaschek/dev/paigasus/paigasus-helikon/.env && set +a && cargo test -p paigasus-helikon-providers-anthropic -- --ignored live_text_block_api_facts live_thinking_round_trip`

- [ ] **Step 3: commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-anthropic/src/live_round_trip.rs
git commit -m "test(providers): SMA-854 add a live test of the text block api facts" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: doc corrections

**Files:**

- Modify: `crates/paigasus-helikon-core/src/model.rs:573-575` (the `ModelTurn::items` doc)
- Modify: `docs/superpowers/specs/2026-11-21-sma-840-anthropic-thinking-signatures-design.md`
  (the non-goal "Exact text order …", about line 118)

- [ ] **Step 1: core doc.** Change "(text, reasoning and/or provider data
  content, in that order)" to "(reasoning, text and/or provider data content, in
  that order)". This matches `build_items`.

- [ ] **Step 2: commit the core doc alone**

```bash
cargo fmt --all
git add crates/paigasus-helikon-core/src/model.rs
git commit -m "docs(core): SMA-854 correct the part order in the model turn doc" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 3: SMA-840 spec note.** Under that non-goal, add:
  "> **Correction (SMA-854, 2026-11-22):** this is wrong for blocks before a
  thinking block. A changed block before a thinking block in the same message
  makes the API drop that block (`prefix_binding_mismatch`). SMA-854 sends each
  text block at its own position. See
  `2026-11-22-anthropic-per-text-block-positions-design.md`."

- [ ] **Step 4: lint and commit**

Run: `npx --no-install markdownlint-cli2` → `0 issues`.

```bash
git add docs/superpowers/specs/2026-11-21-sma-840-anthropic-thinking-signatures-design.md docs/superpowers/plans/2026-11-22-anthropic-per-text-block-positions.md
git commit -m "docs(specs): SMA-854 correct the sma-840 order non-goal" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: all CI gates

- [ ] **Step 1: run every gate from `CLAUDE.md`**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
npx --no-install markdownlint-cli2
bash scripts/check-markdownlint-config.sh
bash scripts/check-cargo-profile-env-sync.sh
bash scripts/check-breaking-cascade.sh
```

Expected: each exits 0. Report the exit code of each command directly, not
through a pipe.

- [ ] **Step 2: fix and commit** any finding in the task that owns the code,
  with a `fix(providers)` or `docs(core)` commit as fits.
