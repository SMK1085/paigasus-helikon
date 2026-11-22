# SMA-840: send Anthropic thinking blocks back in a tool loop

Linear: SMA-840. Builds on the SMA-827 provider-data carrier
(`docs/superpowers/specs/2026-11-20-sma-827-gemini-thought-signatures-design.md`, sections
"Core types" and the rollback note).

## Intent

With extended thinking, an Anthropic response can hold `thinking` blocks (with a signature) and
`redacted_thinking` blocks. Anthropic's documentation tells callers to send these blocks back,
unchanged, in the next request of a tool loop, so that the model keeps its reasoning. Today the
SDK drops them:

- The stream drops `signature_delta` (`crates/paigasus-helikon-providers-anthropic/src/stream.rs`,
  "signature_delta dropped (round-trip not yet supported)").
- The stream has no type for a `redacted_thinking` block. `model.rs` skips the unparseable
  `content_block_start` event with one warning for each block, so the block is lost.
- The request translator drops `ContentPart::Reasoning` with a warning, and it drops every
  `ContentPart::ProviderData` part.

The probe below shows the effect: Anthropic does not reject the request, but when the first
assistant message of a tool loop has no thinking block, the API turns thinking off for that request
without an error. So today the model stops thinking after its first tool call.

Sven decided (2026-11-21) to build the full round trip.

## Probe results (2026-11-21)

Three live runs against the Anthropic Messages API. The script is committed at
`docs/superpowers/probes/2026-11-21-sma-840-anthropic-thinking-probe.sh` (it reads
`ANTHROPIC_API_KEY` and prints no key and no signature). The raw captures hold real signatures, so
they are not in the repo.

- Run A: `claude-sonnet-5-5`, `thinking: {"type":"adaptive"}`, `output_config.effort: "high"`,
  beta `interleaved-thinking-2025-05-14`.
- Run B: `claude-sonnet-4-5`, `thinking: {"type":"enabled","budget_tokens":2048}`, same beta.
- Run C: `claude-sonnet-4-5` and `claude-opus-4-5`, `enabled` with budget 2048, **no beta header**
  (the SDK default).

Wire format:

- A `thinking` block starts with `content_block_start`
  `{"type":"thinking","thinking":"","signature":""}`. Then `thinking_delta` events, then exactly one
  `signature_delta`, then `content_block_stop`.
- On `claude-sonnet-5-5` the thinking text is empty and only the signature arrives (988 to 1180
  characters). On the 4.5 models the text arrives (106 to 256 characters) with a signature (528 to
  732 characters).
- A `redacted_thinking` block arrives whole in `content_block_start`:
  `{"type":"redacted_thinking","data":"…"}`, with no delta, then `content_block_stop`. Runs B and C
  produced 14 or 15 of them in one response (384 to 440 characters each), with the documented test
  trigger string in the prompt.
- A `tool_use` block start now also has a `caller` key. The SDK ignores it.
- The observed order was always thinking (or redacted) blocks, then `text`, then `tool_use`. No
  response had interleaved thinking inside one response.
- `claude-sonnet-5-5` rejects `{"type":"enabled"}` with HTTP 400 ("\"thinking.type.enabled\" is not
  supported for this model. Use \"thinking.type.adaptive\" and \"output_config.effort\""). With
  adaptive thinking it thought only on some prompts.
- `claude-3-5-haiku-latest` returns HTTP 404: the 3.x models are retired.

Requests (thinking on unless the row says otherwise). "Thinks" means the response has a thinking
block. Input tokens in brackets.

| Shape | Run A (5.5, beta) | Run B (4.5, beta) | Run C sonnet-4-5 / opus-4-5 (no beta) |
|---|---|---|---|
| Turn 2, blocks sent back | 200 (717) | 200, thinks (1016) | 200 (886 / 868) |
| Turn 2, thinking blocks removed | 200 (660) | 200, **does not think** (764) | 200 (785 / 778) |
| Turn 2, blocks removed, thinking off | 200 | 200 | 200 |
| Turn 2, blocks kept, thinking off | 200 | 200 | 200 |
| One signature changed | 400 "Invalid `signature` in `thinking` block" | 400 | 400 / 400 |
| `tool_use` before the thinking block | 200 | 200 | 200 |
| Thinking text changed | 200 | 200, thinks | 200 |
| Same blocks to a Haiku model | 200 | 200 | 200 |
| Turn 3, all blocks | 200 | 200, thinks (1167) | 200 (1005 / 986) |
| Turn 3, round-1 thinking removed | 200 | 200, **does not think** (871) | 200 (904 / 896) |
| Turn 3, round-2 thinking removed (the split shape) | 200 | 200, thinks (1123) | 200 (1005 / 986) |
| Redacted blocks sent back | (none produced) | 200 | 200 / 200 |
| Assistant message with only thinking, then user text | - | - | 200 / 200 |

What the probe shows:

1. Anthropic accepts every shape that this design or `main` can send, with and without the beta
   header. No request needs the `thinking` setting removed, so the design drops the rule that Sven
   chose before the probe ("remove `thinking` when the current tool turn has no signed block").
2. When the first assistant message of the tool loop has no thinking block, the API turns thinking
   off for that request with no error (run B, both "removed" rows). With the blocks sent back and the
   interleaved beta, the model thinks again after a tool result. That is the user gain.
3. Without the interleaved beta, the model does not think again inside the tool loop, with or
   without the blocks. The blocks still go into the context: they add about 100 input tokens for
   each round on run C, and about 250 to 300 on run B.
4. Anthropic checks the signature. It does not check the block order or the thinking text. A block
   from one Claude 4.x model is accepted by another.
5. Not tested: thinking blocks on an earlier user turn's final answer (the run C final answer had
   no thinking), and whether a signature is bound to an organization or an API key.

## Goals

1. The stream keeps each `thinking` block (text and signature) and each `redacted_thinking` block
   whole, in the SMA-827 carrier, with its position in the response.
2. The request translator sends the blocks of each assistant message back unchanged, in their
   original order relative to the `tool_use` blocks and the first `text` block of the same
   response.
3. Interleaved thinking (a thinking block after a `tool_use` block in the same response) keeps its
   order. The probe did not produce it, so unit tests pin it with a synthetic fixture.
4. A response with no thinking block that is sent back writes no provider data. Its items, its
   session record and its next request stay byte-identical to `main`.
5. A history with no Anthropic provider data (old sessions, another provider) gives the same
   request as `main`.

## Non-goals

- Bedrock. It drops Claude reasoning signatures and redacted content in the same way
  (`crates/paigasus-helikon-providers-bedrock/src/stream.rs`). A separate ticket.
- A setting for `output_config.effort`, an automatic interleaved-thinking beta header, or a change
  to the `ExtendedThinking` options.
- A model, API or key check on a block before it goes back. Probe finding 4 shows a cross-model
  block is accepted. A block that Anthropic rejects gives the API error unchanged; the README
  documents how a host removes the blocks (below).
- Exact text order when one response has more than one `text` block. Core joins all text of a
  turn into one `Text` part, and the stream records only the first text position. So
  `thinking, text A, tool_use, thinking, text B, tool_use` goes back as
  `thinking, text AB, tool_use, thinking, tool_use`. Probe finding 4 shows the API does not check
  order.
- A change to core. Core keeps one `Reasoning` part per message for display. The request translator
  does not read it.

## Anti-goals

- Provider data on responses with no thinking. The SMA-827 rollback note says that an older core
  fails on the `provider_data` variant.
- A changed request for a history that has no Anthropic provider data.
- A failure or warning because of another provider's provider data.
- A new failure on a shape that `main` sends today and the API accepts (hand-off transcripts,
  resumed sessions, SMA-834 joined rounds, the drop-induced prefill error).

## Approaches

Sven picked approach A on 2026-11-21.

- **A. One provider-data part for each block (picked).** At the stop of each block, the stream
  sends one `ProviderData { provider: "anthropic" }`. Thinking blocks carry the whole block. The
  first `text` block and each `tool_use` block carry only their position. The request translator
  sorts by position. This is the Gemini pattern, it ties each `tool_use` position to its call
  through `call_id`, and it agrees with conformance assertion 8.
- **B. One part for each response**, sent at `message_stop`, with the ordered block list and
  `tool_use` ids as references. Simpler to rebuild, but nothing is kept if the stream stops early,
  and the references are not tied to their calls through `call_id`.
- **Rejected: thinking first, then text, then `tool_use`.** It loses interleaved order.

## Design

### Carrier key and data shapes

- A new `pub const PROVIDER_DATA_KEY: &str = "anthropic";` in
  `crates/paigasus-helikon-providers-anthropic/src/lib.rs`. It is a wire-format key, separate from
  `PROVIDER_ID` (the same string today). Its rustdoc says that a host may filter parts by this key,
  but the `data` shape is private to the crate and can change.
- The crate reads and writes `data` through one private serde type with a `kind` tag:
  - `{"kind":"block","index":n,"block":{"type":"thinking","thinking":"…","signature":"…"}}`, `call_id: None`;
  - `{"kind":"block","index":n,"block":{"type":"redacted_thinking","data":"…"}}`, `call_id: None`;
  - `{"kind":"text","index":n}`, `call_id: None`, for the first text block of the response only;
  - `{"kind":"tool_use","index":n}`, `call_id: Some(id)`, where `id` is the `tool_use` id.
- `index` is the Anthropic content-block `index` of the response.

### Stream (`sse.rs`, `stream.rs`)

- `ContentBlockHead::Thinking` gets `thinking` and `signature` fields with `#[serde(default)]`;
  their values start the block state. A new `ContentBlockHead::RedactedThinking { data: String }`
  (`"redacted_thinking"`). Remove `#[allow(dead_code)]` on the signature field and on
  `ContentBlockStop::index`.
- `BlockState::Thinking` keeps the joined text and the signature. `thinking_delta` still emits
  `ReasoningDelta`. `signature_delta` appends to the signature. `BlockState::RedactedThinking`
  keeps `data`.
- On `content_block_stop` for index `n`, the stream removes the block state (a second stop for the
  same index only writes a debug log), then:
  - thinking with a non-empty signature: emit the `block` part; a thinking block with no signature
    is not kept (debug log, index and type only);
  - redacted thinking with non-empty data: emit the `block` part;
  - the first text block: record a `text` position;
  - `tool_use`: record a `tool_use` position, but only for a call that the stream emitted as a
    `ToolCallDelta` (`name_emitted`), and never for the synthesized structured-output tool
    (`synthesized_tool_index`).
- Position records wait until the stream emits the response's first `block` part. Then the stream
  emits all waiting records, and after that each new position at its block stop. If the response
  emits no `block` part, it emits no position record (Goal 4). Provider-data order has no meaning,
  because each part carries its index.
- Log lines hold only the index and the block type, never block content.

### Core

No change. The accumulator stores provider data in arrival order in the turn's `AssistantMessage`,
after `Reasoning` and `Text`. Tool calls stay `Item::ToolCall` items sorted by call id. The token
estimate counts the `Reasoning` text and the serialized `data` (which holds the text again and the
signature). So the estimate is high on models that strip earlier thinking and close on models that
keep it in context; this is accepted and documented.

### Request translator (`translate/request.rs`)

Terms: a **source message** is one `Item::AssistantMessage` together with the `tool_use` blocks
that come from it: its own `ContentPart::ToolUse` parts, and the `Item::ToolCall` items that follow
it before the next `AssistantMessage` or the next user message with a part that is not a tool
result. A `ToolResult` does not end a source message (the SMA-827 Gemini rule).

1. **Position map.** For each source message, collect its parts with
   `provider == PROVIDER_DATA_KEY` that parse as the private type. Ignore other providers' parts with
   no log. Skip a part that does not parse, or a `block` that fails the checks below, with a debug
   log. A source message with no valid Anthropic part builds exactly as on `main`.
2. **Block checks.** Rebuild each block from known fields only: `thinking` needs string `thinking`
   (an empty string is valid) and a non-empty string `signature`; `redacted_thinking` needs a
   non-empty string `data`. Send only `type` and those fields.
3. **Groups.** Each wire assistant message is a list of groups, one group for each source message
   that adds blocks to it, in the order the groups are added. Inside a group, each block gets a sort
   key:
   - a `block` part: its `index`;
   - the text block: the `text` position index; with no `text` position, the key is just before
     the first `tool_use` block of the group in key order, or after every block when the group has
     no `tool_use`;
   - a `tool_use` block: the index of the `tool_use` position part with the same `call_id`, taken
     and removed in order, so that a repeated id in one source message stays correct; with no
     position, it goes after every keyed block, in today's order.
   A position part whose `tool_use` is gone (trimmed history) is ignored.
4. **Sort.** Sort each group by key (stable), and never compare keys of different groups. Do the
   sort in one pass at the end of `translate_messages`, before `apply_cache_strategy`. A `tool_use`
   that `close_open_transfers` or another split moves into a different wire message loses its key
   and goes after the keyed blocks of its new message.
5. **Thinking-only messages.** A wire assistant message that, after all its `tool_use` blocks
   join, has no `text` and no `tool_use` block drops its thinking blocks. It then counts as empty,
   exactly as on `main` (no flush, not sent, the drop-induced prefill flag unchanged). The decision
   looks ahead over the items, because the `tool_use` blocks come from later items. This keeps
   hand-off transcripts (`thread_handoff_transcript` removes tool calls and keeps assistant
   messages), `Length` finishes after a thinking block, and
   `trailing_blank_user_then_reasoning_assistant_sets_flag` as on `main`.
6. **SMA-834 join rule.** An assistant message joins an open round when every block is
   `tool_use`, `thinking` or `redacted_thinking`, the pending results are not empty, and the round
   is open. This covers the nested form (`[ProviderData thinking, ToolUse]`) and the form that the
   accumulator writes (`AssistantMessage[Reasoning, ProviderData]` then `Item::ToolCall`).
7. **Reasoning.** `ContentPart::Reasoning` is never sent. Its drop logs at debug level, not warn.
8. **Cache.** `apply_cache_strategy` never puts `cache_control` on a `thinking` or
   `redacted_thinking` block. If the last block is one of them, it uses the last other block of
   that message, or no breakpoint.
9. **Which turns.** Every assistant message in the history (see "Which turns send blocks back"). The `thinking` setting of the request is not
   changed by the history.

### Conformance suite

Add an Anthropic scenario with a thinking block and a `tool_use` block, transcribed from the
`thinking_text_tool_use.txt` fixture under the suite's provenance rule. Other subjects decline it.
Assertion 8 then checks the Anthropic `tool_use` positions.

## Which turns send blocks back

Sven decided (2026-11-21): **all assistant messages in the history** send their blocks back. This
follows Anthropic's advice to send the history unchanged, and the API decides what to keep. The
request prefix stays the same as the history grows, which keeps the prompt cache valid. The other
choice (only the current tool loop) was rejected: it changes the request prefix at each new user
message.

## Recovery and privacy (README)

- One rejected block makes each later request of that session fail. A host recovers by removing
  every part with `provider == PROVIDER_DATA_KEY` from the stored history; the next request is then
  the `main` request.
- The thinking text is stored two times: in the `Reasoning` part and in the `block` data. A host
  that removes `Reasoning` before storage must also remove these parts.

## Tests

- Fixtures, transcribed from the probe captures with fake signatures and fake redacted data, LF line
  endings (the existing `tests/fixtures/*.txt` rule):
  - `thinking_text_tool_use.txt`: thinking (text and signature), text, `tool_use` (run C turn 1);
  - `redacted_thinking_tool_use.txt`: two `redacted_thinking` blocks, text, `tool_use`;
  - `empty_thinking_tool_use.txt`: thinking with empty text and a signature, text, `tool_use`
    (run A);
  - `interleaved_thinking.txt` (synthetic, marked as such in a comment): thinking, `tool_use`,
    thinking, `tool_use`;
  - `unsigned_thinking_tool_use.txt` (synthetic): a thinking block with no `signature_delta`, then
    `tool_use`.
- Stream unit tests: the provider-data events for each fixture; `ReasoningDelta` still emitted; no
  provider data for the unsigned fixture and for the fixtures without thinking
  (`tool_use_then_continuation.txt`, `parallel_tool_use.txt`); the existing `thinking_then_text.txt`
  now emits a `block` part and a `text` position; a `tool_use` with no `input_json_delta` gives no
  position; a repeated `content_block_stop` emits nothing new.
- Round-trip tests from each fixture: stream translator, then `ModelTurnAccumulator::finish`, then
  `translate_messages` / `build_body`. The assistant content equals the original wire blocks in the
  original order, with the text rule above.
- A session round trip, as in SMA-827: `SessionRecorder`, then `project()`, then the translator.
- Request tests: a history with no Anthropic provider data gives the same JSON as `main`; foreign
  provider data is ignored; a malformed part and a failed block check are skipped; a repeated
  `tool_use` id; an orphan position part; the SMA-834 parity test with thinking in both forms; a
  hand-off transcript (thinking-only message); `reasoning_only_assistant_is_not_sent` and
  `trailing_blank_user_then_reasoning_assistant_sets_flag` with Anthropic blocks added; the
  `close_open_transfers` split; `cache_control` with `CacheStrategy::LastTurn` never on a thinking
  block.
- The structured-output guard (`synthesized_tool_index`) is tested with a synthetic fixture only.
  Anthropic rejects a forced `tool_choice` with thinking on, so this path is not live.
- Conformance suite: the new Anthropic scenario passes.
- Live tests, in `src/` under `#[cfg(test)] #[ignore]` so that they can call `build_body`, with
  `ANTHROPIC_API_KEY`, the SDK default headers, `claude-sonnet-4-5` (the model id can be changed
  with an environment variable), `ExtendedThinking::Enabled { budget_tokens: 2048 }` and
  `max_output_tokens` of 4096 or more:
  1. A two-turn loop through the model. The prompt asks for the tool call; if turn 1 has no tool
     call or no thinking `block` part, the test fails with a clear message. It asserts that the
     turn-2 body holds the turn-1 signature, and that turn 2 ends with `Finish` and no error.
  2. An `LlmAgent` loop with a real tool and a recording `Model` wrapper that keeps each
     `ModelRequest`. It asserts that the second request, through `build_body`, holds the signature,
     and that the run completes.
  3. A redacted round trip with the documented trigger string. It asserts that the turn-2 body
     holds every redacted `data` value, and that turn 2 has no error.
  4. Test 1 again with `.beta("interleaved-thinking-2025-05-14")`. It asserts that turn 2 has a
     thinking block again. (Run B saw this once; if it fails by chance, the test message says so.)

## Docs and release

- Crate rustdoc for `PROVIDER_DATA_KEY` and a short crate-docs section; the Anthropic `README.md`
  (the round trip, the interleaved beta for 4.x `Enabled` models, `claude-sonnet-5-5` needs
  `ExtendedThinking::Adaptive`, the cost, recovery and privacy notes); `settings.rs` docs (the
  `claude-sonnet-5-5` line).
- The book: `docs/book/src/concepts/model-providers.md`; `core-primitives.md` (it names only the
  Gemini key); `sessions.md` near line 276 (it gives Gemini as the only reason for provider data);
  `agent-loop.md` (on `claude-sonnet-5-5`, a tool-only turn now gives a `MessageOutput` that holds
  only provider data, because its thinking text is empty).
- Additive change: the commit type is `fix(providers)`. Release-plz does the version bump.
- PR rollback note: core does not change, so a rollback of only the Anthropic crate is safe (the old
  translator drops the parts and sends the `main` request). A rollback of core below the SMA-827
  version stays unsafe, and more sessions are affected than with Gemini, because every Anthropic
  turn with a kept thinking block writes the variant, text-only turns too.
- The commit body states the cost: sending the blocks back adds input tokens.

## Left to the builder

- Where the waiting position records live in `MessageTranslator`, and helper names.
- The data structure for the groups in `translate_messages`.
- The environment variable name for the live-test model id.

## Prefix check (added after the local review)

### Facts

Two live probes on 2026-11-21 and the Claude API reference gave these facts.

- Newer models run a history-editing check, also called the prefix check. These models are Sonnet 5.5, Opus 5.5, Haiku 5.5, and Fable 5.1 and 5.2.
- A signature of a thinking block records the top-level `system` text, the `tools` set, and every message before the block. It also records a chain to the previous thinking block. A client can remove blocks from the front of the history. A client cannot remove blocks from the middle.
- Accounts made on or after 2026-08-31 get an HTTP 400 when the prefix changed. The message is `messages.1.content.0: Invalid signature in thinking block. The block is bound to a different conversation.`
- The request field `thinking.block_binding.prefix_mismatch_behavior: "drop_block"` needs the beta header `thinking-binding-controls-2026-08-01`. The API then drops the first mismatched block and every later thinking block. The request proceeds. The response has a top-level `input_transformations` array.
- The API accepts `block_binding` only with `thinking.type: "adaptive"`. No model accepts it with `"enabled"`. Models that do not run the check also accept the object. A request with `block_binding` and no beta header gets a 400.
- `claude-sonnet-4-5` is deprecated and retires on 2026-11-30. `Enabled { budget_tokens }` works on models before 4.6, for example `claude-haiku-4-5`. It works, deprecated, on Opus 4.6 and Sonnet 4.6. Opus 4.7, 4.8, 5 and 5.5, Sonnet 5 and 5.5, Haiku 5.5 and Fable reject it with a 400.

### Probe result (claude-sonnet-5-5, drop_block)

| Change before the second request | Result |
| --- | --- |
| Changed `system` | thinking block dropped |
| Added a tool | thinking block dropped |
| Changed a tool description | thinking block dropped |
| Changed the text of the first user message | thinking block dropped |
| Inserted a text block before the thinking block in the same message | thinking block dropped |
| Edited or removed text after the thinking block in the same message | thinking block kept |
| Unchanged tool loop | thinking block kept |

### Decision

Sven chose Option A. With `ExtendedThinking::Adaptive`, the provider sends `block_binding` with `prefix_mismatch_behavior: "drop_block"` and the beta header `thinking-binding-controls-2026-08-01`. The provider adds the header once, also when the user added it with `.beta(...)`. The provider sets extended thinking on the builder only, so the header follows the builder value. `Enabled` and `Disabled` send no binding.

Sven rejected two other options. A prefix hash is complex and needs a byte-exact copy of the request prefix. Documentation only leaves the 400 error in place for hand-offs.

With `Adaptive`, a changed prefix costs the model its earlier reasoning. The request does not fail.

### Addendum: effort setting

The live tests on `claude-sonnet-5-5` with `ExtendedThinking::Adaptive` failed 3 of 6, because turn 1 had no thinking block. This model has the default effort `medium`, and at `medium` it can skip thinking on a short task. Probes with `output_config: { effort: "high" }` got thinking every time. Sven chose to add an `effort` builder option (`Effort`) in this PR. The default sends no effort.
