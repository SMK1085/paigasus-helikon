# SMA-860 zero-argument captures Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve `ZeroArgToolCallCleanStop` for `openai/chat`, `litellm`, `anthropic`, `gemini` and `bedrock` from the committed raw captures, and remove their five `UNCAPTURED` rows.

**Deviation from spec §6.3 (planner's call):** for the four SSE subjects the guard compares the script's concatenated chunks with the whole `.raw` file, byte for byte. This is stronger than a frame-split comparison and needs no shared parser. Bedrock still decodes frames, because the suite writes the headers in a different order than AWS.

**Architecture:** Each subject copies its capture's payloads into a const and sends them byte for byte (approach A). A guard test in each subject module compares the script with the committed `.raw` file, so the copy cannot drift. Each subject task also removes its own `UNCAPTURED` row and its mock-subject entry, so the suite is green after every task.

**Tech Stack:** Rust, `tokio`, `serde_json`, `aws-smithy-eventstream` (`MessageFrameDecoder`, `write_message_to`).

**Spec:** `docs/superpowers/specs/2026-11-24-sma-860-zero-arg-captures-design.md`

## Global Constraints

- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-860`, on branch `feature/sma-860-capture-a-zero-argument-tool-call-stream-for`. Do not run `git checkout`, `git switch`, `git reset`, `git stash` or `git rebase`. Do not push.
- Do not edit any file under `tests/provider-stream-conformance/fixtures/`. The `.raw` files are the wire bytes; their SHA-256 values are in spec §4.
- Do not change a provider translator (`crates/*/src/`). If a subject fails the suite, stop and report the failure (spec §5).
- The suite package is `paigasus-helikon-provider-stream-conformance`. Test command: `cargo test -p paigasus-helikon-provider-stream-conformance`.
- Run long cargo commands in the foreground and wait for them to finish. Do not end the task while a command is still running.
- Before each commit: `cargo fmt --all` and `cargo clippy -p paigasus-helikon-provider-stream-conformance --all-targets -- -D warnings`.
- Commit messages: `test(providers): SMA-860 <lowercase subject>`, ending with the line `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Stage explicit paths only; never `git add -A`.
- The declared call for every subject is `ExpectedCall { name: "get_current_time", args: "{}" }`.
- The tool for every subject's `request()` in this scenario: `ToolDef::new("get_current_time", "Return the current server time. Takes no arguments.", json!({"type":"object","properties":{}}))`, and `req.tool_choice = Some(ToolChoice::Tool { name: "get_current_time".into() })` (adjust the field type to the real `ToolChoice::Tool` definition in `crates/paigasus-helikon-core/src/model.rs`).
- Doc and comment text follows the repo's Simplified Technical English style: short sentences, active voice.

## Review Focus

- **A copy error in a payload that no floor reads** (Anthropic `ping`, `message_start`, a usage chunk). The drivers skip an unparseable frame with only a warning. Covered by each subject's guard test (whole-body equality or decoded-frame equality).
- **Gemini CRLF framing.** A script that writes `\n\n` instead of `\r\n\r\n` still passes the floors. Covered by the gemini guard test (byte equality with the CRLF `.raw` file).
- **Anthropic and Bedrock `""` arguments.** The floors must read blank as `{}`. Covered by the Task 1 lib test and by the subject runs.
- **A served scenario still pinned as uncaptured, or a mock subject that still reports it.** Covered by `conforms` in each module and by the lib mock tests after each row removal.
- **A guard that compares nothing** (for example, an empty const). Covered by each guard asserting the frame count, and by the one-byte mutation step in each task.

---

### Task 1: Eventstream helpers and the blank-arguments floor test

**Files:**
- Modify: `tests/provider-stream-conformance/src/eventstream.rs`
- Modify: `tests/provider-stream-conformance/src/lib.rs` (tests module only)

**Interfaces:**
- Produces: `pub fn frame_raw(event_type: &str, payload: &[u8]) -> Vec<u8>` and `pub fn decode_frames(raw: &[u8]) -> Vec<(String, Vec<u8>)>` in `eventstream`. `decode_frames` returns `(:event-type value, payload bytes)` for each frame, in order, and panics on an incomplete frame or trailing bytes.
- `frame(event_type, payload: &serde_json::Value)` keeps its signature and calls `frame_raw` with `serde_json::to_vec(payload)`.

- [ ] **Step 1: Write the failing tests** in `eventstream.rs`'s test module:
  - `frame_raw_keeps_the_payload_bytes`: `frame_raw("metadata", br#"{"b":1, "a":2}"#)` decoded with `decode_frames` gives `vec![("metadata".to_owned(), br#"{"b":1, "a":2}"#.to_vec())]` (key order and the space survive).
  - `decode_frames_reads_two_concatenated_frames`: concatenate `frame_raw("messageStart", b"{}")` and `frame_raw("messageStop", b"{\"x\":1}")`; `decode_frames` returns both pairs in order.
  - `frame_is_frame_raw_of_the_serialised_value`: `frame("e", &json!({"a":1}))` equals `frame_raw("e", br#"{"a":1}"#)`.
- [ ] **Step 2: Write the failing lib test** in `lib.rs`'s tests module, beside the other `ToolCallsMismatch` tests: `blank_args_match_a_declared_empty_object` — a `ZeroArgToolCallCleanStop` event list with one `ToolCallDelta { call_id: "c1", name: Some("get_current_time"), args_delta: "" }`, `Usage`, `Finish(ToolCalls)`, checked against the declared `[ExpectedCall { name: "get_current_time", args: "{}" }]`, gives no `ToolCallsMismatch`. Build it with the same helpers that the neighbouring `ToolCallsMismatch` tests use.
- [ ] **Step 3: Run** `cargo test -p paigasus-helikon-provider-stream-conformance --lib`. Expected: the eventstream tests fail to compile (no `frame_raw`, `decode_frames`). The lib test passes already (it pins existing behaviour; spec §6.4).
- [ ] **Step 4: Implement `frame_raw` and `decode_frames`.** Move the body of `frame` into `frame_raw`, with the same three headers in the same order. `decode_frames` loops `MessageFrameDecoder::decode_frame` over a `&[u8]` cursor, as `frame_headers_are_exactly_the_three_the_decoder_requires` does, and reads `:event-type` with `as_string()`. Update the module doc (lines 1-12): the module now also re-frames captured payloads and decodes captured frames.
- [ ] **Step 5: Run** the same command. Expected: PASS.
- [ ] **Step 6: Commit** `test(providers): SMA-860 add raw eventstream framing and decoding helpers`.

### Task 2: `openai/chat`

**Files:**
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (`mod openai_chat`, about lines 640-1345)
- Modify: `tests/provider-stream-conformance/src/declines.rs` (remove the `openai/chat` `ZeroArgToolCallCleanStop` row)
- Modify: `tests/provider-stream-conformance/src/lib.rs` (mock: `conforming("openai/chat")` and the two tests near lines 1829 and 1861)
- Read: `tests/provider-stream-conformance/fixtures/openai_chat/zero_arg_tool_call.raw`

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces, in `mod openai_chat`: `const ZERO_ARG_CHUNKS: [&str; 5]` (the `data:` values; the last is `[DONE]`), `const ZERO_ARG_CALLS: &[ExpectedCall]`, `fn captured_frame(payload: &str) -> Vec<u8>` writing `data: {payload}\n\n`, and `const ZERO_ARG_RAW: &[u8] = include_bytes!("../fixtures/openai_chat/zero_arg_tool_call.raw")`.

- [ ] **Step 1: Write the failing guard test** `zero_arg_script_is_the_capture_byte_for_byte`:
  ```rust
  let script = script(Scenario::ZeroArgToolCallCleanStop).expect("served");
  assert_eq!(script.chunks.len(), 5);
  assert_eq!(script.chunks.concat(), ZERO_ARG_RAW);
  ```
  Its doc comment is the provenance record (spec §6.1): CAPTURED, `https://api.openai.com/v1/chat/completions`, `gpt-5-mini` (wire `gpt-5-mini-2025-08-07`), 2026-11-24 20:39:21 UTC, request `fixtures/openai_chat/zero_arg_tool_call.request.json`, 1618 bytes, SHA-256 `5c5e74ac372aa0f954e89801c0addb62592a1ae02ad3c36b427b4d24f56d8cf2`, the deltas `""` then `"{}"`, and "Removed: nothing. Added: nothing. Changed: nothing."
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-provider-stream-conformance --test conformance openai_chat::`. Expected: FAIL (compile error or `None` script).
- [ ] **Step 3: Serve the scenario.** Copy the five `data:` values from the `.raw` file into `ZERO_ARG_CHUNKS` as raw strings (`r##"…"##`), exactly. The script arm maps them through `captured_frame`, `gate_after: None`, `Ending::Clean`. Its comment follows the `openai_responses` zero-arg arm: source file, capture, and "Edits made here: none". Replace the `Outcome::Uncaptured` return in `stream` with the normal served path. `request()`, `fixture_calls()` and `fixture_finish_reason()` per Global Constraints (`Some(FinishReason::ToolCalls)`). Add `("zero-arg tool chunk", captured_frame(ZERO_ARG_CHUNKS[1]))` to `scan_finds_only_a_populated_finish_reason`'s negative list.
- [ ] **Step 4: Remove the row and the mock entries.** Delete the `openai/chat` `ZeroArgToolCallCleanStop` row from `UNCAPTURED`. In `lib.rs`, remove `ZERO_ARG_UNCAPTURED` from the `"openai/chat"` arm of `Subject::conforming`, from `a_pinned_uncaptured_pair_that_stopped_happening_fails`'s `pinned_uncaptured`, and from `an_uncaptured_outcome_for_a_declined_pair_fails`'s `uncaptured`.
- [ ] **Step 5: Run** `cargo test -p paigasus-helikon-provider-stream-conformance`. Expected: PASS.
- [ ] **Step 6: Mutation checks** (revert each after): (a) change `{}` in `ZERO_ARG_CHUNKS[1]` to `{\"x\":1}` → `conforms` fails with `ToolCallsMismatch` and the guard fails; (b) change one byte in `ZERO_ARG_CHUNKS[0]` → the guard fails. Report both results.
- [ ] **Step 7: Commit** `test(providers): SMA-860 serve the captured zero-argument call for openai/chat`.

### Task 3: `litellm`

Same as Task 2, with these differences.

**Files:** `mod litellm` (about lines 1345-2036); `declines.rs` (the `litellm` row). `lib.rs` has no `litellm` mock entry.

- `ZERO_ARG_CHUNKS: [&str; 6]` (last is `[DONE]`); `ZERO_ARG_RAW` from `../fixtures/litellm/zero_arg_tool_call.raw`; `chunks.len() == 6`.
- Provenance: CAPTURED from the LiteLLM 1.98.0 proxy, `ghcr.io/berriai/litellm@sha256:3a1980074ff2ac13883e0ffee1bebec345d8addd004be251079cee173b0e8305`, `/v1/chat/completions`, proxy alias `zero-arg-anthropic` (the wire `model` value), upstream `anthropic/claude-haiku-5-5`, 2026-11-24 20:59:06 UTC, request and config files in `fixtures/litellm/`, 1739 bytes, SHA-256 `b81bc790cdcf6a93e1ef6e8391a353a344d95549ef08a412ee0f7c1f241552c7`. Deltas `""`, `""`, `"{}"`. Say that the upstream call was not captured, so the source of the `{}` is an inference (spec §4 finding 5).
- Mutation (a) edits the chunk that carries `"{}"` (`ZERO_ARG_CHUNKS[2]`).
- Commit `test(providers): SMA-860 serve the captured zero-argument call for litellm`.

### Task 4: `anthropic`

Same as Task 2, with these differences.

**Files:** `mod anthropic` (about lines 2036-3081); `declines.rs` (the `anthropic` row); `lib.rs` (remove `ZERO_ARG_UNCAPTURED` from the `"anthropic"` arm of `Subject::conforming`).

- `const ZERO_ARG_EVENTS: [(&str, &str); 7]` as `(event name, data payload)`: `message_start`, `content_block_start`, `ping`, `content_block_delta`, `content_block_stop`, `message_delta`, `message_stop`. Keep the spaces before each closing `}` exactly.
- `fn captured_frame(event: &str, payload: &str) -> Vec<u8>` writing `event: {event}\ndata: {payload}\n\n`. `ZERO_ARG_RAW` from `../fixtures/anthropic/zero_arg_tool_call.raw`; `chunks.len() == 7`.
- Provenance: CAPTURED, `https://api.anthropic.com/v1/messages`, `anthropic-version: 2023-06-01`, `claude-haiku-5-5`, 2026-11-24 20:38:59 UTC, 1364 bytes, SHA-256 `b6114d956d7d80a6b315fcd2f6866fff8c776e9ebbed846c8ab7ccf9093dd86e`. The arm comment says: the wire sent one `input_json_delta` with `partial_json: ""`; `content_block_start` carries `"input":{}`, which the translator ignores; the floors read blank arguments as `{}`.
- Mutation (a): change `partial_json` `""` to `"{\"x\":1}"` in the `content_block_delta` payload. Add mutation (c): remove the `content_block_delta` frame from the script only (not the const), run `conforms`, and report the exact failure (spec §6.9; expected: no `ToolCallDelta` for the call). Revert.
- Update the `anthropic` module text "The three tool scripts" (near lines 2023-2026) to count the new script.
- Commit `test(providers): SMA-860 serve the captured zero-argument call for anthropic`.

### Task 5: `gemini`

Same as Task 2, with these differences.

**Files:** `mod gemini` (about lines 3081-3810); `declines.rs` (the `gemini` row). `lib.rs` has no `gemini` mock entry.

- `const ZERO_ARG_CHUNKS: [&str; 2]` (the two `data:` values). New `fn captured_frame_crlf(payload: &str) -> Vec<u8>` writing `data: {payload}\r\n\r\n`. Do not add `done()`; `Ending::Clean`. `ZERO_ARG_RAW` from `../fixtures/gemini/zero_arg_tool_call.raw`; `chunks.len() == 2`.
- Keep the real `thoughtSignature` value. Do not replace it with a placeholder.
- Provenance: CAPTURED, `https://generativelanguage.googleapis.com/v1beta/models/gemini-3.8-flash:streamGenerateContent?alt=sse`, `gemini-3.8-flash`, 2026-11-24 21:14:44 UTC, 1383 bytes, SHA-256 `45271e2e2c96d08f183bec5251811498fee18b74c3cbfc92f9a2fc7c35542624`. Note: CRLF line endings on the wire; `"args": {}`, not a missing `args`; the earlier capture without `parameters` (20:39:23) is not committed (spec §4 finding 3).
- Mutation (a): change `"args": {}` to `"args": {"x": 1}`.
- Add the new capture to the `gemini` provenance list in the module doc (near line 2952).
- Commit `test(providers): SMA-860 serve the captured zero-argument call for gemini`.

### Task 6: `bedrock`

**Files:** `mod bedrock` (lines 91-640); `declines.rs` (the `bedrock` row); `lib.rs` (remove `ZERO_ARG_UNCAPTURED` from `BEDROCK_UNCAPTURED` and from the override near line 2243). This is its last use, so delete the const and its doc in this task too, or clippy `-D warnings` fails on dead code.

**Interfaces:**
- Consumes: `eventstream::frame_raw` and `eventstream::decode_frames` (Task 1).
- Produces, in `mod bedrock`: `const ZERO_ARG_EVENTS: [(&str, &str); 6]` (`messageStart`, `contentBlockStart`, `contentBlockDelta`, `contentBlockStop`, `messageStop`, `metadata`), `ZERO_ARG_CALLS`, `ZERO_ARG_RAW` from `../fixtures/bedrock/zero_arg_tool_call.raw`.

- [ ] **Step 1: Write the failing guard test** `zero_arg_events_are_the_capture_frames`:
  ```rust
  let captured = eventstream::decode_frames(ZERO_ARG_RAW);
  let copied: Vec<(String, Vec<u8>)> = ZERO_ARG_EVENTS
      .iter()
      .map(|(e, p)| ((*e).to_owned(), p.as_bytes().to_vec()))
      .collect();
  assert_eq!(captured.len(), 6);
  assert_eq!(copied, captured);
  ```
  Add a second assertion: `decode_frames` of the script's concatenated chunks equals `captured` too. Doc comment: CAPTURED, `https://bedrock-runtime.eu-central-1.amazonaws.com/model/eu.anthropic.claude-haiku-5-5/converse-stream` via `curl --aws-sigv4`, inference profile `eu.anthropic.claude-haiku-5-5`, 2026-11-24 20:39:45 UTC, 1250 bytes, SHA-256 `9196618e83783e261b5a3997d81a8cb343ff58bb1c560bff611a53c0d6a98b04`, one `toolUse.input: ""` delta.
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-provider-stream-conformance --test conformance bedrock::`. Expected: FAIL.
- [ ] **Step 3: Serve the scenario.** The script sends `frame_raw(event, payload.as_bytes())` for each pair. The arm comment's "Edits made here": the prelude, header block and CRCs are encoded again by the suite, and the header order is `:message-type`, `:event-type`, `:content-type` (the capture's order is `:event-type`, `:content-type`, `:message-type`); payload bytes unchanged. `request()`, `fixture_calls()`, `fixture_finish_reason()` per Global Constraints; extend `scan_finds_only_the_stop_event`. Replace the `Outcome::Uncaptured` return.
- [ ] **Step 4: Remove the row and the mock entries** listed under Files.
- [ ] **Step 5: Run** the full suite command. Expected: PASS.
- [ ] **Step 6: Mutations** (revert each): (a) `"input":""` → `"input":"{\"x\":1}"` in the `contentBlockDelta` payload → `ToolCallsMismatch` and guard failure; (b) one byte in `metadata` → guard failure; (c) drop the `contentBlockDelta` frame from the script only → report the exact failure.
- [ ] **Step 7: Update the bedrock docs** (spec §6.5): the module doc (lines 51-90: "HAND-AUTHORED … No helper here is a capture", "the repo holds no captured Bedrock traffic", "Why there are no `.bin` fixture files", "No capture records the order") and lines 116, 171-172, 188-192 (the `metadata` after `messageStop` order is now recorded by this capture; the hand-authored scripts of the other scenarios do not change).
- [ ] **Step 8: Commit** `test(providers): SMA-860 serve the captured zero-argument call for bedrock`.

### Task 7: Close the table, fix the remaining docs, run every gate

**Files:**
- Modify: `tests/provider-stream-conformance/src/declines.rs`
- Modify: `tests/provider-stream-conformance/src/lib.rs`
- Modify: `tests/provider-stream-conformance/tests/conformance.rs` (`openai_chat` doc)
- Modify: `.gitattributes`

- [ ] **Step 1:** Remove `Scenario::ZeroArgToolCallCleanStop` from `UNCAPTURED_SCENARIOS` and the now-empty comment block above the removed rows. Confirm that `ZERO_ARG_UNCAPTURED` is gone (Task 6 deletes it).
- [ ] **Step 2:** `lib.rs` lines 67-68: the variant doc becomes "One complete tool call with empty arguments (blank or `{}`), then a tool-use stop reason."
- [ ] **Step 3:** `openai_chat` doc, lines 672-673 ("No capture of `api.openai.com` Chat Completions is recorded"), 681-684 and 797-799 ("No recorded capture sends that key"): the new capture is a recorded Chat Completions stream and sends `"finish_reason":null`. Correct each sentence. Do not change any other scenario's script.
- [ ] **Step 4:** `.gitattributes`: rewrite the two "Presently inert" comments (lines about `fixtures/**/*.txt` and `fixtures/bedrock/**`). The directory now holds `.raw` captures, which the `*.raw binary` rule covers.
- [ ] **Step 5:** `git grep -n "SMA-860\|only \`openai/responses\`\|ZERO_ARG_UNCAPTURED"` in the worktree. Each remaining hit must be true. Expected hits: the spec, the plan, and the provenance doc comments.
- [ ] **Step 6: Run every gate** from `CLAUDE.md` and report each result: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-features --all-targets -- -D warnings`; `cargo test --workspace --all-features`; `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`; `npx markdownlint-cli2`; `shasum -a 256 tests/provider-stream-conformance/fixtures/*/zero_arg_tool_call.raw` (must equal spec §4).
- [ ] **Step 7: Commit** `test(providers): SMA-860 close the zero-argument rows and correct the stale docs`.
