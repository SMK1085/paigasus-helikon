# SMA-840 Anthropic thinking round trip Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Anthropic provider keeps each `thinking` and `redacted_thinking` block of a response
in the SMA-827 provider-data carrier, and sends the blocks back unchanged, in their original order,
in each later request.

**Architecture:** The stream translator emits one `ModelEvent::ProviderData { provider: "anthropic" }`
at the stop of each thinking block, and position records for the first `text` block and each
`tool_use` block. Core does not change. The request translator collects the parts of each source
message, rebuilds the blocks from known fields, and sorts the blocks of each source message by
their response `index` in one pass at the end of `translate_messages`.

**Tech Stack:** Rust 2021 workspace (MSRV 1.94), serde / serde_json, insta, wiremock, tokio.

**Spec:** `docs/superpowers/specs/2026-11-21-sma-840-anthropic-thinking-signatures-design.md`
(binding; it wins over this plan).

## Global Constraints

- No change to `crates/paigasus-helikon-core`. No new dependency in any `Cargo.toml`.
- No hand version bump and no hand CHANGELOG edit. release-plz does the bump.
- Carrier key: `pub const PROVIDER_DATA_KEY: &str = "anthropic";` in
  `crates/paigasus-helikon-providers-anthropic/src/lib.rs`. It is separate from `PROVIDER_ID`.
- `data` shapes, exactly:
  `{"kind":"block","index":n,"block":{"type":"thinking","thinking":"…","signature":"…"}}`,
  `{"kind":"block","index":n,"block":{"type":"redacted_thinking","data":"…"}}` (both `call_id: None`),
  `{"kind":"text","index":n}` (`call_id: None`, first text block only),
  `{"kind":"tool_use","index":n}` (`call_id: Some(<tool_use id>)`).
- A response with no thinking block that is sent back writes no provider data (Goal 4).
- A history with no Anthropic provider data gives the same request body as `main` (Goal 5). The
  Task 1 `.snap` files must not change in any later task.
- Another provider's `ProviderData` part is ignored with no log and never fails a request.
- A log line holds only the index and the block type. It never holds thinking text, a signature,
  redacted data or a tool argument.
- Do not edit an existing test assertion, except where a task names the test. A test that builds
  `ContentBlockHead::Thinking` as a unit variant gets the new fields only.
- Fixtures hold no real signature and no real redacted data. Use `SIG_FAKE_1`, `SIG_FAKE_2`, …
  and `REDACTED_FAKE_1`, `REDACTED_FAKE_2`, …. The probe captures are under
  `/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/ebc68a6c-86b8-4a5f-80af-4e374bf8b2aa/scratchpad/`.
  Never copy a value from a capture into the repo, a commit message or a PR body.
- SSE fixtures stay LF. The existing `.gitattributes` rule
  `crates/paigasus-helikon-providers-anthropic/tests/fixtures/*.txt text eol=lf` covers every new
  fixture. Confirm with `git check-attr eol -- <file>` (expected: `eol: lf`).
- Commit format `<type>(<scope>): SMA-840 <lowercase subject>`. Crate code: `fix(providers)`.
  Tests only, crate or conformance suite: `test(providers)`. Docs: `docs(docs)`. These scopes are
  in the `.versionrc` `scopeRegex` (`providers-anthropic` is also allowed, but the spec names
  `providers`). End every commit message with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Commits are signed through 1Password. Never bypass signing. If a commit fails with
  "failed to fill whole buffer", stop and ask Sven to unlock the vault.
- Stage files by explicit path. Never `git add -A` or `git add .` (`.env` is not ignored). After
  each commit, run `git show --stat HEAD` and check the file list.
- Work only in `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-840`. Start
  every shell command with `cd /Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-840 || exit 1`.
  Use worktree-absolute paths for every file write. Do not run a git command that moves HEAD or a
  branch (no checkout, switch, reset, rebase, stash, merge, pull).
- Gates before each commit: `cargo fmt --all`,
  `cargo clippy --workspace --all-features --all-targets -- -D warnings`,
  `cargo test -p paigasus-helikon-providers-anthropic --all-features`. Task 6 also runs
  `cargo test -p paigasus-helikon-provider-stream-conformance`. Task 8 also runs
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`,
  `npx markdownlint-cli2` and `mdbook build docs/book`.
- Work in the foreground. Do not put a cargo command in the background. Do not end a task until
  each command shows its final status.
- A new `pub` item gets a `///` doc comment. A `pub` doc comment does not intra-doc-link a private
  item.
- All rustdoc, README and book text is in ASD-STE100 Simplified Technical English: approved words,
  short sentences (20 words for an instruction, 25 for a description), active voice, no idiom.
- Live tests are `#[ignore]` and need `ANTHROPIC_API_KEY`. Implementers do not run them. Sven runs
  them.

## Review Focus

1. **A signature that arrives in more than one `signature_delta`, or thinking text with `\n` and
   `"`.** One changed byte gives HTTP 400 on every later request of the session. The stored values
   must be the exact concatenation (Task 2 test `signature_from_split_deltas_is_joined`).
2. **A session that starts on an old SDK and continues on this one.** The old turns have no
   Anthropic parts and must build as on `main`; the new turns must be ordered (Task 3 test
   `mixed_old_and_new_turns`).
3. **A response with ten or more blocks** (run C produced 15 redacted blocks). Index 10 must sort
   after index 2, not before it (Task 3 test `two_digit_indexes_sort_numerically`).
4. **Two `text` blocks in one response** (`thinking, text A, tool_use, thinking, text B, tool_use`).
   The request must hold one text block with `AB` at the first text position, with nothing lost
   and nothing doubled (Task 5 test `two_text_blocks_join_at_first_text_position`).
5. **A stream that ends after a thinking block but before the `tool_use` block stops.** The stream
   keeps the complete block and writes no position for the open `tool_use` block (Task 2 test
   `eof_after_thinking_block_keeps_only_the_block`).

---

### Task 1: Pin the request bodies of histories without Anthropic provider data

This task changes no production code. It records the `main` request bodies, so that later tasks
can prove Goal 5 against the real baseline and not only against the spec.

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs` (new
  `#[cfg(test)] mod baseline` after `mod tests`)
- Create: `crates/paigasus-helikon-providers-anthropic/src/translate/snapshots/*.snap` (written by
  insta)

**Interfaces:**
- Produces: `fn baseline_body(cfg: &Config, items: Vec<Item>) -> serde_json::Value` (test only),
  and one snapshot for each case below. Later tasks run these tests unchanged.

- [ ] **Step 1: Write the snapshot tests.** One `#[test]` per case, each calls
  `insta::assert_json_snapshot!(<case name>, baseline_body(..))`. Use the default config
  (`AnthropicModelBuilder::new("claude-sonnet-4-6").api_key("sk-test").build_config()`) unless the
  case names another:
  - `baseline_text_turns`: user `q`, assistant text `a`, user `r`.
  - `baseline_accumulator_tool_loop`: user `q`, `AssistantMessage[Reasoning "r", Text "t"]`,
    `ToolCall tu_1`, `ToolResult tu_1`, assistant text `done`.
  - `baseline_nested_open_round`: the items of the existing request test
    `mixed_assistant_inside_open_round_matches_top_level_form`.
  - `baseline_handoff_transcript`: user `q`, `AssistantMessage[Reasoning "r"]`, user
    `You are now handling a transferred conversation. Continue assisting the user.`
  - `baseline_transfer_split`: user `q`, `AssistantMessage[Text "t"]`, `ToolCall a`,
    `ToolResult a`, `ToolCall b`, `ToolResult b`.
  - `baseline_foreign_provider_data`: the items of `provider_data_dropped_without_warning` (Gemini
    parts).
  - `baseline_last_turn_cache`: `baseline_accumulator_tool_loop` with
    `.cache_strategy(CacheStrategy::LastTurn)`.
  - `baseline_thinking_enabled`: `baseline_accumulator_tool_loop` with
    `.extended_thinking(ExtendedThinking::Enabled { budget_tokens: 2048 })`.
- [ ] **Step 2: Run** `INSTA_UPDATE=always cargo test -p paigasus-helikon-providers-anthropic --all-features baseline`
  — expected: PASS, and eight new `.snap` files. Read each file. Each body must agree with the
  rules in `translate/request.rs` today (for example, no `thinking` block anywhere, `Reasoning`
  text absent).
- [ ] **Step 3: Run** the gates. Expected: clean.
- [ ] **Step 4: Commit** the test module and the `.snap` files:
  `test(providers): SMA-840 pin request bodies of histories without anthropic thinking data`

### Task 2: The stream keeps thinking blocks

**Files:**
- Create: `crates/paigasus-helikon-providers-anthropic/src/provider_data.rs`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/lib.rs` (`mod provider_data;`, the
  const)
- Modify: `crates/paigasus-helikon-providers-anthropic/src/sse.rs` (`ContentBlockHead`,
  `ContentBlockDelta::SignatureDelta`, `AnthropicEvent::ContentBlockStop`)
- Modify: `crates/paigasus-helikon-providers-anthropic/src/stream.rs` (`BlockState`,
  `MessageTranslator`, `consume`)
- Create fixtures in `crates/paigasus-helikon-providers-anthropic/tests/fixtures/`:
  `thinking_text_tool_use.txt`, `redacted_thinking_tool_use.txt`, `empty_thinking_tool_use.txt`,
  `interleaved_thinking.txt`, `unsigned_thinking_tool_use.txt`
- Test: unit tests in `sse.rs`, `stream.rs`, `provider_data.rs`;
  `crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs`

**Interfaces:**
- Produces in `lib.rs`: `pub const PROVIDER_DATA_KEY: &str = "anthropic";`. Rustdoc: it is the
  key of the `ProviderData` parts and events that this crate writes; a host may filter or remove
  parts by this key; the `data` shape is private to the crate and can change; it is separate from
  `PROVIDER_ID`.
- Produces in `provider_data.rs`:
  - `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(tag = "type", rename_all = "snake_case")] pub(crate) enum ThinkingBlock { Thinking { thinking: String, signature: String }, RedactedThinking { data: String } }`
  - `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(tag = "kind", rename_all = "snake_case")] pub(crate) enum AnthropicData { Block { index: u32, block: ThinkingBlock }, Text { index: u32 }, ToolUse { index: u32 } }`
  - `impl AnthropicData { pub(crate) fn into_event(self, call_id: Option<String>) -> ModelEvent }`
    — gives `ModelEvent::ProviderData { provider: PROVIDER_DATA_KEY.to_owned(), call_id, data: serde_json::to_value(self) }`.
  - `impl ThinkingBlock { pub(crate) fn is_valid(&self) -> bool }` — a non-empty `signature`
    (an empty `thinking` is valid), or a non-empty `data`. The stop rule uses it.
- Produces in `sse.rs`: `ContentBlockHead::Thinking { #[serde(default)] thinking: String, #[serde(default)] signature: String }`,
  `ContentBlockHead::RedactedThinking { data: String }` (`#[serde(rename = "redacted_thinking")]`).
  The `#[allow(dead_code)]` on `SignatureDelta::signature` and on `ContentBlockStop::index` goes.

- [ ] **Step 1: Write the fixtures.** Each starts with an SSE comment line (`: …`), which the SSE
  parser ignores. Replace every signature with `SIG_FAKE_<n>` and every redacted `data` with
  `REDACTED_FAKE_<n>`. Remove trailing spaces inside `data:` lines. Keep every other byte of a
  transcribed event.
  - `thinking_text_tool_use.txt`: from `probe-840-c-claude-sonnet-4-5/1-turn1.sse` (all events).
    Comment: `: transcribed from the SMA-840 probe run C (claude-sonnet-4-5); signature replaced`.
  - `redacted_thinking_tool_use.txt`: from `probe-840-c-claude-sonnet-4-5/16-redacted.sse`. Keep
    `message_start`, the redacted blocks with index 0 and 1, the `text` block and the `tool_use`
    block, `message_delta`, `message_stop`. Remove the redacted blocks with index 2 to 13. Change
    the text index 14 to 2 and the `tool_use` index 15 to 3. Say this in the comment line.
  - `empty_thinking_tool_use.txt`: from `probe-840-a/1-turn1.sse` (claude-sonnet-5-5, the thinking
    text is empty, one `thinking_delta` with `""`).
  - `interleaved_thinking.txt` (synthetic; comment `: synthetic, no capture has interleaved
    thinking`): the run C envelope, then thinking index 0 (`"Need the code."`, `SIG_FAKE_1`),
    `tool_use` index 1 id `toolu_fake_b` name `lookup_city_code` input `{"city":"Paris"}`,
    thinking index 2 (`"Now the weather."`, `SIG_FAKE_2`), `tool_use` index 3 id `toolu_fake_a`
    name `weather_by_code` input `{"code":"PAR-7351"}`, stop reason `tool_use`. The ids sort in
    the other order than the stream order on purpose.
  - `unsigned_thinking_tool_use.txt` (synthetic): a thinking block with two `thinking_delta`
    events and no `signature_delta`, then one `tool_use` block, stop reason `tool_use`.
- [ ] **Step 2: Check the fixtures.** Run
  `grep -nE '[A-Za-z0-9+/=_-]{60,}' crates/paigasus-helikon-providers-anthropic/tests/fixtures/*.txt`
  — expected: no output. Run `git check-attr eol -- <each new file>` — expected: `eol: lf`.
- [ ] **Step 3: Write the failing tests.** In `stream.rs` tests add
  `fn translate_fixture(raw: &str, synthesizing: bool) -> Vec<ModelEvent>` (parse each
  `data: ` line as `AnthropicEvent`, feed `MessageTranslator::consume`, then `finish()`), and
  `fn provider_data(events: &[ModelEvent]) -> Vec<(AnthropicData, Option<String>)>`. Tests:
  - `thinking_text_tool_use_emits_block_and_positions`: provider data is exactly
    `[Block{0, Thinking{<joined thinking deltas>, "SIG_FAKE_1"}} None, Text{1} None, ToolUse{2} Some(<fixture tool id>)]`;
    the `ReasoningDelta` texts still join to the same thinking text.
  - `redacted_fixture_emits_two_blocks`: `[Block{0, RedactedThinking{"REDACTED_FAKE_1"}}, Block{1, RedactedThinking{"REDACTED_FAKE_2"}}, Text{2}, ToolUse{3} Some(id)]`.
  - `empty_thinking_fixture_emits_block_with_empty_text`: the first entry is
    `Block{0, Thinking{"", "SIG_FAKE_1"}}`.
  - `interleaved_fixture_emits_each_block`: the set of entries is
    `{Block 0, ToolUse{1} Some("toolu_fake_b"), Block 2, ToolUse{3} Some("toolu_fake_a")}`.
  - `unsigned_thinking_emits_no_provider_data`: empty; `ReasoningDelta` is still there.
  - `fixtures_without_thinking_emit_no_provider_data`: empty for `text_only.txt`,
    `parallel_tool_use.txt` and both parts of `tool_use_then_continuation.txt`.
  - `thinking_then_text_emits_block_and_text_position`:
    `[Block{0, Thinking{"Let me think...", "abc123"}}, Text{1}]`.
  - `tool_use_without_input_json_delta_gets_no_position` (events in code): thinking 0 signed,
    `tool_use` 1 with no delta, stops → only `Block{0}`.
  - `repeated_content_block_stop_emits_nothing_new`: a second `ContentBlockStop { index: 0 }`
    returns `Ok(vec![])`.
  - `synthesized_tool_gets_no_position`: `MessageTranslator::new(true)`, thinking 0 signed,
    `tool_use` 1 named `SYNTHESIZED_TOOL_NAME` with a delta, stops → only `Block{0}`.
  - `positions_wait_for_first_block`: text 0 stops before thinking 1 stops → the `Text{0}` record
    comes in the same `consume` result as `Block{1}`, after it; nothing before.
  - `signature_from_split_deltas_is_joined` (Review Focus 1): two `signature_delta` events
    `"SIG_"` and `"FAKE_9"`, thinking deltas `"a\n"` and `"\"b\""` → `Thinking{"a\n\"b\"", "SIG_FAKE_9"}`.
  - `eof_after_thinking_block_keeps_only_the_block` (Review Focus 5): thinking 0 signed and
    stopped, `tool_use` 1 started with one delta, no stop, `finish()` → provider data is only
    `Block{0}`; the `ToolCallDelta` is there.
  - In `sse.rs`: `deserializes_redacted_thinking_head` and
    `thinking_head_without_signature_defaults_to_empty`.
  - In `provider_data.rs`: `data_serializes_to_the_documented_shapes` — `serde_json::to_value` of
    each of the four kinds equals the JSON in Global Constraints;
    `provider_data_key_is_anthropic`.
  - In `messages_streaming.rs`: `redacted_thinking_stream_keeps_blocks` and
    `thinking_text_tool_use_stream_emits_provider_data` through `run_stream` (the wiremock path,
    which today drops `redacted_thinking` as unparseable): count the `ProviderData` events with
    `provider == PROVIDER_DATA_KEY` (4 and 3), and call `assert_exactly_one_terminal_finish`.
- [ ] **Step 4: Run** `cargo test -p paigasus-helikon-providers-anthropic --all-features` —
  expected: FAIL (types and variants are missing).
- [ ] **Step 5: Implement.** `BlockState::Thinking { text: String, signature: String }` starts
  from the head values; `thinking_delta` appends to `text` and still emits `ReasoningDelta`;
  `signature_delta` appends to `signature` (replace the "signature_delta dropped" debug line).
  `BlockState::RedactedThinking { data: String }`. New `MessageTranslator` fields:
  `text_position_recorded: bool`, `block_emitted: bool`, `waiting_positions: Vec<ModelEvent>`.
  On `ContentBlockStop { index }`, remove the state from `blocks`. No state: a debug log with the
  index only, emit nothing. Then apply the spec rules (section "Stream"): a thinking block with an
  empty signature and a redacted block with empty data are not kept (debug log with index and
  type); a `tool_use` block gets a position only when `name_emitted` is true and
  `Some(index) != synthesized_tool_index`. A position waits in `waiting_positions` until the first
  `Block` is emitted; then emit the `Block`, then all waiting records, and later positions at
  their stop. A delta for an index whose state is gone keeps today's behavior for that delta type.
- [ ] **Step 6: Run** the gates — expected: PASS. Every existing test in `stream.rs` and
  `messages_streaming.rs` passes with no assertion change.
- [ ] **Step 7: Commit** the new and changed files by path:
  `fix(providers): SMA-840 keep anthropic thinking blocks in the stream`

### Task 3: The request translator sends the blocks back

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/provider_data.rs` (reader side)
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs`
  (`translate_messages`, `assistant_blocks`, the `only_tool_uses` check at lines 105-106, the
  `Reasoning` warn at lines 262-268)
- Test: a new `#[cfg(test)] mod thinking_round_trip_tests` in `request.rs`

**Interfaces:**
- Consumes: Task 2 `AnthropicData`, `ThinkingBlock` (with `is_valid`), `PROVIDER_DATA_KEY`.
- Produces in `provider_data.rs`:
  - `impl ThinkingBlock { pub(crate) fn to_wire(&self) -> serde_json::Value }` — only `type` and
    the known fields.
  - `#[cfg(test)] pub(crate) fn part(data: AnthropicData, call_id: Option<&str>) -> ContentPart`
    — test helper that Task 5 also uses.
  - `pub(crate) fn from_part(part: &ContentPart) -> Option<(AnthropicData, Option<&str>)>` — `None`
    with no log for a part that is not `ProviderData` or has another provider; `None` with a debug
    log (no content) for a part with this key that does not parse or holds a block that fails
    `is_valid`.
- Produces in `request.rs`: `translate_messages` keeps its signature and `TranslatedMessages`.

- [ ] **Step 1: Write the failing tests.** Helpers: `thinking(i, text, sig)`, `redacted(i, data)`,
  `text_pos(i)`, `tool_pos(i, id)` (all through `part`), and the wire forms
  `wthinking(text, sig)`, `wredacted(data)`. Each test asserts the whole `messages` JSON:
  - `accumulator_form_restores_wire_order`: user `q`,
    `Asst[Reasoning "r", Text "t", thinking(0,"r","SIG_FAKE_1"), text_pos(1), tool_pos(2,"tu_1")]`,
    `ToolCall tu_1`, `ToolResult tu_1` → assistant content
    `[wthinking("r","SIG_FAKE_1"), text "t", tu("tu_1")]`.
  - `redacted_blocks_go_back_in_order`: `redacted(0,"REDACTED_FAKE_1")`, `redacted(1,"REDACTED_FAKE_2")`,
    `text_pos(2)`, `tool_pos(3,"tu_1")` → `[wredacted 1, wredacted 2, text, tu_1]`.
  - `interleaved_thinking_keeps_order`: `thinking(0,"A","SIG_FAKE_1")`, `tool_pos(1,"tu_z")`,
    `thinking(2,"B","SIG_FAKE_2")`, `tool_pos(3,"tu_a")`, then `ToolCall tu_a`, `ToolCall tu_z`
    (the accumulator order) → `[thinking A, tu_z, thinking B, tu_a]`.
  - `text_without_position_goes_before_first_tool_use`: blocks 0 and `tool_pos(2,..)`, a `Text`
    part, no `text_pos` → `[thinking, text, tu]`.
  - `text_without_position_and_no_tool_use_goes_last`: `Asst[Text "t", thinking(5,..)]` →
    `[thinking, text]`.
  - `tool_use_without_position_goes_after_keyed_blocks`: `tool_pos(1,"tu_1")` only, calls
    `tu_1` and `tu_2` → `[thinking 0, tu_1, tu_2]`.
  - `repeated_tool_use_id_takes_positions_in_order`: nested
    `Asst[thinking(2,..), tool_pos(1,"dup"), tool_pos(3,"dup"), ToolUse dup {"n":1}, ToolUse dup {"n":2}]`
    → `[dup n=1, thinking, dup n=2]`.
  - `orphan_position_part_is_ignored`: `tool_pos(1,"gone")` and no call → `[thinking, text]`, no
    error.
  - `two_digit_indexes_sort_numerically` (Review Focus 3): `redacted(10,..)`, `redacted(2,..)`,
    `text_pos(11)` → `[redacted 2, redacted 10, text]`.
  - `mixed_old_and_new_turns` (Review Focus 2): turn 1 in the `baseline_accumulator_tool_loop`
    shape with no Anthropic part, then user `q2`, then turn 2 in the
    `accumulator_form_restores_wire_order` shape → turn 1 messages equal that baseline's messages;
    turn 2 is ordered.
  - `earlier_turn_blocks_are_sent_back`: user `q1`,
    `Asst[Text "a1", thinking(0,..), text_pos(1)]`, user `q2` → the assistant message is
    `[thinking, text "a1"]`.
  - `foreign_provider_data_is_ignored`: the previous test plus Gemini parts gives the same JSON.
  - `malformed_part_is_skipped`: data `{"kind":"nope"}`, `{"kind":"block","index":0}`, and a
    thinking block whose `signature` is a number → the same JSON as with no Anthropic part.
  - `failed_block_check_is_skipped`: an empty `signature`, an empty `data` → not sent.
  - `block_is_rebuilt_from_known_fields`: a block with an extra `"cache_control"` key → the wire
    block has only `type`, `thinking`, `signature`.
  - `sma834_join_with_thinking_both_forms`: the nested form
    `[user q, Asst[thinking(0,"T1","SIG_FAKE_1"), tool_pos(1,"t1"), tool_pos(2,"t2"), ToolUse t1, ToolUse t2], result t1, Asst[thinking(0,"T2","SIG_FAKE_2"), tool_pos(1,"t3"), ToolUse t3], result t2, result t3]`
    and the accumulator form (the second message is `Asst[Reasoning "T2", thinking(0,..), tool_pos(1,"t3")]`
    then `ToolCall t3`) both give
    `[utext q, asst [thinking T1, tu t1, tu t2, thinking T2, tu t3], user [tr t1, tr t2, tr t3]]`.
  - `handoff_transcript_thinking_only_message_is_dropped`: the `baseline_handoff_transcript` items
    with `thinking(0,..)` and `tool_pos(1,"tu_1")` added to the assistant message → the same
    messages as that baseline.
  - `reasoning_only_assistant_is_not_sent_with_anthropic_blocks` and
    `trailing_blank_user_then_reasoning_assistant_sets_flag_with_anthropic_blocks`: the existing
    tests with `thinking(0,..)` added; the same expected values, including the flag.
  - `length_finish_after_thinking_is_dropped`: user `q`, `Asst[Reasoning "r", thinking(0,..)]` →
    `[utext q]`, flag `false`.
  - `transfer_split_moves_tool_use_unkeyed`: user `q`,
    `Asst[Reasoning, thinking(0,..), tool_pos(1,"a"), tool_pos(2,"b")]`, `ToolCall a`,
    `ToolResult a`, `ToolCall b`, `ToolResult b` →
    `[utext q, asst [thinking, tu a], user [tr a], asst [tu b], user [tr b]]`.
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-providers-anthropic --all-features thinking_round_trip_tests`
  — expected: FAIL.
- [ ] **Step 3: Implement** per spec section "Request translator", with these decisions:
  - Inside `translate_messages`, replace the `Vec<Value>` message list and `pending_tool_use` with
    private structs: `WireBlock { value: Value, slot: Option<Slot> }`,
    `Slot { group: usize, key: (u64, u8) }`, `WireMessage { role: &'static str, content: Vec<WireBlock> }`.
    Convert to `Value` at the end. No sort key reaches the JSON.
  - A **group** is one source message. It is sortable only when `from_part` accepts at least one
    of its parts. A non-sortable group gives no slots, so it builds as on `main`.
  - Keys: a `Block` part `(index, 1)`; the text block with a `Text` position `(index, 1)`; the text
    block with no position `(k, 0)`, where `k` is the smallest keyed `tool_use` index of the
    group, else `u64::MAX`; a `tool_use` with a position `(index, 1)` (take positions with the
    same `call_id` in order); a `tool_use` with no position `(u64::MAX, 1)`.
  - A `tool_use` from an `Item::ToolCall` gets a slot only when it lands in the wire message that
    holds its group's blocks. Otherwise it has no slot.
  - Final pass, before `TranslatedMessages` is built (so before `apply_cache_strategy`): in each
    message, for each group, take the positions its blocks occupy, sort those blocks by key with a
    stable sort, and write them back into the same positions. Blocks of other groups and blocks
    with no slot do not move.
  - Thinking-only rule: when an `AssistantMessage` gives only `thinking` / `redacted_thinking`
    blocks, look ahead over the items of its source message (until the next `AssistantMessage` or
    a `UserMessage` with a part that is not a `ToolResult`). With no `Item::ToolCall` in that span,
    drop the thinking blocks, so the item is empty exactly as on `main`.
  - SMA-834 join: the `only_tool_uses` check accepts `tool_use`, `thinking` and
    `redacted_thinking` blocks.
  - `ContentPart::Reasoning`: `tracing::debug!` (no text), not `warn!`.
- [ ] **Step 4: Run** the gates — expected: PASS. The Task 1 snapshots pass with no `.snap` change
  (`git status` shows no change under `translate/snapshots/`).
- [ ] **Step 5: Commit**
  `fix(providers): SMA-840 send anthropic thinking blocks back in the request`. The commit body
  says that the blocks add input tokens to each later request (about 100 for each round without
  the interleaved beta, and 250 to 300 with it, in the probe).

### Task 4: No cache breakpoint on a thinking block

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/cache.rs:53-65`
- Test: `mod tests` in the same file; `mod thinking_round_trip_tests` is not touched

**Interfaces:**
- Consumes: wire blocks with `"type": "thinking"` / `"redacted_thinking"` (Task 3).
- Produces: `apply_cache_strategy` keeps its signature.

- [ ] **Step 1: Write the failing tests:**
  - `last_turn_skips_trailing_thinking_block`: last message content `[text, thinking]` → the text
    block has `cache_control`, the thinking block has none.
  - `last_turn_skips_trailing_redacted_block`: `[tool_use, redacted_thinking]` → the `tool_use`
    block has it.
  - `last_turn_with_only_thinking_blocks_sets_no_breakpoint`: `[thinking, redacted_thinking]` → no
    block has `cache_control`.
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-providers-anthropic --all-features last_turn`
  — expected: the three new tests FAIL.
- [ ] **Step 3: Implement.** For `LastTurn`, mark the last block of the last message whose `type`
  is not `thinking` or `redacted_thinking`; if no such block, mark none.
- [ ] **Step 4: Run** the gates — expected: PASS, the Task 1 snapshots unchanged.
- [ ] **Step 5: Commit** `fix(providers): SMA-840 never put a cache breakpoint on a thinking block`

### Task 5: Offline round trip from stream to request

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs` (new
  `#[cfg(test)] mod round_trip`; one test in `mod tests`)

**Interfaces:**
- Consumes: `MessageTranslator`, `AnthropicEvent`, `AnthropicData`, `ModelTurnAccumulator::new`,
  `observe`, `finish`, `SessionRecorder`, `paigasus_helikon_core::project`, `build_body`.
  Use `crates/paigasus-helikon-providers-gemini/src/translate/mod.rs` `mod round_trip` as the
  pattern.

- [ ] **Step 1: Write the tests.** Helper
  `fn turn_items(fixture: &str) -> (Vec<Item>, Vec<ModelEvent>)` (translate, accumulate with agent
  `"root"`). Helper `fn turn2_body(items) -> Value`: user `q`, the items, one `ToolResult` per
  `ToolCall`, then `build_body`. The expected thinking text is the join of the `ReasoningDelta`
  texts, and the expected text is the join of the `TokenDelta` texts, of the same translation.
  - `thinking_text_tool_use_round_trip`: assistant content is
    `[{"type":"thinking","thinking":<join>,"signature":"SIG_FAKE_1"}, {"type":"text","text":<join>}, {"type":"tool_use","id":<id>,"name":"lookup_city_code","input":{"city":"Paris"}}]`.
  - `redacted_round_trip`: `[redacted REDACTED_FAKE_1, redacted REDACTED_FAKE_2, text, tool_use]`.
  - `empty_thinking_round_trip`: the first block is `{"type":"thinking","thinking":"","signature":"SIG_FAKE_1"}`.
  - `interleaved_round_trip`: `[thinking SIG_FAKE_1, tool_use toolu_fake_b, thinking SIG_FAKE_2, tool_use toolu_fake_a]`.
  - `unsigned_round_trip`: no `thinking` block in the body, and the items hold no `ProviderData`.
  - `no_thinking_turn_writes_no_provider_data` (Goal 4): for both parts of
    `tool_use_then_continuation.txt` and for `parallel_tool_use.txt`, no item holds a
    `ProviderData` part.
  - `two_text_blocks_join_at_first_text_position` (Review Focus 4, events in code): thinking 0
    (`SIG_FAKE_1`), text 1 `"A"`, `tool_use` 2 `tu_x`, thinking 3 (`SIG_FAKE_2`), text 4 `"B"`,
    `tool_use` 5 `tu_y` → `[thinking 1, text "AB", tu_x, thinking 2, tu_y]`.
  - `session_projection_round_trip`: write the `thinking_text_tool_use.txt` and
    `interleaved_thinking.txt` turns through `SessionRecorder` (`record_input`, `observe` of
    `MessageOutput`, `ToolCallItem`, `ToolOutputItem`), `project(&recorder.drain())`, then
    `build_body` → the same assistant content as the direct round trip.
  - In `mod tests`: `history_does_not_change_thinking_setting`: with
    `ExtendedThinking::Enabled { budget_tokens: 2048 }`, a history with thinking blocks and one
    without give `body["thinking"] == {"type":"enabled","budget_tokens":2048}`.
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-providers-anthropic --all-features round_trip`
  — expected: PASS. A failure here is a bug in Task 2 or 3: fix it there (in this task's commit)
  and say so in the task report.
- [ ] **Step 3: Run** the gates — expected: PASS.
- [ ] **Step 4: Commit** `test(providers): SMA-840 cover the anthropic thinking round trip end to end`

### Task 6: Conformance scenario with a thinking block

**Files:**
- Modify: `tests/provider-stream-conformance/src/lib.rs` (`Scenario`, `Scenario::ALL`,
  `expects_stop_reason`, `floor_violation`, the `fixture_finish_reason` trait doc,
  `scenario_all_is_exhaustive`)
- Modify: `tests/provider-stream-conformance/src/declines.rs` (`DECLINED`)
- Modify: `tests/provider-stream-conformance/src/fakes.rs` (`conforming` and the finish-reason
  arms)
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (the `anthropic` module script,
  request and provenance doc; each other subject's `script`, `fixture_finish_reason` and
  `stream` decline)

**Interfaces:**
- Produces: `Scenario::ThinkingToolCallCleanStop` — "a signed thinking block, then one complete
  tool call, then a tool-use stop reason". It expects a stop reason and `FinishReason::ToolCalls`.

- [ ] **Step 1: Write the failing test.** Add the variant to `scenario_all_is_exhaustive` and the
  pinned decline rows. Run `cargo test -p paigasus-helikon-provider-stream-conformance` —
  expected: FAIL to compile on the non-exhaustive matches.
- [ ] **Step 2: Implement.**
  - The floor for the new scenario is the `ToolCallCleanStop` floor plus one more check: at least
    one `ProviderData` event with `call_id: Some(_)`. Otherwise
    `InsufficientEvidence("no call-scoped ProviderData: assertion 8 would pass vacuously")`. Its
    `Finish` floor is the same as `ToolCallCleanStop`.
  - `fakes.rs`: the conforming fake for the new scenario also emits a `ProviderData` event with
    the fake tool call's id.
  - The `anthropic` script: transcribe the event shapes from `thinking_text_tool_use.txt` (thinking
    `content_block_start` with `"thinking":"","signature":""`, one `thinking_delta`, one
    `signature_delta` with `SIG_FAKE_1`, `content_block_stop`), then the existing
    `tool_use_start(TOOL_USE_ID, TOOL_NAME)`, `input_json_delta`, `content_block_stop`,
    `message_delta("tool_use", ..)`, `message_stop()`. Add the fixture to the module's
    "Fixture provenance" list. `request()` sends the tool definition for this scenario too.
  - The other five subjects (`gemini`, `bedrock`, `litellm`, `openai/chat`, `openai/responses`)
    return `Outcome::Declined("the scenario serves an anthropic thinking block")`, with one
    `DECLINED` row each and the same reason string.
- [ ] **Step 3: Run** `cargo test -p paigasus-helikon-provider-stream-conformance` — expected:
  PASS, and the Anthropic subject runs the new scenario (assertion 8 then checks the `tool_use`
  position event).
- [ ] **Step 4: Run** the gates — expected: PASS.
- [ ] **Step 5: Commit**
  `test(providers): SMA-840 add an anthropic thinking scenario to the stream conformance suite`

### Task 7: Live tests

**Files:**
- Create: `crates/paigasus-helikon-providers-anthropic/src/live_round_trip.rs`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/lib.rs`
  (`#[cfg(test)] mod live_round_trip;`)

**Interfaces:**
- Consumes: `AnthropicModel` (its `cfg` field), `build_body`, `AnthropicData`,
  `PROVIDER_DATA_KEY`, core `LlmAgent`, `Tool`, `RunContext`, `AgentInput`,
  `RunResultStreaming`. Use `crates/paigasus-helikon-providers-gemini/src/live_round_trip.rs` and
  `crates/paigasus-helikon-providers-gemini/tests/live.rs` (`StepTool`) as the pattern.
- Produces: env var `ANTHROPIC_LIVE_MODEL_ID` (default `claude-sonnet-4-5`).

- [ ] **Step 1: Write the four tests,** each `#[tokio::test] #[ignore]`, each skipping with
  `eprintln!` when `ANTHROPIC_API_KEY` is not set. Model:
  `AnthropicModel::messages(<id>).extended_thinking(ExtendedThinking::Enabled { budget_tokens: 2048 })`,
  default headers, `max_output_tokens = Some(4096)`. Tools `lookup_city_code` and
  `weather_by_code` with the probe schemas
  (`docs/superpowers/probes/2026-11-21-sma-840-anthropic-thinking-probe.sh`); results
  `PAR-7351` and `18 C, light rain`. Prompt: the probe `BASE_PROMPT`. No forced `tool_choice`.
  - `live_thinking_round_trip`: turn 1 must hold a tool call and a `Block` with a `Thinking`
    signature, else fail with a message that names which one is missing. Build the turn-2 request
    (turn 1 items plus results); assert `build_body(&model.cfg, &req)` holds that signature; turn
    2 ends with `Finish` and no error.
  - `live_agent_loop_sends_signature`: a `RecordingModel` (wraps `AnthropicModel`, implements
    `Model`, pushes each `ModelRequest` into an `Arc<Mutex<Vec<_>>>`, delegates the rest) in an
    `LlmAgent` with the two tools. Assert the run stops with `StopReason::Completed`, and that
    `build_body` of the second recorded request holds the signature from its Anthropic `Block`
    part.
  - `live_redacted_round_trip`: prompt `REDACT_TRIGGER` + `" "` + `BASE_PROMPT` (the value is in
    the probe script). Assert turn 1 has at least one `RedactedThinking` block, the turn-2 body
    holds every redacted `data` value, and turn 2 has no error.
  - `live_interleaved_beta_thinks_again`: `live_thinking_round_trip` with
    `.beta("interleaved-thinking-2025-05-14")`; assert turn 2 has a thinking `Block`. The failure
    message says that run B saw this once and the model can skip thinking by chance.
  - Assertions print no signature and no redacted value (print lengths only).
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-providers-anthropic --all-features` —
  expected: PASS, the four tests listed as ignored. Do not run them with `--ignored`.
- [ ] **Step 3: Run** the gates — expected: PASS.
- [ ] **Step 4: Commit** `test(providers): SMA-840 add live anthropic thinking round trip tests`.
  In the task report, give Sven the command
  `ANTHROPIC_API_KEY=… cargo test -p paigasus-helikon-providers-anthropic -- --ignored live_`.

### Task 8: Docs

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/lib.rs` (crate docs: a section
  "Extended thinking in a tool loop")
- Modify: `crates/paigasus-helikon-providers-anthropic/src/settings.rs` (`ExtendedThinking` model
  compatibility list)
- Modify: `crates/paigasus-helikon-providers-anthropic/README.md` (a new section before "Links")
- Modify: `docs/book/src/concepts/model-providers.md` (the Anthropic section, lines 116-133)
- Modify: `docs/book/src/concepts/core-primitives.md:37` (it names only the Gemini key)
- Modify: `docs/book/src/concepts/sessions.md:276` (it gives Gemini as the only reason)
- Modify: `docs/book/src/concepts/agent-loop.md:71` (the Gemini-only `MessageOutput` sentence)

- [ ] **Step 1: Write the text,** in Simplified Technical English:
  - Crate docs and README: the stream keeps each `thinking` and `redacted_thinking` block as
    `ContentPart::ProviderData` with key `PROVIDER_DATA_KEY`, and the next request sends the blocks
    back unchanged for every assistant message in the history; the `data` shape is private; for
    4.x `Enabled` models, add `.beta("interleaved-thinking-2025-05-14")` to let the model think
    again after a tool result; `claude-sonnet-5-5` needs `ExtendedThinking::Adaptive`; the cost
    (more input tokens on each request, numbers from Task 3's commit body); recovery (one rejected
    block makes each later request of the session fail; remove every part with
    `provider == PROVIDER_DATA_KEY` from the stored history); privacy (the thinking text is stored
    two times; a host that removes `Reasoning` must also remove these parts).
  - `settings.rs`: add "Claude Sonnet 5.5 rejects `Enabled { .. }` (400). Use `Adaptive`."
  - Book: `model-providers.md` gets a short form of the README section; `core-primitives.md` adds
    that the Anthropic crate exports `PROVIDER_DATA_KEY` (`"anthropic"`); `sessions.md` adds
    Anthropic thinking blocks as a second reason and says that the estimate is high on models
    that strip earlier thinking; `agent-loop.md` adds that on `claude-sonnet-5-5` a tool-only turn
    gives a `MessageOutput` that holds only provider data, because its thinking text is empty.
- [ ] **Step 2: Run** `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`,
  `npx markdownlint-cli2`, `mdbook build docs/book`, and the gates — expected: all clean.
- [ ] **Step 3: Commit** `docs(docs): SMA-840 document the anthropic thinking round trip`
  The PR body (a later pipeline stage) carries the rollback note of the spec section
  "Docs and release"; this task does not write it.
