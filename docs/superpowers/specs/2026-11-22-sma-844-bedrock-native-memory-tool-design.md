# SMA-844: Send the native Anthropic memory tool type through the Bedrock provider

- Linear: [SMA-844](https://linear.app/smaschek/issue/SMA-844/send-the-native-anthropic-memory-tool-type-through-the-bedrock)
- Related: SMA-830 (provider tool types, PR #374), SMA-833 (`tool_choice: none`), SMA-839 (native structured output), SMA-837 (family override)
- Crate: `paigasus-helikon-providers-bedrock`

## 1. Intent

SMA-830 lets a `ToolDef` declare a provider tool type (`ProviderToolType`).
The Anthropic provider sends the typed entry, for example
`{"type": "memory_20250818", "name": "memory"}`. The Bedrock provider ignores
the declaration. It sends the generic `toolSpec` with the JSON schema. So a
Claude model on Bedrock gets the custom schema and not the native memory
protocol.

After this change, a Claude model on Bedrock gets the typed memory entry when
a tool declares the `anthropic` type `memory_*`. All other models, and all
other tool types, get the generic `toolSpec`, as today.

## 2. Goals, non-goals, anti-goals

Goals:

- A tool that declares an `anthropic` provider tool type whose `type_name`
  starts with `memory_` goes to a Claude model on Bedrock as the typed entry.
- The typed entry has the same shape as in the Anthropic provider: the
  `params`, without `cache_control`, then `type` and `name`.
- A builder opt-out: `BedrockModelBuilder::native_tool_types(bool)`, default
  `true`.
- Every request shape that works today with the generic tool also works with
  the typed tool: each tool choice, a tool round in the history, structured
  output on both output paths, and `ToolChoice::None`.

Non-goals:

- Other native tool types (computer use, text editor, bash). They keep the
  generic `toolSpec` (section 5.2). A later change can allow them, with the
  beta headers that some of them need.
- The `InvokeModel` API with the Anthropic Messages body.
- A per-model support list. The gate is the model family only.
- `disable_parallel_tool_use` for a forced choice of a user tool. Today's
  behaviour with parallel calls stays (section 5.6). The one exception is the
  forced structured-output tool (section 5.6).

Anti-goals (a failure even if the code works):

- A non-Claude model gets the typed entry. Nova rejects it with a 400 error
  (p16).
- The memory protocol changes between turns of one conversation. A turn that
  sends the typed entry and a later turn that sends the generic schema for the
  same tool is a failure.
- A request that is valid today on a current Claude model becomes an error,
  either a 400 from Bedrock or an error from the stream translator.

Legacy Claude models (Claude 3.x, Opus 4.1) are out of scope (Sven,
2026-11-22). If one of them rejects the typed entry, the user calls
`native_tool_types(false)`.

## 3. Evidence (live probes, 2026-11-22, us-east-1)

The probes used the AWS CLI 2.39.6 `bedrock-runtime converse` command with the
`maip-dev-admin` profile. The CLI has no `converse-stream` command, so the
probes used `Converse`. The provider uses `ConverseStream`. The live tests in
section 7 must prove the streaming path.

Terms in the table:

- The turn-1 user message is "Before you answer, look in your memory
  directory for earlier notes. Then tell me the capital of France."
- `turn 2` is that message, an assistant `toolUse{name: "memory", input:
  {command: "view", path: "/memories"}}`, and a user `toolResult` with the
  text "Directory: /memories\n(empty)".
- `typed` means `additionalModelRequestFields = {"tools": [{"type":
  "memory_20250818", "name": "memory"}]}`. "+ `tool_choice: X`" means that
  key is also in the additional fields.
- `weather` means a `toolConfig` with one generic `get_weather` tool and no
  `toolChoice`.
- `placeholder` means a `toolConfig` with one `toolSpec` named
  `__paigasus_structured_output__`, the description "Placeholder. Never call
  this tool." and the schema `{"type": "object"}` (p10 used
  `{"type": "object", "properties": {}}` and "Do not call this tool.").
- Unless the table names another model, the model is
  `us.anthropic.claude-sonnet-4-6`.

| Id | Request | Result |
|----|---------|--------|
| p1 | turn 1, `typed`, no `toolConfig` | OK. `stopReason: tool_use`, `toolUse{name: "memory", input: {command: "view", path: "/memories"}}` |
| p2 | turn 1, `typed` + `weather` | OK. The model calls `memory`. |
| p3 | p2 + `anthropic_beta: ["context-management-2025-06-27"]` | OK. No difference. The beta header is not necessary. |
| p4 | `typed` + `weather`, the user asks for the weather | OK. The model calls `get_weather`. Bedrock merges the two tool lists. |
| p5 | turn 2, `typed`, no `toolConfig` | 400: `The toolConfig field must be defined when using toolUse and toolResult content blocks.` |
| p6 | p5 + `weather` | OK. `end_turn`, text answer. |
| p7 | turn 1, `typed` + a generic `toolSpec` named `memory` | 400: `The model returned the following errors: tools: Tool names must be unique.` |
| p8 | turn 1, `typed` + `weather` with `toolChoice: {any: {}}` | OK. The model calls `memory`. `any` covers the merged list. |
| p9 | p5 + `toolConfig: {tools: []}` | Client-side validation error: `toolConfig.tools` min length 1. |
| p10 | p5 + `placeholder` (old text and schema) | OK. `end_turn`, text answer. |
| pe | p5 + `placeholder` | OK. `end_turn`, text answer. |
| p11 | `typed` + `weather` with `toolChoice: {tool: {name: "memory"}}` | 400: `The tool memory at toolConfig.toolChoice.name is not defined in tools.` |
| p12 | p6 + `tool_choice: {type: "none"}` | OK. `end_turn`, text answer. |
| p13 | `typed` + `tool_choice: {type: "tool", name: "memory"}` + `weather`, the user asks for the weather | OK. The model calls `memory`. The forced choice applies. |
| p14 | p13 on `us.anthropic.claude-sonnet-5-5` | 400: `The model returned the following errors: tool_choice: type "tool" and "any" are not supported for this model.` |
| p15 | `typed` + `tool_choice: {type: "any"}`, no `toolConfig`, the user asks for the weather | OK. The model calls `memory`. |
| pb | `typed` + `tool_choice: {type: "any"}` + `weather`, the user asks for the weather | OK. The model calls `get_weather`. |
| p16 | turn 1, `typed` on `us.amazon.nova-pro-v1:0` | 400: `Malformed input request: #: extraneous key [tools] is not permitted` |
| p17 | turn 1, `typed` on Sonnet 4.5, Haiku 4.5, Sonnet 5.5 | OK on all three. |
| p18 | turn 1, `typed` + `tool_choice: {type: "tool", name: "get_weather"}` + `weather` | OK. The model calls `get_weather` and, in parallel, `memory`. A forced choice in the additional fields can name a `toolConfig` tool. |
| p19 | turn 1, `typed` + `tool_choice: {type: "tool", name: "__paigasus_structured_output__"}`, `toolConfig` with only the synthesized output tool | OK. The model calls the synthesized tool and, in parallel, `memory`. |
| p20 | p5 + `placeholder` (old) + `tool_choice: {type: "none"}` | OK. `end_turn`, text answer. |
| p21, p22 | Baseline without typed tools, two runs each: generic `memory` + `get_weather` with `toolChoice.tool = get_weather` (p21); generic `memory` + the synthesized tool with `toolChoice.tool = __paigasus_structured_output__` (p22) | OK. Each run gives the forced tool and a parallel `memory` call, the same as p18 and p19. |
| p23 | p18 + `disable_parallel_tool_use: true` in `tool_choice` | OK. Only `get_weather`. |
| p24 | p19 + `disable_parallel_tool_use: true` in `tool_choice`. Three runs. | OK. Each run gives only the synthesized tool. |
| pc1 | turn 1, `typed` + `outputConfig.textFormat` (JSON schema `{capital}`), no `toolConfig`, on Sonnet 5.5 | OK. The model calls `memory`. |
| pc2 | turn 2, `typed` + `placeholder` + the `outputConfig` of pc1, on Sonnet 5.5 | OK. `end_turn`, text `{"capital":"Paris"}`. |
| pd1 | turn 2, `typed` + `tool_choice: {type: "any"}` + `placeholder` | OK. The model calls `memory`. |
| pd2 | turn 2, `typed` + `tool_choice: {type: "tool", name: "memory"}` + `placeholder` | OK. The model calls `memory`. |

The raw request and response files are in the session scratchpad
(`probes/`). They are not committed. The live tests in section 7 repeat the
important shapes with `ConverseStream`.

Other facts:

- Each `toolUse` block in the response has an extra key, `"type": "tool_use"`.
  The SDK 1.157.0 `ToolUseBlock` and `ToolUseBlockStart` have a field
  `r#type: Option<ToolUseType>`. The SDK knows only `server_tool_use`, so the
  value becomes `ToolUseType::Unknown("tool_use")`. `stream.rs` does not read
  this field, so the response path does not change.
- The p14 error text matches the first pattern of
  `error::is_forced_choice_rejection`. So the SMA-839 fallback can detect a
  rejected forced choice in the additional fields, if
  `PreparedConverse::has_forced_tool_choice` reports it (section 5.5).
- The AWS documents agree with p1 to p4: typed Anthropic tools go in
  `additionalModelRequestFields.tools`
  (<https://docs.aws.amazon.com/bedrock/latest/userguide/computer-use.html>).
  No document describes the merge with `toolConfig`. The probes are the only
  source for it.

## 4. Approaches

1. **Converse, typed entries in `additionalModelRequestFields.tools`
   (chosen).** It uses the existing additional-fields path in `model.rs` and
   the existing stream parser. Two adaptations follow from the probes: a
   placeholder `toolSpec` when the history needs a `toolConfig` and no other
   `toolSpec` exists (p5, p9, pe), and the tool choice in the additional
   fields when the request has a typed tool (p11, p13, p15, p18, p19).
2. **`InvokeModelWithResponseStream` with the Anthropic Messages body.** It
   needs a second request translator and a second stream parser for a second
   wire format. Approach 1 works, so this adds much code for no gain.
3. **The typed entry only on simple turns, the generic schema on other
   turns.** It is smaller, but the memory protocol changes between turns. That
   is an anti-goal.

Sven chose approach 1 on 2026-11-22. He also chose the family-only gate, with
no per-model list.

## 5. Design

### 5.1 Configuration and the gate

- `Config` gets `native_tool_types: bool`. The test literals of `Config` in
  `translate/mod.rs` and `offline_config` in `model.rs` need the new field.
- `BedrockModelBuilder::native_tool_types(enabled: bool) -> Self`. The default
  is `true`. The doc comment follows the Anthropic builder: with `false`, the
  provider always sends the generic `toolSpec` and ignores declared provider
  types.
- One method holds the gate, for example
  `Config::sends_typed_tools(&self) -> bool`: `native_tool_types` is `true`
  and `family == ModelFamily::Anthropic`. No other code repeats the
  condition. `cfg.family` is the detected family or the family from
  `BedrockModelBuilder::family` (SMA-837). So an application inference
  profile ARN with `.family(ModelFamily::Anthropic)` also gets the typed path.

### 5.2 Which declaration matches

A tool is typed when all of these are true:

- the gate (5.1) is open,
- the tool has a `ProviderToolType` with `provider == "anthropic"` (the first
  such entry wins, as in the Anthropic provider),
- that entry's `type_name` starts with `memory_`.

Reason for the `memory_` rule: other Anthropic types are not tested on
Bedrock, and computer use needs an `anthropic_beta` value that this change
does not send. Without the rule, a tool that declares `computer_*` would work
today and fail with a 400 error after the change. The rule is per tool type,
not per model, so it does not conflict with the family-only gate.

The Bedrock crate does not depend on the Anthropic crate. It gets a private
constant `ANTHROPIC_PROVIDER_ID: &str = "anthropic"` (`pub(crate)`), with a
doc comment that names `paigasus_helikon_providers_anthropic::PROVIDER_ID`.
A test in the facade crate, which compiles both providers under
`--all-features`, asserts that the two values are equal. To make that
possible, the Bedrock crate exposes the value through a `#[doc(hidden)]`
public constant, or the test compares the Anthropic constant with the
literal `"anthropic"` that the Bedrock docs promise. The builder picks one.

When the gate is closed or the type is not `memory_*`, a tool with an
`anthropic` declaration gets the generic `toolSpec` and one `debug!` line
that says why (gate closed, or type not supported on Bedrock).

### 5.3 The typed entry

For a typed tool with declaration `pt`:

1. Start with a copy of `pt.params`.
2. Remove `cache_control`.
3. Set `type` to `pt.type_name` and `name` to `ToolDef.name`. These two keys
   override the same keys in `params`.

The result is a JSON object. The tool's `description` and `schema` are not
sent. The schema rewriter does not run on a typed tool. The reserved name
check (`SYNTHESIZED_TOOL_NAME`) applies to typed tools too.

### 5.4 Partition of the tool list

A new function in `translate/tools.rs` splits `req.tools` into two lists:
generic `ToolSpecification`s (as today) and typed entries (5.3). When the
gate is closed, the typed list is empty and the generic list is the same as
today's `tool_specs`.

The partition is made once per request and passed to the three places that
call `tool_specs(&req.tools, ..)` today: `build_request` (forced path),
`native::build_native`, and `tool_choice_none::apply_tool_choice_none`.
`apply_tool_choice_none` takes the partition result, not the flags.

### 5.5 `PreparedConverse` and the additional fields

The additional fields are built once, in `translate`, as a
`serde_json::Value`. `PreparedConverse` holds that value, for example in a
small type with two parts:

- `tools`: the typed entries, in the order of `req.tools`.
- `tool_choice`: unset, or one of `{"type": "none"}`, `{"type": "any"}`,
  `{"type": "tool", "name": ..}`.

This type replaces the field `tool_choice_none: bool`. `model.rs` converts
the value with `value_to_document` and sends it, or sends nothing when both
parts are empty. `to_wire_json` projects the same value. So the snapshots
test the request that the provider sends, not a copy of the wire code. The
existing snapshots must not change.

`PreparedConverse::has_forced_tool_choice` returns `true` also when the
additional `tool_choice` is `any` or `tool`. So the SMA-839 fallback retries
on the native path when a model rejects the forced choice (p14).

### 5.6 Tool choice when the request has a typed tool

Rule: **when the request sends a typed tool, the tool choice goes only in the
additional fields, and `toolConfig.toolChoice` is not set.** Reason: a
`toolConfig.toolChoice.tool` that names a typed tool fails (p11). The
additional `tool_choice` works for `tool` on a typed name (p13), on a generic
name (p18), on the synthesized tool (p19) and with the placeholder (pd2). It
works for `any` with no `toolConfig` (p15), with `weather` (pb) and with the
placeholder (pd1). It works for `none` (p12, p20). One rule for all choices
is simpler than a rule per choice.

When the request sends no typed tool, nothing changes.

A forced choice lets the model add parallel calls to other tools. This is
today's behaviour too (p21, p22), so the change keeps it for a forced user
tool.

Exception (added after the live tests, 2026-11-22): when the additional
choice forces the synthesized structured-output tool, the provider adds
`"disable_parallel_tool_use": true` to that `tool_choice`. Reason: with
`synthesizing == true`, `stream.rs` rejects a response that also calls a
real tool. The native memory protocol makes the model call `memory` first,
so the live test of forced structured output with a typed memory tool failed
on Sonnet 4.6 in 3 of 3 runs. p24 shows that the flag fixes it. A parallel
call is never valid in this case, so the flag removes nothing that worked.
The flag goes only in the additional `tool_choice`. A request without a typed
tool does not change.

Forced-tool path (`OutputPath::ForcedTool`), with a typed tool:

| Request | Today (generic tool) | With a typed tool |
|---------|----------------------|-------------------|
| `Auto` or no choice | `toolChoice: auto` or none | No choice. Auto is the default. |
| `Required` | `toolChoice: any` | `tool_choice: {type: any}` |
| `Tool { name }` | `toolChoice: tool` | `tool_choice: {type: tool, name}` |
| Structured output (synthesized tool) | `toolChoice: tool` (synthesized) | `tool_choice: {type: tool, name: "__paigasus_structured_output__", disable_parallel_tool_use: true}` |
| `None` | section 5.8 | section 5.8 |

Native path (`OutputPath::Native`): the native path never forces a tool call,
and this does not change. `Required` and `Tool` are omitted and give the same
notices as today (`RequiredOmitted`, `ToolOmitted`). `Auto` gives no choice.

The checks count typed tools, **on both paths**:

- `check_tool_name` (forced path in `build_request`, native path in
  `build_native`): a `ToolChoice::Tool` name is valid when it names a generic
  or a typed tool. The placeholder (5.7) is never a valid name.
- `Required` with no tool (`required_without_tools_error`, both paths): the
  error occurs only when no generic tool and no typed tool exists.
- The existing conflict guards for structured output with `Tool` or `None`
  do not change.

### 5.7 The placeholder for a tool round

Bedrock needs a `toolConfig` when the history holds a `toolUse` or a
`toolResult` block (p5), and `toolConfig.tools` needs at least one tool (p9).

Rule: the provider adds one placeholder `toolSpec` when all of these are
true:

- the request sends a typed tool,
- the history holds a tool round (`tool_choice_none::has_tool_round`),
- no other `toolSpec` goes in the `toolConfig` (no generic tool, no
  synthesized output tool, no derived spec).

The placeholder:

- name: `SYNTHESIZED_TOOL_NAME` (already reserved for user tools),
- description: a short text that tells the model never to call it, for
  example "Placeholder. Never call this tool.",
- input schema: `{"type": "object"}` (pe).

`synthesizing` stays `false` for the placeholder. One helper builds the
placeholder for all call sites (forced path, native path, `None` strategy).
It logs one `debug!` line when it adds the placeholder.

If a model calls the placeholder, `stream.rs` takes the real-tool branch
(with `synthesizing == false`), or the forbid branch under
`ToolChoice::None`. It does not panic. The call reaches the agent as a call
to a tool named `__paigasus_structured_output__`. The agent then returns
`unknown tool: ...` as the tool result (`paigasus-helikon-core`
`agent.rs`). This is my call. The risk is largest for `Required` with only
typed tools after a tool round, because `any` also covers the placeholder.
In pd1 the model called `memory`, not the placeholder. The README names this
case. A stream unit test fixes the behaviour: a placeholder call with
`synthesizing == false` gives a normal tool call and `Finish(ToolCalls)`.

#### The native structured-output anchor

`build_native` adds the synthesized structured-output anchor today when
`structured && all_tools.is_empty() && has_tool_round`, and sets
`synthesizing = true`. With `synthesizing == true`, `stream.rs`
(`finish_events`) gives an error when a real tool fired. So if typed tools
were not counted, a typed `memory` call on turn 2 with `JsonSchema` or
`JsonObject` on Sonnet 5.5 or Opus 5.5 would fail. Today the generic
`memory` tool fills `all_tools`, so the anchor is not added.

Rule: on the native path, the structured-output anchor is added only when no
generic tool **and no typed tool** is sent. When typed tools are sent and no
generic tool exists, the placeholder (`synthesizing = false`) is added
instead. pc2 shows that `outputConfig` and the placeholder work together.

### 5.8 `ToolChoice::None`

On Claude models the strategy is `AnthropicNoneField` (SMA-833). Today it
sends the user tools, plus one derived spec for each tool name in the history
that is not listed, plus the `tool_choice: none` field. When no tool round
exists, it sends no tools.

With typed tools, on both paths (the forced path, and the native path when
the request is not structured):

- No tool round: no change. No tools go out, and no typed tools either.
- A tool round: the generic user tools go to `toolConfig`. The typed tools go
  to `additionalModelRequestFields.tools`. The set of listed names includes
  the typed names, so no derived spec is made for a typed name. Otherwise
  Bedrock rejects the request (p7).
- If the `toolConfig` then has no tool, the placeholder (5.7) is added.
- The additional fields always carry `tool_choice: {type: none}` in this
  case. The early return in `apply_tool_choice_none` that gives
  `tool_choice_none: false` when the spec list is empty must not apply when
  typed tools are sent. A history with only `memory` calls must give
  tools = [placeholder], the typed `memory` entry, and the none choice.

The native path with `ToolChoice::None` and a structured format sends no
user tools today. It also sends no typed tools. After a tool round it keeps
the synthesized anchor with `synthesizing = true`, as today. No typed tool is
sent, so the model cannot call `memory`, and the anchor rule of 5.7 does not
apply.

The `TextHistory` strategy is for non-Claude families. The gate is closed
there, so it does not change.

### 5.9 Logging

`tracing::debug!` on the target `paigasus::bedrock::translate`:

- one line for each typed entry, with the tool name and the type (as in the
  Anthropic provider),
- one line when a tool with an `anthropic` declaration gets the generic
  spec, with the reason (5.2),
- one line when the placeholder is added (5.7).

## 6. Constraints and notes

### 6.1 Compatibility

- The default is `true`. A user who declares an `anthropic` `memory_*` type
  and runs the tool on Claude through Bedrock gets a different request after
  the upgrade. This is the intended change. The opt-out restores the old
  request.
- release-plz gives an additive `feat` a patch bump on a 0.x crate. The
  commit and PR text must state the change of default behaviour clearly,
  because it becomes the CHANGELOG entry.
- The public API change is additive: one new builder method.

### 6.2 Same name in both lists

Two `ToolDef`s with the same name are a caller error. Bedrock rejects them
(p7). The provider does not add a check for this.

### 6.3 Other typed tools

A tool that declares another `anthropic` type, for example
`computer_20251124` or `text_editor_20250728`, gets the generic `toolSpec`,
as today (5.2). The README says that only `memory_*` types are sent as typed
entries on Bedrock.

## 7. Tests

Unit tests (`translate/tools.rs`):

- The test `provider_type_is_ignored` changes. With the gate closed (the
  setting is `false`, or the family is not Anthropic), the result is the same
  as for a tool with no declaration. With the gate open, the tool goes to the
  typed list.
- The typed entry is exactly `{"type": "memory_20250818", "name": "memory"}`.
- `params` are merged. `type` and `name` in `params` do not override.
  `cache_control` is removed.
- A non-`anthropic` declaration gives the generic spec.
- An `anthropic` declaration with a type that is not `memory_*` (for example
  `computer_20251124`) gives the generic spec.
- The reserved name is rejected for a typed tool too.

Unit tests (`translate/tool_choice_none.rs`):

- Only a typed `memory` tool and a history with only `memory` calls give
  tools = [placeholder], the typed entry, and the none choice.
- A typed `memory` tool and a generic tool: no derived spec for `memory`.
- A typed variant of `none_shape_is_the_same_on_both_paths`.

Snapshot tests (`translate/mod.rs`):

Forced path (`us.anthropic.claude-sonnet-4-6`):

- A typed tool alone, turn 1: no `toolConfig`, `tools` in the additional
  fields.
- A typed tool and a generic tool: both lists, no `toolChoice` in
  `toolConfig`.
- `Required`, `Tool { name: "memory" }` and `Tool { name: <generic> }` with a
  typed tool: the choice is in the additional fields only.
- Structured output with a typed tool: the synthesized tool in `toolConfig`,
  the forced choice in the additional fields.
- A tool round with only typed tools: the placeholder is present.
- `ToolChoice::None` after a tool round with only a typed tool: the
  placeholder, no derived spec for the typed name, `tool_choice: none`.

Native path (`us.anthropic.claude-sonnet-5-5`):

- `JsonSchema` after a memory tool round, with only the typed tool: the
  placeholder, `synthesizing == false`, `outputConfig` present.
- `Required` with only a typed tool: no error, the `RequiredOmitted` notice.
- `Tool { name: "memory" }` with only a typed tool: no error, the
  `ToolOmitted` notice.
- `ToolChoice::None` with `JsonSchema` after a tool round, with a typed tool:
  no typed tools, the structured-output anchor as today.

Gate closed:

- A Nova id, and `native_tool_types(false)` on a Claude id: the request is
  the same as today.

Other unit tests:

- `has_forced_tool_choice` is `true` for an additional `any` or `tool`
  choice.
- The fallback: a rejected forced choice in the additional fields gives a
  retry on the native path.
- The stream: a typed `memory` call on the native path with a structured
  format gives `ToolCallDelta` and then `Finish(ToolCalls)`, with no error.
- The stream: a placeholder call with `synthesizing == false` gives a normal
  tool call and `Finish(ToolCalls)`.
- The builder: `native_tool_types` defaults to `true` and can be set to
  `false`.
- The facade test of 5.2 for the provider id.

Live tests (`tests/live.rs`, `#[ignore]`, env-gated like the other live
tests). Each test uses `ConverseStream`:

1. `Auto`, turn 1: the model calls `memory`. Turn 2 with that call and an
   empty-directory result: the request succeeds with a text answer.
2. Forced path: `ToolChoice::Tool { name: "memory" }`: the model calls
   `memory`. This proves the additional forced choice on `ConverseStream`.
3. Forced structured output (`JsonSchema`) with a typed tool: the request
   succeeds.
4. Native structured output (`JsonSchema`) after a memory tool round, with
   only the typed tool: the request succeeds with structured text.
5. `ToolChoice::None` after a memory tool round, with only the typed tool:
   the request succeeds with a text answer.

The tests assert a `memory` call, not one exact command. Before the PR, the
builder runs them on `us.anthropic.claude-sonnet-4-6` (tests 1, 2, 3, 5) and
on `us.anthropic.claude-sonnet-5-5` (tests 1, 4, 5) with the `maip-dev-admin`
profile, and records both runs in the PR body. Test 2 on Sonnet 5.5 is
expected to go through the SMA-839 fallback, so the builder may also run it
there and record the result.

## 8. Documentation

- `crates/paigasus-helikon-providers-bedrock/README.md`: a section "Provider
  tool types" with the typed memory path, the Claude-only gate, the
  `memory_*` rule, the opt-out, the placeholder (and the `Required` case of
  5.7), and the note that other types get the generic schema.
- `docs/book/src/concepts/model-providers.md`: the sentence "Bedrock does not
  use provider tool types." (near line 324) is replaced with two or three
  sentences on the typed memory path, with a link to
  `./tools.md#provider-tool-types`.
- `docs/book/src/concepts/tools.md`: the sentence that says Bedrock sends the
  generic schema (near line 139) is replaced. It now says that the Bedrock
  provider sends `anthropic` `memory_*` types to Claude models as typed
  entries.
- `CHANGELOG.md` entries come from release-plz. No version is bumped by
  hand, so the facade gets no manual bump.

## 9. Left to the builder

- The Rust shape of the additional-fields type and of the partition result.
- How the facade test reads the Bedrock provider id (5.2).
- The exact placeholder description text.
- The split of the work into commits.
