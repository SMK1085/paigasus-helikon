# SMA-833: Bedrock `ToolChoice::None` with a tool history

## 1. Problem

When `model_settings.tool_choice` is `ToolChoice::None`, the Bedrock
translator sends no `toolConfig`. The forced-tool path does this in
`build_request` (`translate/mod.rs`, the `none_choice` early return). The
native path does it in `build_native` (`translate/native.rs:70-76`), because
it sends no user tools for `None`.

Converse requires `toolConfig` when the messages contain `toolUse` or
`toolResult` blocks. Thus a `None` turn after a tool loop fails with:

`ValidationException: The toolConfig field must be defined when using toolUse and toolResult content blocks.`

The usual case is a final "answer now, do not call tools" turn.

## 2. Required behavior

- A `None` request with a tool round in the history gets a 200 response.
- The model cannot make a real tool call in that turn.

## 3. Evidence (live probes, 2026-11-21 and 2026-11-22, us-east-1)

The probes used the AWS CLI 2.39.6 and boto3 (`Converse` and
`ConverseStream`), with the `maip-dev-admin` profile. The history was one
`get_weather` round. The "adversarial" prompt told the model to call the tool
again. The "neutral" prompt asked for a summary.

| # | Request | Result |
|---|---|---|
| P1 | No `toolConfig` (the current `None` shape) | Sonnet 5.5, Opus 5.5, Nova 2 Lite: 400, B15 error |
| P2 | Tools, no `toolChoice` | Sonnet 5.5, Opus 5.5, Nova 2 Lite: 200, **real `toolUse`** |
| P3 | `toolConfig.tools = []` | Sonnet 5.5, Opus 5.5: 400 `The provided request is not valid` (the CLI also refuses it, min length 1) |
| P4 | Tools + `additionalModelRequestFields: {"tool_choice":{"type":"none"}}` | Sonnet 5.5, Opus 5.5: 200, no `toolUse`, on `Converse` and `ConverseStream`. Nova 2 Lite: 400 `extraneous key [tool_choice] is not permitted` |
| P5 | Only the `tool_choice` field, no `toolConfig` | Sonnet 5.5, Opus 5.5: 400, B15 error |
| P6 | Tool blocks written as text, no `toolConfig` | Sonnet 5.5, Opus 5.5, Nova 2 Lite: 200, no `toolUse` |
| P7 | P4 with one placeholder tool instead of the history's tool | Sonnet 5.5, Opus 5.5: 200, but the model says that the earlier result "didn't come from a real weather tool" |
| P8 | P4 with a spec made from the history's tool name, schema `{"type":"object"}` | Sonnet 5.5, Opus 5.5: 200, no `toolUse`. Neutral: a correct answer |
| P9 | P8 on forced-tool-path Claude models | Haiku 5.5, Sonnet 5, Opus 5, Sonnet 4.5: 200, no `toolUse`, on `Converse` and `ConverseStream`. Neutral: a correct answer. Claude 3.5 Haiku and 3.7 Sonnet: end of life on Bedrock (`ResourceNotFoundException`), not testable |
| P10 | A history with only a `toolResult` (no `toolUse`), placeholder tool, P4 field | Sonnet 5.5: 400 `Expected toolResult blocks at messages.0.content for the following Ids: t1` |

Observations:

- P4, P8, P9, adversarial prompt: Claude returned 200 with an **empty**
  assistant message (one output token, sometimes one empty reasoning block).
  Neutral prompt: a normal text answer.
- P6, adversarial prompt: Nova and Sonnet wrote fake call text, for example
  `[tool call t2: get_weather({"city": "Berlin"})]`. This is text, not a tool
  call. Opus wrote that it has no weather tool in this turn.
- P7 against P8: a placeholder tool makes Claude doubt the tool history. A
  spec with the history's real tool name does not.
- P10: Converse rejects an orphan tool result, whatever the `toolConfig` holds.

Converse has no `none` value for `toolChoice` (only `auto`, `any`, `tool`).

## 4. Approaches

1. **One fix for each family (chosen by Sven, 2026-11-22).** Claude gets the
   Anthropic `tool_choice: none` field with a valid `toolConfig`. The other
   families get the tool history as text and no `toolConfig`. Every family
   gets a 200 with no real tool call, and Claude keeps the structured history.
2. **Text history for every family.** One code path and no new request field.
   Rejected: Claude loses the structured history, and P4 gives Claude a
   cleaner result than P6.
3. **Anthropic field, and an error for the other families.** Rejected: a
   `None` turn after a tool loop then still fails on all non-Claude families.

Rejected without a probe-backed alternative: tools with no `toolChoice` (P2),
because the model then calls a tool.

## 5. Design

### 5.1 When the fix applies

The fix applies only when all of these are true:

- `tool_choice` is `Some(ToolChoice::None)`,
- the translated messages contain a `toolUse` or `toolResult` block (the
  existing `has_tool_round` test, moved to a place that both paths can use),
  and
- on the native path, the response format is not `JsonSchema` or
  `JsonObject` (see 5.5).

A `None` request with no tool round keeps today's shape: no tools, no
`toolConfig`, no new field. The existing test
`tool_choice_none_with_user_tools_omits_tool_config` stays green unchanged.

### 5.2 The strategy lives on `ModelFamily`

Add `ModelFamily::tool_choice_none_strategy() -> ToolChoiceNoneStrategy`
next to `supports_forced_tool_choice` (`family.rs`). It is `pub(crate)`.

- `Anthropic` → `ToolChoiceNoneStrategy::AnthropicNoneField` (5.3).
- `AmazonNova`, `AmazonTitan`, `Llama`, `Mistral`, `Cohere`, `Unknown` →
  `ToolChoiceNoneStrategy::TextHistory` (5.4).

One shared helper in the `translate` module applies the strategy. It takes the
translated messages, the user tools, the family and the ruleset. It returns
the final messages, the tool list and the `tool_choice_none` flag (5.3 step
4). Both builders call this helper, so the two paths cannot drift. SMA-837 (a
family override) then changes the branch through the family only.

### 5.3 `AnthropicNoneField` (Claude)

1. Build the tool list:
   - the user tools, through `tool_specs` (the same schema rewrite and the
     same reserved-name check that every other tool choice uses), then
   - one derived spec for each tool name in the history's `toolUse` blocks
     that is not already in the list. The order is the first occurrence, with
     no duplicates. Each derived spec has the name from the history, the
     description `A tool that this conversation used earlier.`, and the input
     schema `{"type":"object"}`. This schema does not go through
     `rewrite_tool_schema`; it has nothing to rewrite. A derived spec skips
     the reserved-name check, because the name came from an earlier model
     turn (for example the synthesized structured-output tool). A name that
     the `ToolSpecification` builder refuses is skipped with a `debug!`; a
     `toolUse` with no matching spec is accepted (P7 has one).
2. If the list is empty after step 1, the helper changes nothing: the request
   keeps today's shape. This occurs only for a history with orphan tool
   results and no user tools, and Converse rejects that history anyway (P10).
   No placeholder tool is added.
3. Send `toolConfig` with this list and **no** `toolChoice`.
4. Set the new `PreparedConverse` field `tool_choice_none: bool` to `true`.
   `send_converse_stream` (`model.rs`) then sends
   `additionalModelRequestFields = {"tool_choice": {"type": "none"}}`. The
   flag is a typed intent and not a raw `Document`, so a later feature that
   adds a second key (for example `thinking`) merges into one JSON object at
   one place. All other builders set the flag to `false`. The
   wire-projection helper for the snapshot tests prints the field when the
   flag is set.

A user tool named `__paigasus_structured_output__` now makes a `None` request
with a tool round fail on Claude with the existing reserved-name error. This
is the same error that every other tool choice gives, so it is intended.

`has_forced_tool_choice()` stays `false` for this request, so the
forced-choice fallback (`fallback.rs`) never fires for it.

### 5.4 `TextHistory` (all other families)

Nova rejects the Anthropic field (P4), and we have no evidence that the other
families accept it.

After `items_to_messages`, replace every tool block in the messages:

- `ContentBlock::ToolUse` becomes
  `ContentBlock::Text("[tool call <toolUseId>: <name> <input>]")`.
- `ContentBlock::ToolResult` becomes
  `ContentBlock::Text("[tool result <toolUseId>: <text>]")`.

`<input>` and `<text>` are JSON-encoded with `serde_json::to_string`. The
input goes from the `Document` to a `serde_json::Value` through a new
`document_to_value` helper in `document.rs`. The workspace builds
`serde_json` without `preserve_order`, so object keys come out **sorted**, and
the text is stable. `<text>` is the JSON string literal of the result text, so
a result that holds `]`, a quote or a newline cannot close the bracket early.
The translator makes exactly one text content block per tool result today
(`build_tool_result_block`). The helper joins the text of all text blocks with
`\n` only as a defensive case.

The order of the blocks in each message does not change. The request sends no
`toolConfig` and the flag stays `false`. The helper logs one `debug!` with
target `paigasus::bedrock::translate` when it writes a history as text.

### 5.5 Both wire paths

- **Forced-tool path (`build_request`).** The `none_choice` early return calls
  the helper when 5.1 is true. The existing conflict guard (`JsonSchema` or
  `JsonObject` with `None`, on families that support forced tool choice) does
  not change and runs first.
- **Native path (`build_native`).** When the format is not structured, `None`
  with a tool round calls the helper. When the format is `JsonSchema` or
  `JsonObject`, the path keeps today's shape: no user tools and the hidden
  anchor (SMA-839 section 7.5). That shape is a 200 today, and the test
  `snap_native_tool_choice_none_json_schema_finalizing_anchor` stays
  unchanged. That the model can call the anchor is SMA-847.
- The native path is used only for `ModelFamily::Anthropic` today
  (`family.rs`, `fallback.rs`), so its `TextHistory` branch is not reachable
  yet. It goes through the shared helper and gets a unit test that calls
  `build_native` with a Nova config directly.

### 5.6 A local guard in the stream translator

Do not rely only on the service to honor `tool_choice: none`. When the
request has `tool_choice == Some(ToolChoice::None)` (any family), the stream
translator turns a `toolUse` content block start into
`ModelError::Other("the model called a tool, but tool_choice is None")` and
emits no `ToolCallDelta` for it. The translator gets this as one more input
flag, set from the `ModelRequest` when the stream starts. A synthesized anchor
call (the native `JsonSchema` + `None` case of 5.5) is not a user tool call:
the guard does not fire while `synthesizing` is `true`.

## 6. Tests

Unit tests (`translate/mod.rs`, `translate/native.rs`, `family.rs`,
`document.rs`, `stream.rs`):

- `tool_choice_none_strategy` for every `ModelFamily` variant.
- Claude, `None`, tool round, user tools that include the history's tool:
  `toolConfig` holds the user tools only, `toolChoice` is absent, the flag is
  `true`.
- Claude, `None`, tool round, no user tools: one derived spec with the
  history's name, the fixed description and the `{"type":"object"}` schema.
  The test reads the `ToolInputSchema` document directly, because the wire
  projection prints every schema as `"<Document>"`.
- Claude, `None`, tool round, user tools that do not include the history's
  tool: user tools first, then the derived spec. A history that calls the
  same tool twice gives one spec.
- Claude, history tool named `__paigasus_structured_output__`: the derived
  spec is accepted.
- Claude, user tool named `__paigasus_structured_output__` with `None` and a
  tool round: the reserved-name error.
- Claude, history with only orphan tool results and no user tools: today's
  shape, flag `false`.
- Nova and Llama, `None`, tool round: no `toolConfig`, flag `false`, and the
  messages hold the exact text blocks of 5.4.
- Text history: an input with several keys gives sorted keys. A result that
  holds `]`, `"` and a newline is escaped.
- `None` with no tool round, on Claude and Nova: today's shape, flag `false`.
- Native path, Claude, `None`, tool round, no format: the 5.3 shape.
- Native path, Claude, `JsonSchema` and `JsonObject` with `None` and a tool
  round: today's anchor shape, flag `false`.
- Native path through `build_native` with a Nova config: the 5.4 shape.
- `document_to_value` round-trips `value_to_document` for nested values.
- Stream guard: a `toolUse` start with `None` gives the error and no
  `ToolCallDelta`. With `Auto` it gives a `ToolCallDelta`. With `None` and
  `synthesizing == true` the anchor is handled as today.
- Every other request shape: the flag is `false` (one assertion in an
  existing broad test).

Snapshot tests: one new wire snapshot each for the Claude and the Nova
`None` + tool-round shapes.

The send wiring in `model.rs` (`.set_additional_model_request_fields`) has no
offline test, the same as `output_config`. The live runs below cover it, and
the PR must not open before they pass.

Live tests in `tests/live.rs` (`#[ignore]`, gated on `BEDROCK_MODEL_ID` like
the others), each with a `get_weather` tool round in the history and
`ToolChoice::None`:

- **Neutral prompt:** the stream succeeds, has no `ToolCallDelta`, the finish
  reason is not `ToolCalls`, and the text is not empty.
- **Adversarial prompt:** the stream succeeds, has no `ToolCallDelta`, and the
  finish reason is not `ToolCalls`. Empty text is allowed (P4, P8, P9).

Run both on `global.anthropic.claude-sonnet-5-5` (native path),
`global.anthropic.claude-haiku-5-5` (forced-tool path) and
`global.amazon.nova-2-lite-v1:0` (text history). Record the results in the PR
body.

## 7. Known limits and non-goals

- **Empty answer on Claude.** When the prompt tells Claude to call a tool and
  the `none` field forbids it, Claude can return an empty message (P4, P8,
  P9). The request succeeds and makes no tool call, which meets the ticket.
  This change does not alter how the core runner handles an empty assistant
  turn; the plan does not investigate it. If it causes a problem, that is a
  new Linear issue.
- **Fake call text on other families** (5.4, P6). The model can write text in
  the `[tool call ...]` format. It is never a real tool call.
- **Prompt injection through the text history.** On the `TextHistory`
  families, the tool result sits in a user-role text block. JSON encoding
  stops a result from closing the bracket, but the model can still read
  instructions inside the result as user text more easily than inside a
  structured `toolResult` block.
- **No probe for Titan, Llama, Mistral, Cohere.** They get 5.4 because it
  sends no tool fields at all, so it does not depend on what each family
  accepts.
- **Claude 3.x is not probed** (end of life on Bedrock). If a Claude model
  rejects the `tool_choice` field, the request fails with that 400. There is
  no automatic change to 5.4.
- **A history that ends with an unanswered `toolUse`** behaves as today in
  `items_to_messages`. This change adds no new check for it.
- The native anchor case (`JsonSchema` or `JsonObject` with `None`) is
  SMA-847.
- A `ModelFamily` override (SMA-837) changes which strategy a model gets
  through 5.2, and needs no work here.

## 8. Documentation

- `crates/paigasus-helikon-providers-bedrock/README.md`: the native
  tool-choice text (about lines 83 and 99) gets one sentence on `None` after a
  tool round. Also add a short note on the `None` behavior for the forced-tool
  path and the other families.
- `docs/book/src`: no page describes the Bedrock tool-choice mapping, so no
  book edit.
- No public API changes. The commit type is `fix(providers)`.
