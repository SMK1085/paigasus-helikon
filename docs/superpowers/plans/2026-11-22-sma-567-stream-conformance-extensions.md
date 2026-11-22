# SMA-567 stream conformance extensions — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the SMA-533 coverage gaps that captured traffic supports, and pin the rest in a new
`UNCAPTURED` table.

**Architecture:** Extend `tests/provider-stream-conformance` in its own style: new `Scenario`
variants, new `classify` rules (proven by `Fake`s), new `floor_violation` checks (proven by direct
`floor_violation` tests), and an `Outcome::Uncaptured` outcome cross-checked against a pinned table.
One doc sentence goes into `core`, and one test goes into `providers-bedrock`.

**Tech Stack:** Rust 1.94, tokio, `serde_json`, `aws-smithy-eventstream` (already a dependency).

**Spec:** `docs/superpowers/specs/2026-11-22-sma-567-stream-conformance-extensions-design.md`. Read
it before every task. Section numbers below (§) refer to it.

## Global Constraints

- Worktree: `/Users/smaschek/dev/paigasus/paigasus-helikon/.worktrees/sma-567`, branch
  `feature/sma-567-extend-the-stream-conformance-suite-to`. Use absolute paths. Never run
  `git checkout`, `git switch`, `git reset`, `git stash` or any command that moves `HEAD`.
- Work in the foreground. Do not background a `cargo` run. Do not end a task before its command
  reports a final status.
- Suite test command: `cargo test -p paigasus-helikon-provider-stream-conformance`.
- Before each commit: `cargo fmt --all` and
  `cargo clippy -p paigasus-helikon-provider-stream-conformance --all-targets -- -D warnings`.
- Commit messages: `<type>(<scope>): SMA-567 <lowercase subject>`. Scopes must match `.versionrc`.
  There is no `tests` scope. Use `test(providers)` for suite and provider-test commits, and
  `docs(core)` and `docs(docs)` for §6.6. End each message with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Stage explicit paths only. Never `git add -A` or `git add .` (`.env` is not ignored).
- Provenance rule (§1, §6.9): every new fixture is transcribed from a named captured file, and each
  edit is listed in its comment. Never invent an event.
- Decline and uncaptured reasons in `tests/conformance.rs` are **literal strings** copied by hand.
  Never look them up from the tables (header rule 1 of `tests/conformance.rs`).
- **Stop rule (§7):** when the full suite fails on a real subject after you add a rule, do not change
  the rule, a fixture or a translator to make it pass. Stop and report the subject, the scenario and
  the `observed:` line.
- Every exhaustive `match` on `Scenario` stays exhaustive, with no `_` arm (see
  `bedrock::fixture_finish_reason`).

## Review Focus

1. **`Err` then `Usage` then something else.** Option B exempts only `Usage`. `Err, Usage,
   TokenDelta` must still give `EventAfterError`. Test in Task 2.
2. **Gemini's `"finishReason": "STOP"` with a space.** A byte-faithful transcription makes
   `encodes_stop_reason` return `false`, and the scenario fails with `StopReasonDeclarationMismatch`.
   Test in Task 6.
3. **A blank-id group with two names under assertion 10.** It must be skipped, not parsed as one
   concatenated value. Test in Task 3.
4. **A subject that reports `Uncaptured` for a pair that `DECLINED` lists, or the reverse.** It must
   fail. Test in Task 1.
5. **Empty deltas in the channel order.** Anthropic's `content_block_start` for text carries
   `"text":""`. If the translator emits an empty `TokenDelta` there, it is a run. The declared order
   comes from the capture, and the stop rule applies if they disagree. Recorded in Task 5.

---

### Task 0: File the three follow-up Linear issues (coordinator, not a subagent)

- [x] **Step 1:** In the Linear project `Paigasus Helikon` (team `Sven Maschek`), check with
  `list_issues` that no matching issue exists. Then create three issues, related to SMA-567, with
  the labels `area:providers`, `area:testing` and `Improvement`:
  1. Capture a reasoning stream for `bedrock`, `litellm` and `openai/responses`. The SMA-585
     gpt-5-mini captures are the first input.
  2. Capture a parallel tool-call stream for `anthropic`, `bedrock`, `litellm`, `openai/chat` and
     `openai/responses`, if possible with interleaved arguments. Record spec §3.5.
  3. Capture a zero-argument tool-call stream for `anthropic`, `bedrock`, `gemini`, `litellm` and
     `openai/chat`, including a stream with no argument deltas. Note Gemini's `"null"` arguments.

  Each issue's acceptance criteria says that it closes only when its `UNCAPTURED` rows are removed,
  by a capture or by a reviewed move to `DECLINED`.
- [x] **Step 2:** Recorded: reasoning `SMA-858`, parallel `SMA-859`, zero-arg `SMA-860`. The tasks below cite them.

### Task 1: `Outcome::Uncaptured` and the `UNCAPTURED` table

**Files:**
- Modify: `tests/provider-stream-conformance/src/lib.rs` (`Outcome`, `assert_conforms`,
  `assert_declines_match`, tests module)
- Modify: `tests/provider-stream-conformance/src/declines.rs`
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (header rule 1 doc only)

**Interfaces:**
- Produces: `Outcome::Uncaptured(&'static str)`;
  `pub const UNCAPTURED: &[(&str, Scenario, &str, &str)]` (subject, scenario, reason, follow-up id),
  empty in this task; `#[cfg(test)] const UNCAPTURED_SCENARIOS: &[Scenario]` (the allow-list),
  empty in this task. Later tasks add rows to both.

- [ ] **Step 1: Write the failing tests.** In the `lib.rs` tests module, give the mock `Subject` a
  field `uncaptured: &'static [(Scenario, &'static str)]` (default `&[]`) that `stream()` returns
  as `Outcome::Uncaptured`. Add three `#[should_panic]` tests that mirror the three decline tests:
  - `an_unpinned_uncaptured_pair_fails` — expected `"unexpected uncaptured"`;
  - `a_pinned_uncaptured_pair_that_stopped_happening_fails` — expected `"is pinned as uncaptured"`.
    Drive it with a test-only table, see Step 3;
  - `a_drifted_uncaptured_reason_fails` — expected `"drifted"`.

  Add `an_uncaptured_outcome_for_a_declined_pair_fails` — the subject reports `Uncaptured` with the
  `DECLINED` reason for `ThinkingToolCallCleanStop`. It must panic with `"unexpected uncaptured"`.
  In `declines.rs` tests, add:
  - `no_pair_is_in_both_tables`;
  - `every_uncaptured_subject_is_registered`;
  - `every_follow_up_is_a_linear_id` (each fourth field matches `^SMA-\d+$`, checked without a
    regex crate);
  - `every_scenario_has_a_serving_subject` (for each `Scenario::ALL` entry, some name in
    `REGISTERED_SUBJECTS` is in neither table);
  - `uncaptured_rows_name_only_allow_listed_scenarios`.
- [ ] **Step 2:** Run the suite test command. Expected: compile failure on `Outcome::Uncaptured`.
- [ ] **Step 3: Implement.** Generalise `assert_declines_match` into
  `fn assert_pinned_match(subject: &str, kind: PinnedKind, observed: &[(Scenario, &'static str)], pinned: &[(Scenario, &'static str)])`,
  where `PinnedKind` is `Declined` or `Uncaptured` and selects the message words ("decline" /
  "uncaptured", and "DECLINED" / "UNCAPTURED"). Keep the three existing message substrings that the
  decline tests match. `assert_conforms` collects both kinds and calls it twice. To test the stale
  direction without a real row, let the mock `Subject` carry
  `pinned_uncaptured: &'static [(Scenario, &'static str)]`, and expose a `#[cfg(test)]` entry point
  `assert_conforms_with(subject, declined, uncaptured)` that `assert_conforms` calls with the real
  tables. Update the docs of `Outcome`, `assert_conforms` and header rule 1 in `conformance.rs` to
  name both tables.
- [ ] **Step 4:** Run the suite test command. Expected: all pass, including the six real subjects.
- [ ] **Step 5: Commit** `test(providers): SMA-567 add the pinned uncaptured table`.

### Task 2: Assertion 9 — nothing after an `Err` except `Usage`

**Files:**
- Modify: `tests/provider-stream-conformance/src/lib.rs` (`Violation`)
- Modify: `tests/provider-stream-conformance/src/check.rs`
- Modify: `tests/provider-stream-conformance/src/fakes.rs`

**Interfaces:**
- Produces: `Violation::EventAfterError`; `Fake::TokenAfterError`;
  `Fake::ProviderDataUnknownCall`.

- [ ] **Step 1: Write the failing tests** in `check.rs`:
  - `token_after_err_is_a_violation`: `[token, Err, token]` on `ErrorMidGeneration` gives
    `Some(Violation::EventAfterError)`;
  - `usage_after_err_conforms`: `[token, Err, usage]` on `ErrorMidGeneration` gives `None`;
  - `token_after_usage_after_err_is_a_violation`: `[token, Err, usage, token]` gives
    `Some(Violation::EventAfterError)` (Review Focus 1);
  - `finish_after_err_keeps_its_own_name`: `[token, Err, finish]` on `ErrorAfterStopReason` still
    gives `FinishAfterError`.

  In `fakes.rs`, add cases to `every_fake_is_rejected_with_its_classification`:
  `(Fake::TokenAfterError, Scenario::ErrorMidGeneration, Violation::EventAfterError)` and
  `(Fake::ProviderDataUnknownCall, Scenario::ToolCallCleanStop, Violation::ProviderDataUnknownCall { call_id: "x".into() })`.
- [ ] **Step 2:** Run the suite test command. Expected: compile failure on the new variants.
- [ ] **Step 3: Implement.** Assertion 9 in `classify` goes directly after assertion 6: from the
  first `Err`, any later item that is not `Ok(ModelEvent::Usage { .. })` returns
  `EventAfterError`. Its comment says that the `Usage` exemption is option B of the spec's §6.6, and
  gives the reason (consumed tokens are billing data). Update the `classify` doc on rule order.
  `Fake::TokenAfterError` appends `token("trailing")`. `Fake::ProviderDataUnknownCall` pushes a
  call-scoped `ProviderData` with `call_id: Some("x")` before the `Finish`.
- [ ] **Step 4:** Run the suite test command. Expected: all pass. Apply the stop rule if a real
  subject fails.
- [ ] **Step 5: Commit** `test(providers): SMA-567 assert nothing but usage follows an err`.

### Task 3: Assertion 10 — tool arguments parse as JSON

**Files:**
- Modify: `tests/provider-stream-conformance/src/lib.rs` (`Violation`)
- Modify: `tests/provider-stream-conformance/src/check.rs`
- Modify: `tests/provider-stream-conformance/src/fakes.rs`

**Interfaces:**
- Produces: `Violation::ToolArgsNotJson { call_id: String }`; `Fake::ArgsNotJson`.

- [ ] **Step 1: Write the failing tests** in `check.rs`:
  - `truncated_args_are_a_violation`: `tool("c1", Some("get_weather"), "{\"city\":")` then `Finish`
    gives `ToolArgsNotJson { call_id: "c1" }`;
  - `blank_args_conform`: `tool("c1", Some("get_weather"), "")` then `Finish` gives `None`;
  - `whitespace_args_conform`: args `"  "` gives `None`;
  - `non_object_json_conforms`: args `"null"` gives `None` (Gemini's shape, spec §3.4);
  - `truncated_args_without_finish_conform`: the truncated case on `TruncatedMidGeneration`, with no
    `Finish`, gives `None`;
  - `blank_id_group_with_two_names_is_not_parsed`: `tool("", Some("a"), "{}")`,
    `tool("", Some("b"), "{}")`, `Finish` gives `None` (Review Focus 3);
  - `blank_id_group_with_one_name_is_parsed`: `tool("", Some("a"), "{")`, `Finish` gives
    `ToolArgsNotJson { call_id: "" }`.

  In `fakes.rs`, add `(Fake::ArgsNotJson, Scenario::ToolCallCleanStop, Violation::ToolArgsNotJson { call_id: "c1".into() })`.
- [ ] **Step 2:** Run the suite test command. Expected: compile failure.
- [ ] **Step 3: Implement.** Assertion 10 in `classify` runs after assertion 8, and only when the
  stream holds a `Finish`. Group `args_delta` by `call_id` in first-seen order. Count names per group
  with the assertion-7 counts that already exist. Skip a `""` group whose name count is not 1. Trim
  the concatenation. Blank becomes `{}`. Parse with `serde_json::from_str::<serde_json::Value>`.
  `Fake::ArgsNotJson` uses `replace_tool_deltas` with `[(Some(TOOL_NAME), "{\"city\":")]`.
- [ ] **Step 4:** Run the suite test command. Expected: all pass (stop rule applies).
- [ ] **Step 5: Commit** `test(providers): SMA-567 assert tool arguments parse as json`.

### Task 4: Two evidence floors — no stray `Err`, and `CleanStop` has `Usage`

**Files:**
- Modify: `tests/provider-stream-conformance/src/lib.rs` (`floor_violation` and its doc; tests
  module)

**Interfaces:**
- Consumes: `fakes::conforming(scenario)` (crate-visible).

- [ ] **Step 1: Write the failing tests** in the `lib.rs` tests module, calling `floor_violation`
  directly:
  - `an_err_outside_the_error_scenarios_fails_its_floor`: `conforming(CleanStop)` with
    `Err(ModelError::Unavailable)` inserted before `Finish` gives `InsufficientEvidence` whose text
    contains `"an Err outside an error scenario"`;
  - `a_clean_stop_without_usage_fails_its_floor`: `conforming(CleanStop)` without its `Usage` gives
    `InsufficientEvidence` whose text contains `"no Usage in CleanStop"`.
- [ ] **Step 2:** Run the suite test command. Expected: both FAIL (no violation returned).
- [ ] **Step 3: Implement** both floors in `floor_violation`. The no-`Err` floor applies to every
  scenario except `ErrorMidGeneration` and `ErrorAfterStopReason`. Add both to the
  `floor_violation` doc, and say there that the ticket's adjacency floor was replaced, with the
  reason (spec §3.2).
- [ ] **Step 4:** Run the suite test command. Expected: all pass (stop rule applies).
- [ ] **Step 5: Commit** `test(providers): SMA-567 floor stray errs and a missing clean-stop usage`.

### Task 5: `ReasoningCleanStop` and the channel-order floor

**Files:**
- Modify: `tests/provider-stream-conformance/src/lib.rs` (`Scenario`, `ALL`,
  `expects_stop_reason`, `Channel`, `StreamUnderTest`, `floor_violation`, `Violation`, the
  `fixture_finish_reason` doc, the mock `Subject`)
- Modify: `tests/provider-stream-conformance/src/fakes.rs` (`conforming`, `finish`,
  `declared_finish_reason`)
- Modify: `tests/provider-stream-conformance/src/declines.rs` (rows, allow-list)
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (all six subjects; the anthropic
  script and module doc)

**Interfaces:**
- Produces: `Scenario::ReasoningCleanStop`;
  `pub enum Channel { Reasoning, Text, Tool }` (derive `Debug, Clone, Copy, PartialEq, Eq`);
  trait method `fn fixture_channel_order(&self, scenario: Scenario) -> Option<&'static [Channel]>;`
  (no default; every subject implements it);
  `Violation::ChannelOrderMismatch { expected: Vec<Channel>, observed: Vec<Channel> }`.

- [ ] **Step 1: Write the failing tests** (`lib.rs` tests module, direct `floor_violation` calls
  with a new `channel_order` argument):
  - `a_reasoning_run_after_the_text_run_fails`: `conforming(ReasoningCleanStop)` with its
    `ReasoningDelta` moved after the `TokenDelta`, declared `[Reasoning, Text, Tool]`, gives
    `ChannelOrderMismatch { expected: [Reasoning, Text, Tool], observed: [Text, Reasoning, Tool] }`;
  - `a_reasoning_scenario_without_reasoning_fails`: the `ReasoningDelta` removed gives
    `InsufficientEvidence` containing `"no ReasoningDelta"`;
  - `a_reasoning_scenario_without_a_declared_order_fails`: declared `None` gives
    `InsufficientEvidence` containing `"declares no channel order"`.

  `the_conforming_fake_clears_every_floor` must pass the declared order for each scenario: add a
  test-side `declared_channel_order(scenario)` that returns `Some(&[Reasoning, Text, Tool])` for
  `ReasoningCleanStop` and `None` otherwise.
- [ ] **Step 2:** Run the suite test command. Expected: compile failure.
- [ ] **Step 3: Implement the harness.** `floor_violation` gains a parameter
  `channel_order: Option<&[Channel]>`, and `assert_conforms` passes
  `subject.fixture_channel_order(scenario)`. The check is spec §6.3 "Channel order". Its doc
  comment says that it checks fidelity to the wire, not the contract (`core/src/model.rs:56-57`), and
  that an empty delta is a run. `ReasoningCleanStop` expects a stop reason, ends with
  `Finish(ToolCalls)` in `fakes::finish`, and gets the tool floors and the clean-stop `Finish`
  floor. `conforming(ReasoningCleanStop)` is `ReasoningDelta`, `token("hi")`, the two tool deltas,
  `Usage`, `Finish(ToolCalls)`. Add `ReasoningCleanStop` to `UNCAPTURED_SCENARIOS`. Update the
  `fixture_finish_reason` doc ("Exactly four" becomes five in this task). The mock `Subject` must
  serve, decline or report uncaptured exactly as the tables say for its name.
- [ ] **Step 4: Implement the subjects.**
  - `anthropic` serves it. Its script is all of run C
    (`crates/paigasus-helikon-providers-anthropic/tests/fixtures/thinking_text_tool_use.txt`):
    thinking at index 0 (start, the `thinking_delta`s, the `signature_delta`, stop), text at
    index 1, `tool_use` at index 2, `message_delta` (`tool_use`), `message_stop`. You may merge
    consecutive deltas of one block into fewer deltas. List each merge and the dropped `ping` in
    the provenance comment. `request()` offers the tool that the capture calls. Declare
    `fixture_channel_order` = `[Reasoning, Text, Tool]`. If the observed order differs, apply the
    stop rule (Review Focus 5). Replace the module doc sentence "this suite has no reasoning-delta
    scenario".
  - `gemini` returns
    `Outcome::Declined("the request never enables thought parts, so none arrive on the wire")`.
  - `openai/chat` returns `Outcome::Declined("the Chat Completions wire carries no reasoning text")`.
  - `bedrock`, `litellm` and `openai/responses` return
    `Outcome::Uncaptured("no captured reasoning stream")`.
  - Every subject returns `None` from `fixture_channel_order` for all other scenarios.

  Add the two `DECLINED` rows and the three `UNCAPTURED` rows (follow-up `SMA-858`).
- [ ] **Step 5:** Run the suite test command. Expected: all pass (stop rule applies).
- [ ] **Step 6: Commit** `test(providers): SMA-567 add a reasoning channel-order scenario`.

### Task 6: `ParallelToolCallCleanStop` and declared calls

**Files:**
- Modify: `tests/provider-stream-conformance/src/lib.rs` (`Scenario`, `ExpectedCall`,
  `StreamUnderTest`, `floor_violation`, `Violation`, docs, mock `Subject`)
- Modify: `tests/provider-stream-conformance/src/fakes.rs` (`tool` takes a `call_id`;
  `conforming`; `finish`; helpers)
- Modify: `tests/provider-stream-conformance/src/declines.rs`
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (all six subjects; gemini script)

**Interfaces:**
- Consumes: Task 5's `floor_violation` signature.
- Produces: `Scenario::ParallelToolCallCleanStop`;
  `pub struct ExpectedCall { pub name: &'static str, pub args: &'static str }`;
  trait method `fn fixture_calls(&self, scenario: Scenario) -> &'static [ExpectedCall];`;
  `Violation::ToolCallsMismatch { expected: Vec<(String, serde_json::Value)>, observed: Vec<(String, serde_json::Value)> }`;
  `floor_violation` gains `calls: &[ExpectedCall]`.

- [ ] **Step 1: Write the failing tests** (direct `floor_violation` calls):
  - `swapped_parallel_arguments_fail`: `conforming(ParallelToolCallCleanStop)` (calls `c1`
    `get_weather` `{"city":"Berlin"}`, `c2` `get_weather` `{"city":"Rome"}`) with the two argument
    strings swapped gives `ToolCallsMismatch`;
  - `a_single_call_in_the_parallel_scenario_fails`: only `c1` gives `InsufficientEvidence`
    containing `"fewer than two"`;
  - `unparseable_arguments_are_left_to_classify`: `c2`'s arguments truncated gives `None` from
    `floor_violation`, and `ToolArgsNotJson { call_id: "c2" }` from `classify`.

  In `fakes.rs`, update `TwoNamedDeltas`/`NoNamedDelta` for the new `tool(call_id, name, args)`
  signature with no change in behaviour.
- [ ] **Step 2:** Run the suite test command. Expected: compile failure.
- [ ] **Step 3: Implement the harness** (spec §6.3 "Declared calls"). For the scenarios with a
  non-empty `fixture_calls`, the "declared name is present" floor reads the names from it instead of
  `fixture_tool_name`. `ParallelToolCallCleanStop` expects a stop reason, ends with
  `Finish(ToolCalls)`, and gets the tool floors and the clean-stop floor. Add it to
  `UNCAPTURED_SCENARIOS`. "Exactly four" in the `fixture_finish_reason` doc becomes six.
- [ ] **Step 4: Implement the subjects.**
  - `gemini` serves it from
    `crates/paigasus-helikon-providers-gemini/tests/fixtures/parallel_signed.sse`: four chunks,
    three single-`functionCall` chunks (`search`, `weather` Paris, `weather` Rome; only the first
    carries `thoughtSignature`), then the `STOP` chunk. Cite SMA-827 design "Capture 1"
    (`docs/superpowers/specs/2026-11-20-sma-827-gemini-thought-signatures-design.md:299`). Add a
    builder variant of `tool_call_chunk` that can leave out `thoughtSignature`. `request()` offers
    `search` and `weather`. `fixture_calls` returns the three calls with their captured arguments.
  - **Review Focus 2:** first write `gemini_parallel_script_encodes_its_stop_reason`, which asserts
    `Gemini.encodes_stop_reason(Scenario::ParallelToolCallCleanStop)`. Then make it pass, either by
    letting the marker scan accept `"finishReason": "` too, or by recording the whitespace change as
    an edit in the provenance comment. Prefer the marker change, because it keeps the bytes faithful.
  - `anthropic`, `bedrock`, `litellm`, `openai/chat` and `openai/responses` return
    `Outcome::Uncaptured("no captured parallel tool-call stream")`.
  - Every subject returns `&[]` from `fixture_calls` for all other scenarios.

  Add the five `UNCAPTURED` rows (follow-up `SMA-859`).
- [ ] **Step 5:** Run the suite test command. Expected: all pass (stop rule applies).
- [ ] **Step 6: Commit** `test(providers): SMA-567 add a parallel tool-call scenario`.

### Task 7: `ZeroArgToolCallCleanStop`

**Files:**
- Modify: `tests/provider-stream-conformance/src/lib.rs`, `src/fakes.rs`, `src/declines.rs`
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (all six subjects;
  `openai_responses` script)

**Interfaces:**
- Consumes: Task 6's `ExpectedCall` and `fixture_calls`.
- Produces: `Scenario::ZeroArgToolCallCleanStop`.

- [ ] **Step 1: Write the failing test:** `a_zero_arg_fixture_with_real_arguments_fails`:
  `conforming(ZeroArgToolCallCleanStop)` (one call `c1` `get_current_time`, one delta `{}`) with the
  delta changed to `{"tz":"UTC"}`, declared `[ExpectedCall { name: "get_current_time", args: "{}" }]`,
  gives `ToolCallsMismatch`.
- [ ] **Step 2:** Run the suite test command. Expected: compile failure.
- [ ] **Step 3: Implement the harness.** The scenario expects a stop reason, ends with
  `Finish(ToolCalls)`, and gets the tool floors and the clean-stop floor. Add it to
  `UNCAPTURED_SCENARIOS`. "Exactly four" becomes seven in the `fixture_finish_reason` doc.
- [ ] **Step 4: Implement the subjects.**
  - `openai/responses` serves it from
    `crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call_zero_args.txt`
    (`output_item.added`, the `{}` arguments delta, `.done`, `output_item.done`,
    `response.completed`). Keep that file's list of edits in the provenance comment. `request()`
    offers `get_current_time`. `fixture_calls` returns `get_current_time` with `{}`.
  - `anthropic`, `bedrock`, `gemini`, `litellm` and `openai/chat` return
    `Outcome::Uncaptured("no captured zero-argument tool-call stream")`.

  Add the five `UNCAPTURED` rows (follow-up `SMA-860`).
- [ ] **Step 5:** Run the suite test command. Expected: all pass (stop rule applies).
- [ ] **Step 6: Commit** `test(providers): SMA-567 add a zero-argument tool-call scenario`.

### Task 8: The Bedrock wire-order check

**Files:**
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (`bedrock` module)

**Interfaces:**
- Produces (module-private): `fn event_types(chunks: &[Vec<u8>]) -> Vec<String>` and
  `fn assert_message_stop_before_metadata(chunks: &[Vec<u8>])`.

- [ ] **Step 1: Write the failing tests:**
  - `a_swapped_metadata_and_message_stop_is_caught`: a chunk list `[message_stop("end_turn"), metadata()]`
    reversed must panic in `assert_message_stop_before_metadata` (`#[should_panic(expected = "metadata before messageStop")]`);
  - `every_bedrock_script_keeps_message_stop_before_metadata`: for each `Scenario::ALL` entry with a
    script that has both frames, the function does not panic, and at least two scripts were checked
    (`CleanStop`, `ToolCallCleanStop`).
- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-provider-stream-conformance bedrock`.
  Expected: compile failure.
- [ ] **Step 3: Implement.** `event_types` decodes each chunk with
  `aws_smithy_eventstream::frame::MessageFrameDecoder` (as `src/eventstream.rs:210` does in its
  tests) and reads the `:event-type` string header. Do not re-encode. Replace the "do not reorder
  it casually" prose in the `metadata()` doc with a pointer to the check, and keep the reason.
- [ ] **Step 4:** Run the suite test command. Expected: all pass.
- [ ] **Step 5: Manual mutation.** Swap `message_stop` and `metadata` in the `CleanStop` script,
  run the suite, confirm that `every_bedrock_script_keeps_message_stop_before_metadata` fails,
  then revert. Record the output line for the PR body.
- [ ] **Step 6: Commit** `test(providers): SMA-567 pin the bedrock terminal frame order`.

### Task 9: The contract sentence and the Bedrock pin (§6.6, option B)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs` (the `Model::invoke` ordering list)
- Modify: `docs/book/src/concepts/model-providers.md` (the paragraph that calls `Finish` terminal)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/stream.rs` (tests module)

- [ ] **Step 1: Write the test** in `providers-bedrock/src/stream.rs`:
  `a_refused_tool_call_yields_err_then_usage_and_nothing_else`. Feed `tool_use_start`,
  `tool_delta`, `message_stop(ToolUse)`, `metadata` to a `forbid_tool_calls(true)` translator, then
  call `finish()`. Assert that the items are exactly `[Err, Ok(Usage)]` and that `finish()` adds
  nothing. The doc comment cites the contract sentence.
- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-providers-bedrock a_refused_tool_call`.
  Expected: PASS, because this pins current behaviour. If `finish()` yields an item, apply the stop
  rule.
- [ ] **Step 3: Commit** `test(providers): SMA-567 pin bedrock usage after a refused tool call`.
- [ ] **Step 4: Add the contract sentence.** After "`Finish` is the terminal event; nothing follows
  it." add this exact text:

  ```text
  /// - An `Err` item is terminal for every event except `Usage`: implementations MUST NOT yield
  ///   any item after an `Err` other than `Usage` snapshots for the same response.
  ```

  Bring the book paragraph into line with the same rule. Do not edit any `version` field.
- [ ] **Step 5:** Run `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --no-deps` and
  `npx markdownlint-cli2 docs/book/src/concepts/model-providers.md`. Expected: both clean.
- [ ] **Step 6: Commit** the core file alone as
  `docs(core): SMA-567 state that only usage may follow an err`, then the book file alone as
  `docs(docs): SMA-567 describe err terminality in the model providers page`.

### Task 10: Final verification (coordinator)

- [ ] **Step 1:** Run every CI gate from `CLAUDE.md` → "Common commands", in order. Expected: each
  exits 0. `mdbook build docs/book` must also be clean.
- [ ] **Step 2:** `git diff origin/main --stat`. Confirm that only the files named in this plan
  changed, and that no `Cargo.toml` `version` changed.
- [ ] **Step 3:** Check the spec §9 acceptance criteria one by one, and record each with its
  evidence for the PR body.
