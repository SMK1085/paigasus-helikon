# SMA-863: emit a `ToolCallDelta` for a tool call with no input delta (anthropic, bedrock)

Status: revised after the spec challenge, 2026-11-25. Size: a small change (two translators,
unit tests only).

## 1. Intent

The `anthropic` and `bedrock` stream translators send the tool name only with the first input
delta. A tool call that has a start event and no input delta then gives no `ToolCallDelta`. The
stream still ends with `Finish(ToolCalls)`, so the caller gets a tool-call finish with no tool
call.

Real traffic does not show this shape today. The SMA-860 captures (2026-11-24) show that both
providers send one empty input delta for a zero-argument call (anthropic: one `input_json_delta`
with `partial_json: ""`; bedrock: one `contentBlockDelta` with `toolUse.input: ""`). The
translators are correct now only because of that empty delta. This change makes them correct
without it. `openai/chat` and `litellm` (`flush_buffered_names`) and `openai/responses` (SMA-562)
already handle the no-delta shape. Their rule is wider than the safety net here: `openai/chat`
also flushes before a terminal that is not `ToolCalls`, and at end of stream with no `Finish`.
This design keeps the narrower net on purpose (§4).

## 2. Goals

1. A tool call with a start event and no input delta gives exactly one `ToolCallDelta`, with
   `name: Some(<name>)`, before the terminal event. This applies to both translators.
2. The primary emit point is the block stop: anthropic `content_block_stop`, bedrock
   `contentBlockStop`.
3. **Arguments of that delta.** Bedrock: `args_delta: ""`. Its `ToolUseBlockStart` carries only
   `tool_use_id` and `name`. Anthropic: the `content_block_start` event already carries `input`
   (`ContentBlockHead::ToolUse { input }` in `src/sse.rs`, parsed today but dropped). When
   `input` is a JSON object with at least one key, send `serde_json::to_string(&input)`.
   Otherwise (`{}`, `null`, absent) send `""`. A server that sends the whole input at the start
   and no delta then gives the real arguments, not `{}`. Core changes `""` to `{}`
   (`crates/paigasus-helikon-core/src/model.rs`, the `accum.args_str.trim().is_empty()` branch).
4. **Safety net** (agreed with Sven on 2026-11-25): if the translator is about to emit
   `Finish(ToolCalls)` and a tool block has no name sent yet, it first sends that name, with the
   same arguments rule. This covers a stream that has no block stop.
5. Each fix has unit tests. Each test doc says that no capture shows this wire shape:
   HAND-AUTHORED, derived from the SMA-860 captures with the empty delta removed (and, for the
   net tests, with the block stop removed too).

## 3. Non-goals

- No new scenario in `tests/provider-stream-conformance` (Sven chose approach A, §5).
- No change to core.
- No change for a call that already emitted its name. It gets no second `ToolCallDelta`.
- No change for the synthesized structured-output tool (`SYNTHESIZED_TOOL_NAME`). Its input goes
  out as `TokenDelta`, and it never gives a `ToolCallDelta`. **Known consequence, kept on
  purpose:** a zero-argument synthesized call streams one `""` delta on the real wire, so the
  text is empty and the structured-output parse fails for a schema that accepts `{}`. This is a
  separate defect (core or synthesis), filed as SMA-865.
- No change for a bedrock tool call refused by `tool_choice: None`. The translator records no
  block for it, so it has nothing to flush.
- Out of scope: the older bedrock paths where a second `MessageStop` after `Metadata` gives a
  second `Finish`, or a second `Metadata` gives `Usage` after `Finish`. The new
  `terminal_emitted` flag (§6.2) could close them, but that changes behaviour this issue does not
  cover. Filed as SMA-866.
- No README or mdBook edit: the public API does not change. This is a conscious call.
- No SMA-532 work. Sven chose to fold SMA-532 (bedrock doc comments misstate the Usage/Finish
  contract) into this PR, but PR #211 (SMA-533, 2026-08-22) already rewrote both comments. Only
  the sentence in §6.2 changes, for this design's own reason.

## 4. Anti-goals and the limit of the net

- A second `ToolCallDelta` with a name for one call. Each call carries its name exactly once.
- Any event after the terminal event (`Finish` or `Err`). This is the core ordering contract, and
  conformance assertion 9 (`tests/provider-stream-conformance/src/check.rs`) forbids any event
  except `Usage` after an `Err`.

**Why the net runs only before `Finish(ToolCalls)`.** The block-stop emit (Goal 2) runs under
every stop reason. That is not a new risk: it copies what the empty-delta wire already does
today. On the captured wire, a `tool_use` block that `max_tokens` cuts still has its first `""`
delta, so it already gives a named call with `""` under `Finish(Length)`. The block-stop emit
gives the same result for a stream without that delta. The net is different. It fires only for a
block that the provider never closed, which is a malformed or cut stream. To invent a call there
is justified only when the provider says that the turn ended for tool calls. So the net is
limited to `Finish(ToolCalls)`.

A related defect exists in core: the agent loop runs every `Item::ToolCall` and does not check
the finish reason, so a call under `Finish(Length)` still runs. That is out of scope here and is
filed as SMA-864.

## 5. Approaches

- **A. Unit tests in each `stream.rs` (chosen).** One hand-authored event list per case, with the
  HAND-AUTHORED provenance doc. Small and direct, and it matches the acceptance criteria.
- **B. A new `ZeroArgToolCallNoDelta` conformance scenario as well.** It covers every subject at
  the same time, but each of the five or more subjects must then script it or decline it. That is
  too large for a Low-priority issue. Rejected by Sven.

For the emit point there is one sensible way: the block stop. An emit at the block start is also
allowed by the core contract (the name is complete there). But it adds one event to every tool
call, changes every captured stream, and breaks `tool_use_emits_call_delta_with_name_only_once`
(anthropic) and `parallel_tool_calls_emit_correct_call_ids` (bedrock). The block stop changes no
captured stream, because each anthropic and bedrock capture and conformance script sends at least
one input delta. The safety net is the only extra, and Sven agreed to it.

## 6. Design

### 6.1 anthropic (`crates/paigasus-helikon-providers-anthropic/src/stream.rs`)

- `BlockState::ToolUse` keeps the start event's `input` (§2 Goal 3) and a `synthesized: bool`.
  The synthesized check at the new emit sites reads that flag, not `synthesized_tool_index`,
  because a second synthesized start overwrites the index.
- In `stop_block`, the `ToolUse` arm: if `!name_emitted` and `!synthesized`, push the
  `ToolCallDelta` and set the name as sent. Then record the `tool_use` position, as for any
  emitted call. Log a `tracing::debug!` with the index and `call_id` only.
- `stop_block` already returns early after the terminal event, so a block stop after `Finish` or
  `Err` emits nothing.
- **Position records.** SMA-840's rule is "a `tool_use` position only for a call that the stream
  emitted as a `ToolCallDelta`". After this change, such a call does emit one, so it also gets a
  position. The rule does not change. The test `tool_use_without_input_json_delta_gets_no_position`
  changes its expectation and its name to match.
- **Safety net.** In both terminal paths (the `MessageStop` arm and `finish()`), when
  `finish_or_error` gives `Ok(Finish { reason: ToolCalls })`, first emit the name for each
  non-synthesized `ToolUse` block in `blocks` with no name sent, in ascending index order. Log a
  `tracing::warn!` with the index and `call_id` only. Such a block gets no position record,
  because positions are recorded only at the block stop. In synthesis mode the net cannot fire,
  because `tool_use` never maps to `ToolCalls` there; no test is written for that unreachable
  case.

### 6.2 bedrock (`crates/paigasus-helikon-providers-bedrock/src/stream.rs`)

- Bedrock never removes block state; this does not change. A stop does not remove the block, so
  a late delta for that index is not dropped. "A block with no name sent" means a recorded
  `ToolUse` block with `name_emitted == false`.
- `BlockState::ToolUse` gets `synthesized: bool`, for the same reason as anthropic.
- **Terminal guard (mandatory).** Add `terminal_emitted: bool`. Set it when `finish_events`
  returns a non-empty result (a `Finish` or the both-tools `Err`). Also set it when a refused
  tool call emits its `Err`. The `ContentBlockStop` arm returns early when it is set.
- The `ContentBlockStop` arm stops being a no-op. For the stopped index, if the block is
  `ToolUse` with `!name_emitted` and `!synthesized`, push the `ToolCallDelta` with
  `args_delta: ""`, set `name_emitted`, and log a `tracing::debug!` (index and `call_id` only).
  Other blocks give no output, as today.
- **Safety net.** Inside `finish_events` (which then takes `&mut self`), before it returns
  `Ok(Finish { reason: ToolCalls })`, emit the name for each non-synthesized `ToolUse` block with
  no name sent, in ascending index order, and log a `tracing::warn!`. In the `Metadata` arm the
  order is then `Usage`, `ToolCallDelta`, `Finish`. The core contract allows this. The module doc
  ("Usage immediately followed by Finish") changes to say so.
- Rewrite the stale comment in the `ContentBlockStart` arm ("Emit the name-carrying delta
  immediately on block start ... We *don't* emit here"). Rename or re-document the test
  `content_block_stop_produces_no_events`, which still holds for a text block.

### 6.3 The `finish()` signature (both crates)

`finish()` returns one event today, and bedrock's keeps only the first item of `finish_events`
(`.into_iter().next()`). With the net, that would drop the `Finish`. Both change to return
`Vec<Result<ModelEvent, ModelError>>`, the same type that gemini's `finish()` returns. Each
`model.rs` driver changes to `for ev in translator.finish() { yield ev; }`, as in
`crates/paigasus-helikon-providers-gemini/src/model.rs`. Every other caller changes too: the
tests in both `stream.rs` files and anthropic `src/translate/mod.rs`. Assertions of `is_none()`
become `is_empty()`, and the idempotence checks stay. The doc text of both methods changes to
describe an empty `Vec`. Both methods are `pub(crate)`, so this is not a public API change.

This does part of SMA-564 ("Unify the stream translator `finish()` return shape across the six
providers"): anthropic and bedrock move to gemini's shape. SMA-564 stays open for the other
providers. Its acceptance criterion "no provider's observable event sequence changes" applies to
SMA-564's own refactor. Here the only sequence change is the intended one (Goals 1 and 4).

### 6.4 Tests (HAND-AUTHORED)

Each crate gets these tests:

1. Start, block stop, terminal (`tool_use`): one named `ToolCallDelta` before `Finish(ToolCalls)`.
2. The captured shape (with the empty delta): still exactly one named `ToolCallDelta`.
3. Safety net, for each terminal path: anthropic `message_stop` and `finish()`; bedrock
   `Metadata` after `MessageStop`, `MessageStop` after `Metadata`, and `finish()`.
4. The net is limited: a block with no stop and a terminal reason other than `tool_use` gives no
   `ToolCallDelta`.
5. Block stop with `max_tokens` (anthropic) or `max_tokens` stop reason (bedrock): one named
   `ToolCallDelta`, then `Finish(Length)`. This pins §4.
6. Two zero-delta calls: the net emits both names in ascending index order.
7. One named block and one block with no name at the net: only the second gets a
   `ToolCallDelta`.
8. The synthesized tool with no delta gives no `ToolCallDelta`.
9. A block stop after the terminal event emits nothing. Bedrock: after `Finish`, and after the
   both-tools `Err`.
10. `finish()` twice after a net flush: the second call is empty.
11. Bedrock only: a repeated `ContentBlockStop` gives one `ToolCallDelta`; and
    `forbid_tool_calls` with a no-delta call and `ContentBlockStop` gives nothing after the `Err`.
12. Anthropic only: a start event whose `input` has keys, and no delta, gives a `ToolCallDelta`
    with that JSON as `args_delta`.

## 7. Left to the builder

- Test names and helper reuse.
- Where the arguments rule (§2 Goal 3) lives in anthropic: a small helper is expected, so the
  block-stop emit and the net use one rule.
