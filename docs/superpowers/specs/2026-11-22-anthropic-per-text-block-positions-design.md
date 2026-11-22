# SMA-854: one position for each Anthropic text block

Linear: [SMA-854](https://linear.app/smaschek/issue/SMA-854). Approach approved by
Sven on 2026-11-22. Revised after the adversarial challenge on the same day.

## Intent

The Anthropic stream records a position only for the first text block of a
response (`stream.rs`, `text_position_recorded`). The request translator gives
all text of the turn that one position (`translate/request.rs`, `text_seen`).
When a thinking block is between two text blocks, the replay changes the block
order:

- Response: `[Thinking(0), Text "A"(1), Thinking(2), Text "B"(3), ToolUse(4)]`
- Replay today: `[Thinking(0), Text "AB", Thinking(2), ToolUse(4)]`

With `ExtendedThinking::Adaptive` the provider sends `drop_block`. The models
that run the prefix check then drop `Thinking(2)` and every later thinking
block. The request does not fail, but the model loses that reasoning.

The goal: the replay sends the text blocks of such a response at their own
positions, so the block order is the same as in the response.

### Evidence for the bug

The SMA-840 design has two statements that disagree. Its non-goal says "Probe
finding 4 shows the API does not check order". Its later prefix-check probe
table (`2026-11-21-sma-840-anthropic-thinking-signatures-design.md`, "Probe
result") says that "Inserted a text block before the thinking block in the same
message" makes the API drop the thinking block. The joined `"AB"` before
`Thinking(2)` is a change of that kind. The SMA-854 live probe (below) confirmed
this mechanism again: a text block added before a thinking block in the same
message gives `thinking_dropped` with `prefix_binding_mismatch`.

## Facts that set the design

- The core joins all `TokenDelta` texts of a turn into one `ContentPart::Text`
  with `push_str` and no separator (`paigasus-helikon-core/src/model.rs:701`).
  No `ModelEvent` marks a text block boundary. `build_items` gives at most one
  `AssistantMessage`, with the parts `[Reasoning?, Text?, ProviderData*]`.
- The stream emits position records only after it keeps a thinking block
  (`position`, `waiting_positions`). A response with no kept thinking block
  writes no provider data, so its replay does not change.
- `AnthropicData` is `pub(crate)`. The `data` shape is private to the crate
  (`provider_data.rs` module doc, book `docs/book/src/concepts/model-providers.md:134`).
- The keep rule contract (`request.rs:220-228`): when a rule fails, the thinking
  blocks are dropped, and the item builds as if it had no Anthropic part. The
  SMA-840 tests check this with `without_anthropic` equality.
- SMA-840 is merged on `main` but not yet released. So stored data with a `text`
  position exists only for users who build from `main`.

## Goals

- The stream records one position for each text block of a response.
- The replay sends each text block at its own position, when the thinking
  blocks of that response are sent.
- When the thinking blocks are not sent, the message builds exactly as it does
  today: one joined text block.
- Stored data from before this change still replays as it does today.

## Non-goals

- No change to `paigasus-helikon-core`, to `ModelEvent`, or to the "at most one
  `Text` part" behaviour of the core.
- No change for other providers.
- No change to a response with no kept thinking block.

## Approaches

1. **Chosen: record the byte length of each text block, and cut the joined text
   on replay.** The change is only in the Anthropic crate. When the stored
   lengths do not match the text, the translator falls back to the behaviour of
   today.
2. **A core event for text block boundaries, so that the core makes several
   `Text` parts.** Rejected. It changes the core contract, needs a breaking
   bump of the core with a cascade to all dependent crates, and touches every
   provider. That is too much for a rare shape with Low priority.
3. **Keep one joined text block at a different position.** Rejected. Any
   position for the joined text still changes the order before `Thinking(2)`.

## Design

### Stream (`stream.rs`)

- Remove `text_position_recorded`. On each `content_block_stop` of a text block,
  emit `AnthropicData::Text { index, bytes }` through `position`, so the record
  waits for the first kept thinking block, as today.
- `bytes` is the UTF-8 byte length of all `text_delta` texts of that block. The
  `BlockState::Text` variant counts it.
- A `text_delta` for an index that is not an open text block still emits its
  `TokenDelta`, as today. It adds to no count, and it gets a `tracing::debug!`
  line in the style of the existing `signature_delta` line.
- These shapes make the sum fall short of the joined text, so the replay falls
  back: synthesized structured-output JSON (`TokenDelta` from the synthesized
  tool), a `text_delta` for an index that is not open, a text block whose stop
  never arrives, a stop after the terminal event, and a repeated
  `content_block_start` that resets the count.

### Data shape (`provider_data.rs`)

- `AnthropicData::Text` gets `bytes: Option<u64>` with `#[serde(default,
  skip_serializing_if = "Option::is_none")]`. New data:
  `{"kind":"text","index":3,"bytes":5}`. The field name says the unit. Data with
  no `bytes` still parses.
- The doc of `Text` changes from "the first text block" to "one text block".

### Replay (`translate/request.rs`)

The split happens late, so that the drop paths see one text block as today.

1. In `assistant_blocks`, build **one** text `WireBlock` for the text part, as
   today. Its key is the key of the first `Text` position, or
   `SortKey::BeforeFirstToolUse` when there is none. When the message is
   splittable (below), the block also carries a **split plan**: a list of
   `(SortKey::At(index, 1), byte range)`, one entry for each `Text` position.
2. `drop_thinking_without_own_block` and the last-message path in
   `into_output` run as today. They clear the slot of the block when the
   thinking of its group is dropped.
3. In `WireMessage::into_value`, after `drop_thinking_without_own_block` and
   before `sort_groups`, expand each text block that still has a slot and a
   split plan. Each piece becomes its own `text` block with its own key. A
   whitespace-only piece **is sent**. Probe (c) showed that the API accepts
   one after a text block, before a `tool_use` block, in an assistant message
   that is not the last one, and keeps the thinking. A whitespace-only piece
   directly after a thinking block, and one as the last block of such a
   message, were not probed. The live test `live_text_block_api_facts` checks
   both shapes (`whitespace_after_thinking`, `whitespace_text_last`).
   An empty piece (`bytes: 0`) gives no block, because the API rejects an
   empty text block with HTTP 400 (probe). A block with no slot does not
   expand, so it stays one joined block.
4. A joined text that is whitespace-only gets no text block, as today. A blank
   block in the content would change the `only_tool_uses`, `thinking_only` and
   "no blocks" decisions of `Translator::step`. Instead, when it is
   splittable, the first thinking block of the item carries its pieces. The
   pieces expand beside that block only when the block survives with its slot.
   When the thinking is dropped, the pieces go with it, as today. So the keep
   rule decides exactly as today, and a last assistant message never sends a
   whitespace-only text.

So a hand-off transcript, a history that lost the tool calls, and a last
assistant message all build as today, with one joined text block.

The keep rule decides the same as today. Only a non-blank joined text counts in
the keep rule, before and after this change.

### When a message is splittable

All of these must be true. Else the message builds as today, and a
`tracing::debug!` line gives the reason and the lengths, never the content.

- The message has exactly one non-empty `ContentPart::Text` part.
- It has at least two `Text` positions. With one position, no split is needed.
- Every `Text` position has `bytes`.
- The `Text` indexes, in part order, are strictly increasing.
- No `Text` index is the same as a `Block` or `ToolUse` index of the message.
- The sum of `bytes` is the byte length of the text part. The sum uses
  `checked_add` and `usize::try_from`. An overflow means "not splittable".
- Each cut is on a `char` boundary. The cut uses `str::get`, so a bad value
  never panics.

The cut is in part order, which is the stream order.

### Behaviour that the challenge made explicit

- An edit that keeps the byte length of the text (for example `John` to
  `XXXX`) still splits. This is harmless: the pieces are contiguous slices, so
  their join is always the edited text.
- An edit that changes the length falls back, also when it is after the last
  thinking block. Then the later thinking blocks can be dropped, as today.
- A whitespace-only piece between two thinking blocks, for example
  `[Th0, Text "\n\n"(1), Th2, Text "B"(3)]`, is sent, so `Th2` keeps its prefix.
- An empty text block of the response (`bytes: 0`) cannot go back, because the
  API rejects an empty text block. This is an API rule. The piece has no bytes,
  so the joined text is the same with or without it.
- Citations cannot occur. The API adds citations to a text block only for a
  `document` or `search_result` block with citations on, or for a server tool
  such as web search. The request translator sends only `text`, `image`,
  `tool_use` and `tool_result` blocks, so no response has a `citations_delta`.

### Rollout

- An older build reads the new data safely. `AnthropicData` has no
  `deny_unknown_fields`, and the old `text_seen` logic takes the first position.
- After a downgrade, a later thinking block can be dropped one time under
  `drop_block`. The request does not fail.
- SMA-840 is not released yet, so this is the cheapest time to set the field
  name.

## Tests (TDD)

Each fallback test has a control test. The control uses the same data with
correct `bytes` values, and it splits. So each guard is proved to be the cause.

Stream:

- `[Thinking, Text, Thinking, Text, ToolUse]` emits one `Text` position with
  the correct `bytes` for each text block, after the first kept block.
- Multi-byte UTF-8 text counts bytes.
- A `text_delta` for an index that is not open adds to no count.
- The existing single-text tests get the `bytes` field.

Data:

- The documented shapes test includes `bytes`.
- Data with no `bytes` parses.

Replay, split:

- The issue example replays as
  `[Thinking(0), Text "A", Thinking(2), Text "B", ToolUse(4)]`.
- Text before the first kept thinking block:
  `[Text A(0), Thinking(1), Text B(2), ToolUse(3)]` replays in that order.
- An empty first text block: `[Th0, Text ""(1), tu2, Th3, Text "B"(4), tu5]`.
  Today `"B"` goes to index 1. The new code puts it at index 4. This test is red
  on today's code.
- A multi-byte split, for example `"é"` and `"ü"`.
- A whitespace-only piece gives a block. An empty piece gives no block.
- A whitespace-only joined text: it expands when its group keeps its thinking,
  it is removed when the group loses its thinking, and it does not count in
  the keep rule.
- The existing test `two_text_blocks_join_at_first_text_position`
  (`translate/mod.rs:1277`) is renamed and inverted: the two blocks split.
- The multi-text shape in `session_projection_round_trip`
  (`translate/mod.rs:1337`) is checked against the new result.
- A round trip through `ModelTurnAccumulator` (the `items_of` helper in
  `translate/mod.rs`) from the stream to the request.

Replay, fallback (each with its control):

- A sum mismatch.
- A missing `bytes`.
- A cut that is not on a `char` boundary.
- `Text` indexes that are not strictly increasing.
- A `Text` index that is the same as a `Block` or `ToolUse` index.
- More than one non-blank text part.
- A `bytes` of `u64::MAX`: no panic, and a fallback.
- Hand-built events with synthesized structured output: the sum is short, so a
  fallback. (The API rejects a forced `tool_choice` with thinking on, so a live
  test cannot make this shape.)

Replay, drop paths (each asserts
`messages(&items) == messages(&without_anthropic(&items))` with `bytes` data):

- A hand-off transcript that removed the tool calls.
- A history that lost the tool calls.
- A last assistant message.

## Docs and release

- No public API change. A `fix(providers)` commit gives the Anthropic crate a
  patch bump through release-plz, with the facade cascade.
- The core `ModelTurn` doc (`paigasus-helikon-core/src/model.rs:573-575`) says
  the parts are "text, reasoning and/or provider data, in that order".
  `build_items` puts `Reasoning` first. This PR corrects the doc to "reasoning,
  text and/or provider data". It is a doc-only change, in a separate
  `docs(core)` commit. release-plz then gives the core a patch bump, and the
  dependent crates get a patch bump too. That is harmless.
- In-code docs that say "first text block" change: the module doc of
  `request.rs` (lines 7-12), the comments at `request.rs:701` and `729-730`, and
  the `AnthropicData::Text` doc (`provider_data.rs:85`).
- The comment on `is_blank` (`request.rs`) states the probe result: the API
  rejects an empty text block, and accepts a whitespace-only one in an assistant
  message that is not the last one.
- The SMA-840 design (`2026-11-21-sma-840-anthropic-thinking-signatures-design.md`)
  gets a correction note under its order non-goal, with a link to this design.
- The book (`docs/book/src/concepts/model-providers.md:139`) already says that
  an edit of an earlier message makes the API drop the thinking. That still
  covers the fallback cases, so the book needs no change.
- The crate README describes the keep rule, not the text position. It needs no
  change.

## Live probe

Sven asked for a live probe on 2026-11-22. The probe script asks
`claude-sonnet-5-5` (adaptive thinking, effort `max`, `drop_block`) for a
response where a thinking block follows a text block. It then sends the next
request in four forms and records `input_transformations`:

1. The exact blocks of the response (the split replay of this fix).
2. All text joined at the first text position (the replay of today).
3. An added whitespace-only text block (to check the blank-piece rule).
4. An added empty text block.

Probe 1 (18 calls, 2026-11-22). The model never gave a thinking block after a
text block. 12 calls gave `[thinking, text, tool_use]`. 6 calls with prompts
that told the model when to think got `stop_reason: "refusal"` with the
category `reasoning_extraction`. A probe must not steer the reasoning, so the
exact SMA-854 shape cannot be made on demand. The fix is still needed, because
the shape is valid output and the mechanism is confirmed below.

Probe 2 (neutral prompt of `live_round_trip.rs`, first response
`[thinking, text(97 B), tool_use]`), next request in five forms:

| Form | Result |
| --- | --- |
| a. Exact blocks | thinking kept |
| e. A text block added before the thinking block, same message | `thinking_dropped`, `prefix_binding_mismatch`, `messages.1.content.1` |
| c. A `"\n\n"` text block added after the text block | accepted, thinking kept |
| d. A `""` text block added after the text block | HTTP 400 `text content blocks must be non-empty` |
| f. The text split into two adjacent text blocks | thinking kept |

Consequences for the design:

- (e) confirms the bug mechanism.
- (c) lets the fix send a whitespace-only piece after a text block. The live
  test also checks one directly after a thinking block, and one as the last
  block of the message.
- (d) forbids an empty piece.
- (f) shows that separate adjacent text blocks after a thinking block are safe.
- The comment on `is_blank` in `request.rs` ("The API rejects empty and
  whitespace-only text blocks") is wrong for whitespace-only text in an
  assistant message that is not the last one. This PR corrects the comment. The
  behaviour of `is_blank` for other text does not change: a whitespace-only text
  in a user message or in the last assistant message was not probed, and the
  last assistant message is documented to reject trailing whitespace.
- The SMA-840 design non-goal "Probe finding 4 shows the API does not check
  order" is wrong for blocks before a thinking block. This PR adds a correction
  note to that document.

### Permanent live test

`live_round_trip.rs` gets one ignored live test that checks the API facts this
design depends on. It takes a real first turn through the crate, builds the
second request with `build_body`, and sends six forms: exact, (e), (c), (f),
`whitespace_text_last` (a `"\n"` text block as the last block of the message)
and `whitespace_after_thinking` (a `"\n\n"` text block directly after the first
thinking block). The last two are the whitespace-only shapes that probe 2 did
not send. The test asserts that (e) reports a dropped thinking block (so the
check is active and the test means something), and that the other five return
HTTP 200 and report none. If the API rejects one of the two new shapes, the
test fails, and the replay rule for whitespace-only pieces must change before
release. The assertion messages hold no signature and no text, as the module
doc requires. The test reads `input_transformations` from a non-streaming
response.

Live run on 2026-11-22 (`claude-sonnet-5-5`): `live_text_block_api_facts`
passed, so both new whitespace-only shapes keep the thinking.
`live_thinking_round_trip` and `live_thinking_round_trip_enabled` also passed on
the new code.

## Left to the builder

- The exact names of new helper functions and of the split plan type.
- The place of the cut logic (a small pure function next to
  `assistant_blocks` is expected).
