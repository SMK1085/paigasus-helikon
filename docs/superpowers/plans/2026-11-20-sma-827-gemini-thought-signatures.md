# SMA-827 Gemini thought signatures Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Gemini 3 tool loops work: the stream keeps each `thoughtSignature`, core carries it in a
new provider-data part, and the request sends it back on the same `functionCall` part, in Gemini's
order, with a dummy fallback for histories with no valid signature.

**Architecture:** Two additive core variants (`ModelEvent::ProviderData`, `ContentPart::ProviderData`)
and one helper (`Item::is_provider_data_only`). The accumulator puts provider data into the turn's
`AssistantMessage`. Text consumers skip a provider-data-only message. Other translators drop the part
silently. The Gemini translators write and read it.

**Tech Stack:** Rust 2021 workspace (MSRV 1.94), serde / serde_json, insta, tokio, bash + curl + jq
for the probe.

**Spec:** `docs/superpowers/specs/2026-11-20-sma-827-gemini-thought-signatures-design.md`

## Global Constraints

- No breaking change in core. Only new variants on `#[non_exhaustive]` enums and a new method.
- No hand version bump in any `Cargo.toml`, no hand CHANGELOG edit (release-plz does it).
- A translator ignores a foreign `ProviderData` part **without a warning** and never fails on one.
- No change of behavior for a turn **without** provider data, for every provider.
- No real thought signature in the repo or the PR body. Fixtures use placeholders such as
  `"SIG_A"`.
- Commit format `<type>(<scope>): SMA-827 <lowercase message>`. Allowed scopes include `core`,
  `cli`, `providers`, `runtime-agentcore`, `runtime-axum`, `runtime-actix`, `docs`, `plan`, `spec`.
  There is no `providers-gemini` scope: use `providers`.
- Every commit ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Before each commit: `cargo fmt --all` and
  `cargo clippy --workspace --all-features --all-targets -- -D warnings` are clean.
- A new `pub` item gets a `///` doc comment (the `missing_docs` lint and the `-D warnings` docs gate).
  A `pub` doc comment must not intra-doc-link a private item.
- Work synchronously in the foreground. Do not end a task until each command shows its final status.
- Do not run any git command that moves HEAD or switches branches. Work only in the worktree
  `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-827`. Stage files by explicit
  path, never `git add -A` (`.env` is not ignored).

## Review Focus

1. **Three or more parallel calls with native ids that sort differently from the stream order** —
   the request must send them in `index` order with the signature on the call that Gemini signed
   (Task 9 test `parallel_calls_restore_stream_order`).
2. **The same `call_id` in two sequential rounds** (`fc_0` twice, synthesized ids) — each signature
   stays in its own round (Task 9 test `repeated_call_id_keeps_signature_per_round`).
3. **A session written before this change, replayed on Gemini 3** — the current-turn calls get the
   dummy, earlier turns are unchanged (Task 9 test `unsigned_history_gets_dummy_in_current_turn_only`).
4. **A Gemini history replayed on OpenAI Chat / LiteLLM** — no empty assistant or user message, tool
   calls still in one assistant message (Task 5 tests).
5. **A run that stops with `RunDeferred` right after a Gemini tool-only turn** — the final output
   keeps the earlier text (Task 3 test `deferred_after_provider_data_only_turn_keeps_text`).

---

### Task 1: Core variants, helper and serde

> **Superseded (2026-11-20):** after probe capture 6, Sven decided that `part_chars` counts the serialized length of `data`, not 0.

**Files:**
- Modify: `crates/paigasus-helikon-core/src/item.rs` (enum `ContentPart`, `impl Item`)
- Modify: `crates/paigasus-helikon-core/src/model.rs` (enum `ModelEvent`, around line 172)
- Modify: `crates/paigasus-helikon-core/src/token_counter.rs:41-47`
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (event forwarding match, around line 1475)
- Test: `crates/paigasus-helikon-core/tests/serde_roundtrip.rs`, unit tests in `item.rs`

**Interfaces:**
- Produces:
  - `ContentPart::ProviderData { provider: String, call_id: Option<String>, data: serde_json::Value }`
    with `#[serde(default, skip_serializing_if = "Option::is_none")]` on `call_id`.
  - `ModelEvent::ProviderData { provider: String, call_id: Option<String>, data: serde_json::Value }`.
  - `impl Item { pub fn is_provider_data_only(&self) -> bool }` — true only for an
    `AssistantMessage` with non-empty content where every part is `ProviderData`.

- [ ] **Step 1: Write the failing tests**
  - `serde_roundtrip.rs`: `content_part_provider_data_roundtrip` — two cases, with
    `call_id: Some("call_1")` and with `None`; assert the JSON is
    `{"type":"provider_data","provider":"gemini","call_id":"call_1","data":{"index":0}}` (no
    `call_id` key in the `None` case), assert it deserializes back equal, and add an insta snapshot.
  - `item.rs` unit test `is_provider_data_only_cases`: `false` for an empty-content assistant
    message, `true` for `[ProviderData]`, `false` for `[Text, ProviderData]`, `false` for
    `[Reasoning]`, `false` for a `UserMessage` with `[ProviderData]`.
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-core --test serde_roundtrip content_part_provider_data`
  — expected: FAIL (no variant).
- [ ] **Step 3: Add the variants and the helper.** The rustdoc of both variants states the full
  contract of the spec section "Core types": wire-format key (not `Model::provider()`), ignore
  foreign parts silently, valid only in an `AssistantMessage`, `call_id` meaning, position has no
  meaning, the rollback note. `part_chars` returns 0 for `ProviderData`. The forwarding match in
  `agent.rs` gets `crate::ModelEvent::ProviderData { .. } => {}`. Add an arm to the accumulator
  `observe` that does nothing yet (Task 2 fills it), so the crate compiles.
- [ ] **Step 4: Run** `cargo test -p paigasus-helikon-core` and
  `cargo build --workspace --all-features` — expected: PASS. Accept the new snapshot with
  `cargo insta accept` after you read it. No existing snapshot changes.
- [ ] **Step 5: Commit** `feat(core): SMA-827 add provider data variants to the model event and content part`

### Task 2: Accumulator keeps provider data

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs` (`ModelTurnAccumulator`, `build_items`,
  rustdoc of `ModelTurn::items`)
- Test: `model_turn_tests` in the same file

**Interfaces:**
- Consumes: Task 1 variants.
- Produces: a `ModelTurn` whose items are `[AssistantMessage{Reasoning?, Text?, ProviderData…}, ToolCall…]`.

- [ ] **Step 1: Write the failing tests**
  - `tool_only_turn_with_provider_data_gets_assistant_message`: events
    `ToolCallDelta{call_id:"b",name:Some("t"),args_delta:"{}"}`,
    `ProviderData{provider:"gemini",call_id:Some("b"),data:{"index":0}}`, `Finish{ToolCalls}`; assert
    items are `[AssistantMessage{content:[ProviderData{..same..}], agent:Some("a1")}, ToolCall{call_id:"b",..}]`.
  - `provider_data_follows_reasoning_and_text`: reasoning `"r"`, text `"t"`, two `ProviderData`
    events P1 then P2; assert content is `[Reasoning{"r"}, Text{"t"}, P1, P2]`.
  - `no_provider_data_items_unchanged`: the existing `accumulates_text_reasoning_and_tool_calls`
    case gives the same items as before (keep that test unchanged and green).
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-core model_turn_tests` — expected: FAIL.
- [ ] **Step 3: Implement.** New field `provider_data: Vec<crate::ContentPart>`; `observe` pushes a
  `ContentPart::ProviderData` per event; `build_items` takes it as a fourth input, makes the message
  when text, reasoning or provider data is non-empty, and appends the parts after `Text`.
- [ ] **Step 4: Run** `cargo test -p paigasus-helikon-core` — expected: PASS.
- [ ] **Step 5: Commit** `feat(core): SMA-827 keep provider data in the accumulated model turn`

### Task 3: Core text consumers skip a provider-data-only message

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs` (`collect` around line 434,
  `collect_typed` around line 513)
- Modify: `crates/paigasus-helikon-core/src/workflow.rs:32-46` (`assistant_text`)
- Test: a new `crates/paigasus-helikon-core/tests/provider_data_text_guard.rs` (uses
  `tests/common` `MockModel::with_scripts`, `MockTool`)

**Interfaces:**
- Consumes: `Item::is_provider_data_only`, Task 2 accumulator.
- Produces: the rule "a provider-data-only message is invisible to text consumers".

- [ ] **Step 1: Write the failing tests.** Build two scripts that differ only by `ProviderData`
  events in the tool-only turn: turn 1 = text `"thinking aloud"` + nothing else (a text turn), turn 2
  = one tool call (+ `ProviderData` in variant B), turn 3 = text `"done"`.
  - `collect_output_identical_with_and_without_provider_data`: `run(...).collect()` final output of
    A equals B and equals `"done"`.
  - `collect_typed_identical_with_and_without_provider_data`: same with an `output_type` agent
    whose last turn returns `{"ok":true}`; typed outputs are equal.
  - `deferred_after_provider_data_only_turn_keeps_text`: turn 1 text `"partial"`, turn 2 a call to
    a deferred tool (use `common::DeferB`) with `ProviderData`; the run stops with `RunDeferred`;
    `collect()` output is `"partial"` in both variants.
  - `workflow_state_identical_with_and_without_provider_data`: a `SequentialAgent` with one
    `LlmAgent` step on each script; the step's state value is equal in A and B.
  - `session_records_provider_data_before_tool_call`: run variant B with an in-memory session; the
    session log has `AssistantMessage{content:[ProviderData..]}` before `ToolCalled{call_id}`, and
    `project()` returns the `AssistantMessage` before the `Item::ToolCall`.
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-core --test provider_data_text_guard` —
  expected: the collect, deferred and workflow tests FAIL; the session test may pass already.
- [ ] **Step 3: Implement.** In both `runner.rs` arms, skip the event when
  `item.is_provider_data_only()` (no clear, no append). `assistant_text` returns `None` for a
  provider-data-only message, so its callers in `workflow.rs` (lines 208, 414, 591) and `graph.rs`
  (line 297) skip it. Search core for other `MessageOutput` or `AssistantMessage` text readers
  (`rg -n "MessageOutput" crates/paigasus-helikon-core/src`) and apply the same rule; list each one
  in the commit body.
- [ ] **Step 4: Run** `cargo test -p paigasus-helikon-core` — expected: PASS.
- [ ] **Step 5: Commit** `fix(core): SMA-827 keep text output unchanged when a turn carries only provider data`.
  The commit body (release-plz copies it into the CHANGELOG) names the behavior change — code that
  renders each `MessageOutput` now also gets a provider-data-only message on a Gemini 3 tool turn —
  and the rollback note from the spec.

### Task 4: Text consumers outside core skip a provider-data-only message

**Files:**
- Modify: `crates/paigasus-helikon-cli/src/repl.rs:221`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/agui/map.rs:366-392`
- Modify: `crates/paigasus-helikon-runtime-axum/src/dto.rs:143-156`,
  `crates/paigasus-helikon-runtime-actix/src/dto.rs:144` (same shape)
- Test: unit tests next to each changed function

**Interfaces:**
- Consumes: `Item::is_provider_data_only`.

- [ ] **Step 1: Write the failing tests.**
  - agentcore `provider_data_only_message_emits_no_frames`: feed `EventMapper` the sequence
    `ToolCallDelta(name)`, `MessageOutput([ProviderData])`, `ToolCallItem`, and the same sequence
    without the `MessageOutput`; the emitted AG-UI frames are identical.
  - axum and actix `provider_data_only_message_keeps_output`: events
    `MessageOutput([Text "hi"])`, `MessageOutput([ProviderData])`, `RunCompleted`; `output` is
    `Some("hi")`.
  - CLI: if `repl.rs` has no test seam for the print loop, extract the match into
    `fn render(event: &AgentEvent, out: &mut impl Write)` and test that a provider-data-only
    `MessageOutput` writes nothing, and a text `MessageOutput` writes `"\n"`.
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-runtime-agentcore -p paigasus-helikon-runtime-axum -p paigasus-helikon-runtime-actix -p paigasus-helikon-cli`
  — expected: the agentcore and CLI tests FAIL (axum and actix may already pass; keep them as pins).
- [ ] **Step 3: Implement.** Each consumer skips the event when `item.is_provider_data_only()`. In
  `map.rs`, the check comes first, before `close_call`, and does not touch `streamed_text`. Search
  the other crates (`rg -n "MessageOutput" crates --glob '!*/tests/*'`) and apply the same rule
  where text is read; list each site in the commit body.
- [ ] **Step 4: Run** the command of Step 2 — expected: PASS.
- [ ] **Step 5: Commit** `fix(runtime): SMA-827 skip provider-data-only messages in text renderers`

### Task 5: Other provider translators drop provider data silently

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/translate/request.rs` (Chat:
  `Item::AssistantMessage` arm around line 50, `emit_user_or_hoist` around line 108; Responses:
  `to_responses_input` around lines 293-319)
- Modify: `crates/paigasus-helikon-providers-litellm/src/translate/request.rs` (lines 52, 108-117)
- Modify: `crates/paigasus-helikon-providers-bedrock/src/translate/request.rs:138-143,195-200`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs` (wildcards at
  lines 117, 202, 235 if any of them warns)
- Test: unit tests in each file; use the crate's existing `test_tracing` helper (openai, litellm)
  to assert no warning

**Interfaces:**
- Consumes: Task 1 variants, `Item::is_provider_data_only`.

- [ ] **Step 1: Write the failing tests** (OpenAI Chat and LiteLLM, the same three):
  - `provider_data_only_assistant_alone_emits_nothing`: `[User "q", AssistantMessage[PD], User "r"]`
    gives exactly two user messages.
  - `provider_data_only_assistant_then_calls_forms_one_message`:
    `[User "q", AssistantMessage[PD], ToolCall a, ToolCall b, ToolResult a, ToolResult b]` gives one
    assistant message with two `tool_calls`, then two `tool` messages — the same request JSON as
    the history without the provider-data-only message.
  - `provider_data_in_user_message_is_dropped`: `UserMessage[Text "q", PD]` gives
    `{"role":"user", ...}` with only the text; `UserMessage[PD]` gives no message.
  - Bedrock, Anthropic, OpenAI Responses: `provider_data_dropped_without_warning` — the part does
    not appear in the request and no warning is logged.
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-providers-openai -p paigasus-helikon-providers-litellm -p paigasus-helikon-providers-bedrock -p paigasus-helikon-providers-anthropic provider_data`
  — expected: FAIL.
- [ ] **Step 3: Implement.** Chat and LiteLLM: in the `AssistantMessage` arm, `continue` when
  `item.is_provider_data_only()` (pending tool calls then flush into one assistant message as they
  do today when no assistant precedes them; check this with the second test); in
  `emit_user_or_hoist`, filter `ProviderData` out of `user_parts`. Bedrock, Anthropic, Responses: an
  explicit `ContentPart::ProviderData { .. } => {}` arm before each warning wildcard.
- [ ] **Step 4: Run** the command of Step 2, then `cargo test` for the four crates — expected: PASS,
  and no existing snapshot changes.
- [ ] **Step 5: Commit** `fix(providers): SMA-827 drop foreign provider data without an empty message or a warning`

### Task 6: Conformance rule for call-scoped provider data

**Files:**
- Modify: `tests/provider-stream-conformance/src/lib.rs` (`Violation`)
- Modify: `tests/provider-stream-conformance/src/check.rs` (`classify`, after assertion 7)
- Test: the check unit tests in `check.rs` (or the crate's existing test module)

**Interfaces:**
- Produces: `Violation::ProviderDataUnknownCall { call_id: String }`.

- [ ] **Step 1: Write the failing tests:** `provider_data_with_unknown_call_id_is_a_violation`
  (a `ProviderData{call_id:Some("x")}` and no `ToolCallDelta` for `"x"`) and
  `provider_data_with_known_call_id_passes`, `provider_data_without_call_id_passes`.
- [ ] **Step 2: Run** `cargo test -p provider-stream-conformance provider_data` (use the package
  name from its `Cargo.toml`) — expected: FAIL.
- [ ] **Step 3: Implement** the rule, with a doc comment that names it "assertion 8".
- [ ] **Step 4: Run** the whole conformance test suite — expected: PASS.
- [ ] **Step 5: Commit** `test(providers): SMA-827 check that call-scoped provider data names a streamed call`

### Task 7: Live probe (gate before the Gemini code)

**Files:**
- Probe: `/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/ebc68a6c-86b8-4a5f-80af-4e374bf8b2aa/scratchpad/probe.sh`
  (written during the spec stage)
- Modify: the spec, a new section "Probe results" under "Live check"

- [ ] **Step 1:** Ask Sven to run the probe (`! GEMINI_API_KEY=… bash <probe path>`, optional
  `GEMINI_MODEL_ID_2`). The agent shell has no key. Do not read `.env`.
- [ ] **Step 2:** Read `probe-out/` and record in the spec, per capture: HTTP status, finish reason,
  `promptTokenCount`, and for capture 1 the part layout (keys, `functionCall.id` present, which part
  holds the signature, the chunk of the signature, the signature length).
- [ ] **Step 3: Stop condition.** If capture 1 differs from the spec (signature not a sibling of
  `functionCall`, or not in the same chunk), stop and bring it to Sven. If capture 4 or 5 fails, the
  `index` sort is required (keep it); if both pass, keep it anyway (harmless). If capture 3 fails,
  stop. If capture 6 `promptTokenCount` differs from capture 3 by more than 10%, bring the
  token-count rule back to Sven.
- [ ] **Step 4:** Transcribe capture 1 into a fixture
  `crates/paigasus-helikon-providers-gemini/tests/fixtures/parallel_signed.sse`, each signature
  replaced by `SIG_A`, `SIG_B`, …. Add the `.gitattributes` line
  `crates/paigasus-helikon-providers-gemini/tests/fixtures/*.sse text eol=lf`.
- [ ] **Step 5: Commit** `docs(spec): SMA-827 record the live probe results` (the spec, the fixture,
  `.gitattributes`). The probe source and the result table go into the PR body later, with no
  signature values.

### Task 8: Gemini stream translator emits provider data

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/src/sse.rs` (`Part`)
- Modify: `crates/paigasus-helikon-providers-gemini/src/stream.rs`
- Modify: `crates/paigasus-helikon-providers-gemini/src/model.rs:141` (constructor call)
- Modify: `crates/paigasus-helikon-providers-gemini/src/lib.rs` (export the key)
- Modify: `crates/paigasus-helikon-providers-gemini/src/builder.rs` (`Transport` helper)
- Test: `stream.rs` unit tests, `tests/gemini_streaming.rs` (fixture of Task 7)

**Interfaces:**
- Produces:
  - `pub const PROVIDER_DATA_KEY: &str = "gemini";` exported from the crate root, with a doc comment.
  - `impl Transport { pub(crate) fn api_name(&self) -> &'static str }` → `"developer"` / `"vertex"`.
  - `StreamTranslator::new(model_id: String, api: &'static str) -> Self`.
  - Event per `functionCall` part: `ToolCallDelta`, then
    `ProviderData { provider: PROVIDER_DATA_KEY, call_id: Some(<same id>), data }` with
    `data = {"index": i}` for an unsigned call and
    `{"index": i, "thoughtSignature": s, "model": model_id, "api": api}` for a signed call.

- [ ] **Step 1: Write the failing tests** (`stream.rs`):
  - `signed_function_call_emits_provider_data` — part `{functionCall{id:"call_1",name:"search",args:{}}, thoughtSignature:"SIG_A"}`;
    events are `ToolCallDelta{call_id:"call_1"}` then `ProviderData{provider:"gemini", call_id:Some("call_1"), data:{"index":0,"thoughtSignature":"SIG_A","model":"gemini-3.8-flash","api":"developer"}}`.
  - `unsigned_call_gets_index_only` — the second call in the same chunk gets `data:{"index":1}`.
  - `index_counts_across_chunks` — calls in two chunks get indices 0 and 1.
  - `synthesized_id_matches_provider_data` — a call without `id` gets `fc_0` in both events.
  - `text_signature_is_ignored` — `{text:"hi", thoughtSignature:"SIG_T"}` gives only `TokenDelta`.
  - `gemini_streaming.rs`: the fixture of Task 7 gives one `ProviderData` per call, with the
    signatures on the parts that capture 1 showed.
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-providers-gemini stream` — expected: FAIL.
- [ ] **Step 3: Implement.** `Part.thought_signature: Option<String>` (`#[serde(default)]`). The
  translator keeps a `call_index: usize` across chunks. `model.rs` passes
  `cfg.model_id.clone()` and `cfg.transport.api_name()`.
- [ ] **Step 4: Run** `cargo test -p paigasus-helikon-providers-gemini` — expected: PASS.
- [ ] **Step 5: Commit** `fix(providers): SMA-827 keep gemini thought signatures and call order from the stream`

### Task 9: Gemini request translator sends signatures back

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/request.rs`
  (`items_to_contents`, `assistant_parts`, `user_parts`, `merge_same_role` doc comment, a new
  post-pass after the merge)
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/mod.rs` (`build_request` passes
  `&cfg.model_id` and `cfg.transport.api_name()` to `items_to_contents`)
- Test: unit tests in `request.rs`, snapshots in `translate/mod.rs` (`snap` module)

**Interfaces:**
- Consumes: `PROVIDER_DATA_KEY`, `Transport::api_name`, the `data` shape of Task 8.
- Produces: `items_to_contents(items: &[Item], model_id: &str, api: &str) -> Result<TranslatedContents, ModelError>`;
  `pub(crate) const DUMMY_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";` with a doc
  comment that names its source (the Gemini 400 error text and the SMA-820 probe) and says that
  Google warns it can make answers worse.

Rules (from the spec, section "Gemini request translator"):
- Pending list: on each `AssistantMessage`, first read all its parts with
  `provider == PROVIDER_DATA_KEY` into `Vec<(call_id, data)>` (replace the old list). Clear it at
  the next `UserMessage` that is not only `ToolResult` parts. A `ToolResult` item does not clear it.
- Each `functionCall` takes and removes the first pending entry with its `call_id`.
- Valid signature: `data.thoughtSignature` is a string, `data.model == model_id`,
  `data.api == api`. Anything else is "no signature". Never an error.
- A valid signature goes on the part: `{"functionCall": {...}, "thoughtSignature": "..."}`.
- Order: in each merged `model` content, when every `functionCall` part has an `index`, sort the
  `functionCall` parts by `index` (stable) and keep the non-call parts in place. The index travels
  as side data, never as a wire key.
- Dummy: after the merge, find the last `user` content with no `functionResponse` part. Each later
  `model` content with `functionCall` parts and no `thoughtSignature` on any of them gets
  `DUMMY_THOUGHT_SIGNATURE` on each `functionCall` part. Earlier contents get no dummy.
- A foreign `ProviderData` part and a `ProviderData` part in `user_parts` are dropped with no
  warning.

- [ ] **Step 1: Write the failing tests** (`request.rs`; build items with a small helper
  `signed(call_id, index, sig)` that returns the `ProviderData` part for `gemini-3.8-flash` /
  `developer`):
  - `signature_goes_on_its_call` — `[User, AssistantMessage[PD(call_1,0,SIG_A)], ToolCall call_1, ToolResult call_1]`;
    the `model` part is `{"functionCall":{"id":"call_1",...},"thoughtSignature":"SIG_A"}`.
  - `parallel_calls_restore_stream_order` — calls `call_z` (index 0, `SIG_A`), `call_a` (index 1),
    `call_m` (index 2), in history in sorted order `call_a, call_m, call_z`; the `model` content
    holds them as `call_z, call_a, call_m`, with `SIG_A` on `call_z` only.
  - `repeated_call_id_keeps_signature_per_round` — two rounds, each `[AssistantMessage[PD(fc_0,0,SIG_n)], ToolCall fc_0, ToolResult fc_0]`;
    round 1 has `SIG_1`, round 2 has `SIG_2`.
  - `tool_result_between_calls_keeps_pending` — `[AssistantMessage[PD(a),PD(b)], ToolCall a, ToolResult a, ToolCall b]`;
    `b` still gets its signature.
  - `unsigned_history_gets_dummy_in_current_turn_only` — an earlier round with no PD, then a user
    text, then a current round with no PD; only the current `model` content gets the dummy, on each
    call.
  - `signature_from_other_model_or_api_gets_dummy` — two cases (`model:"gemini-3.6-flash"`,
    `api:"vertex"`); the call gets the dummy.
  - `malformed_signature_is_absent` — `thoughtSignature: 5`; the call gets the dummy and no error.
  - `mixed_content_is_sent_as_received` — one signed and one unsigned call; no dummy on the
    unsigned call.
  - `foreign_provider_data_is_ignored_silently` — `provider:"anthropic"`; no wire part, no warning
    (assert with the crate's tracing test helper if it has one, else assert the output only).
  - `provider_data_only_message_emits_no_content` — `[User, AssistantMessage[PD], User]`; two
    `user` contents merge as today and no `model` content appears.
  - `no_side_data_reaches_the_wire` — serialize the request body of the parallel test; it contains
    no `"index"` key.
- [ ] **Step 2: Run** `cargo test -p paigasus-helikon-providers-gemini translate` — expected: the
  new tests FAIL.
- [ ] **Step 3: Implement** the rules above. Update the `merge_same_role` doc comment: the boundary
  stays; the reason "the SDK sends no signatures yet (SMA-827)" is replaced by a pointer to the
  probe capture 8 result and the spec section "Left for Sven".
- [ ] **Step 4: Update existing expectations.** Run `cargo test -p paigasus-helikon-providers-gemini`;
  existing tests with a call in the current turn now get the dummy. For each failing exact-JSON test
  and each changed snapshot (`snap_tool_call_and_result`, `snap_parallel_same_name_tool_calls`, and
  others that `cargo insta test` reports), check that the only change is the dummy on the
  current-turn calls, then update it. List each updated test in the commit body.
- [ ] **Step 5: Run** `cargo test -p paigasus-helikon-providers-gemini` — expected: PASS.
- [ ] **Step 6: Commit** `fix(providers): SMA-827 send gemini thought signatures back on their calls`

### Task 10: End-to-end tests and live tests

**Files:**
- Test: `crates/paigasus-helikon-providers-gemini/src/translate/mod.rs` (a `round_trip` test
  module), `crates/paigasus-helikon-providers-gemini/src/live_round_trip.rs` (new,
  `#[cfg(test)]`, declared in `lib.rs` as `#[cfg(test)] mod live_round_trip;`),
  `crates/paigasus-helikon-providers-gemini/tests/live.rs`
- Modify: `crates/paigasus-helikon-providers-gemini/Cargo.toml` dev-dependencies only if a test
  needs `paigasus-helikon-core` features or `tokio` macros that are not there yet

**Interfaces:**
- Consumes: Tasks 2, 8 and 9.

- [ ] **Step 1: Offline round trip** `stream_to_request_round_trip`: feed the Task 7 fixture through
  `StreamTranslator`, then `ModelTurnAccumulator`, append one `ToolResult` per call, call
  `build_request`; assert each `SIG_*` is on its call, the calls are in stream order, and no dummy
  appears. Second case `session_projection_round_trip`: write the turn through the core
  `SessionRecorder`, `project()` it, then `build_request`; the same asserts.
- [ ] **Step 2: Live test 1** (`live_round_trip.rs`, `#[tokio::test] #[ignore]`): with
  `GEMINI_API_KEY` set (skip with `eprintln!` when unset), call `GeminiModel::invoke` with the
  `search` tool, accumulate turn 1, assert at least one `ProviderData { provider: "gemini",
  call_id: Some(_) }` with a non-empty `thoughtSignature`; append tool results with no user text;
  assert that `build_request` puts that exact signature on the call and no dummy appears; invoke
  turn 2 and assert `Finish` with no error.
- [ ] **Step 3: Live tests 2 and 3** (`tests/live.rs`, `#[ignore]`):
  `live_agent_two_sequential_tool_rounds` (an `LlmAgent` with a real tool whose instructions force
  two rounds, for example "call `step` with n=1, then with n=2, then answer"); assert the run
  completes and the tool ran two times. `live_agent_structured_output_with_tool` (an `LlmAgent`
  with an `output_type` and one tool); assert a typed output.
- [ ] **Step 4: Run** `cargo test -p paigasus-helikon-providers-gemini` (offline) — expected: PASS.
  Ask Sven to run `! GEMINI_API_KEY=… cargo test -p paigasus-helikon-providers-gemini -- --ignored live`
  and record the result.
- [ ] **Step 5: Commit** `test(providers): SMA-827 cover the gemini signature round trip end to end`

### Task 11: Docs

> **Superseded (2026-11-20):** after probe capture 6, Sven decided that `part_chars` counts the serialized length of `data`, not 0.

**Files:**
- Modify: `docs/book/src/concepts/core-primitives.md` (around line 35: `ContentPart::ProviderData`
  and its contract, the rollback note)
- Modify: `docs/book/src/concepts/model-providers.md` (around line 55; the Gemini limitations
  section around lines 296-300: tool loops on Gemini 3 work, Vertex signatures not live-tested,
  text-level signatures not sent)
- Modify: `docs/book/src/concepts/sessions.md` (around lines 270-274: provider data counts 0 in the
  token estimate)
- Modify: `docs/book/src/concepts/agent-loop.md` (around lines 64-69: `MessageOutput` also comes on
  a Gemini tool-only turn, with provider data only)
- Modify: `crates/paigasus-helikon-providers-gemini/README.md` (limitations, around line 89)
- Modify: `crates/paigasus-helikon-core/README.md` only if it lists the `ContentPart` variants

- [ ] **Step 1:** Write the edits in Simplified Technical English, like the rest of the book.
- [ ] **Step 2: Run** `mdbook build docs/book` and `npx markdownlint-cli2` — expected: clean.
- [ ] **Step 3: Run** the full CI gate list from `CLAUDE.md` (fmt, clippy, test, doc with
  `RUSTDOCFLAGS="-D warnings"`, doc coverage, markdownlint, the script checks) — expected: all clean.
- [ ] **Step 4: Commit** `docs(docs): SMA-827 document provider data and gemini 3 tool loops`
