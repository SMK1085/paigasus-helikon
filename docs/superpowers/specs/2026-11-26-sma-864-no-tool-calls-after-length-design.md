# SMA-864: do not run tool calls from a turn that ended with `Length` or `ContentFilter`

Status: approved by Sven (2026-11-27), with a final sweep (§10) folded in. Linear: SMA-864.
Size: a small change.

## 1. Intent

The agent loop runs every `Item::ToolCall` in a model turn. It does not look at
the finish reason. When `max_tokens` cuts a turn inside a `tool_use` block, the
Anthropic and Bedrock translators give a named call with `""` arguments. Core
changes `""` to `{}` (`crates/paigasus-helikon-core/src/model.rs`, the
`args_str.trim().is_empty()` rule). The loop then runs the tool with `{}`. The
model did not mean that input.

A content filter can also stop a turn while a tool call is open. The arguments
of that call are not reliable either.

When the cut comes inside the JSON arguments, the problem shows differently:
`ModelTurnAccumulator::finish` fails with "invalid tool args" before the loop
sees the turn, and the run fails with `AgentError::Other`. For every provider
that streams JSON arguments (Anthropic, Bedrock, OpenAI Chat, OpenAI Responses,
LiteLLM), this is the common case. The caller must get one typed error for
both cases.

## 2. Rule

### 2.1 In `transition`

When a model turn in the `CallingModel` state has one or more `Item::ToolCall`
items **and** its `finish_reason` is `FinishReason::Length` or
`FinishReason::ContentFilter`, the loop:

1. runs **no** tool call and **no** handoff from that turn. The check comes
   after the max-turns arm and before the tool-call arm (which holds the
   handoff check);
2. emits `AgentEvent::MessageOutput` for each `Item::AssistantMessage` of the
   turn, in item order, as the other arms do;
3. emits **no** `AgentEvent::ToolCallItem`. So the session stores the assistant
   message and no `tool_call` or synthesized `tool_returned` record;
4. emits `AgentEvent::RunFailed { error }`, where `error` is the `Display` text
   of the new error;
5. goes to `LoopState::Failed(AgentError::ToolCallsNotRun { .. })` with
   `NextAction::Terminate`.

The rule applies to the whole turn. If a `Length` turn has two complete calls
and one cut call, the loop runs none of them, because one turn's calls are one
unit. The rule also applies to a handoff transfer call. A transfer call takes no
arguments, so a cut transfer call can still be correct. The run fails all the
same: the turn hit its limit, and a simple rule is easier to predict.

**Accepted false positive.** Gemini sends each function call whole, and OpenAI
Responses closes a call with `output_item.done`. A `Length` turn from these
providers can hold complete, valid calls. The run fails for them too. This is
intended: the caller sees a typed error and can raise `max_tokens`.

OpenAI Responses maps `Status::Incomplete` with no `incomplete_details` to
`Length`. The guard blocks that turn too. This is intended, because an
incomplete response is a cut response.

All other finish reasons keep today's behavior. This includes `Stop` and
`Other(_)` (for example `Other("function_call")` from OpenAI Chat) with tool
calls: the tool calls run.

### 2.2 In the local driver (reassembly failure)

In `crates/paigasus-helikon-core/src/agent.rs`, when `acc.finish()` returns
`Err` **and** the accumulator saw a `Finish` of `Length` or `ContentFilter`,
the driver fails with `AgentError::ToolCallsNotRun` (with the tool names that
the accumulator holds) instead of `AgentError::Other`. The `RunFailed` event
carries the `Display` text of that error. A reassembly error under any other
finish reason keeps `AgentError::Other`.

This needs additive accessors on `ModelTurnAccumulator`, for example
`finish_reason(&self) -> &FinishReason` and a way to read the names of the
accumulated tool calls. The exact shape is the builder's call.

The Temporal `call_model` activity (`runtime-temporal/src/activities.rs`) also
fails on a reassembly error. It keeps its current mapping: Temporal maps an
unknown `AgentError` variant to `Other { message }` anyway (see §6).

## 3. New public API

```rust
/// The model turn did not end cleanly (`Length` or `ContentFilter`), so the
/// loop did not run its tool calls.
#[error("tool calls not run: the model turn ended with {finish_reason:?} ({})", capped_names(tool_names))]
#[non_exhaustive]
ToolCallsNotRun {
    /// Why the model turn stopped. `None` when the stream ended with no
    /// `Finish` event (see §10.2).
    finish_reason: Option<FinishReason>,
    /// The names of the tool calls that did not run, in item order (core
    /// sorts tool calls by `call_id`).
    tool_names: Vec<String>,
},
```

- `AgentError` is `#[non_exhaustive]`, so the new variant is an additive change.
  No crate outside core matches `AgentError` without a wildcard.
- The variant is `#[non_exhaustive]`, so a later `call_ids`, `usage` or
  `Option<FinishReason>` field is not a breaking change.
- The `Display` text caps the name list in the same way as
  `format_blank_names` in `model.rs` (8 names, 64 characters each). Tool names
  are model output, and this text goes to `RunFailed`, spans, AG-UI, Temporal
  and, through `AgentAsTool`, to a parent model. The field keeps the full list.
  The builder can reuse or share the existing helper.

## 4. Approaches

- **Chosen: a guard arm in the pure `transition` function**
  (`crates/paigasus-helikon-core/src/loop_state.rs`). Only `agent.rs` and
  `runtime-temporal/src/driver.rs` call `transition`, so one change covers both
  runners. §2.2 adds a small mapping in the local driver for the reassembly
  case, which never reaches `transition`.
- Rejected: a check only in the `agent.rs` driver. The Temporal runner would
  still run the cut calls.
- Rejected: a check in each provider. That puts a loop rule into six crates,
  and a new provider can forget it.
- Rejected: run tool calls only for `FinishReason::ToolCalls` (an allow-list).
  OpenAI Chat maps `function_call` to `Other("function_call")`, and many
  OpenAI-compatible servers behind LiteLLM send `"stop"` with valid tool calls.
  An allow-list would stop those runs.
- Rejected (Sven chose option 1): do not run the calls, send an error result
  for each call, and call the model again. The next turn has the same token
  limit, so this can repeat until `max_turns`.

## 5. Rollout: replay-breaking for Temporal

`transition` runs inside Temporal workflow code. A workflow history where
`call_model` returned `Length` + a tool call already holds an `invoke_tool`
activity. On replay with the new code, `transition` gives `Failed` at that
point, and Temporal raises a non-determinism error. The runtime-temporal crate
docs ("Upgrade Discipline and Determinism") say that every release which
changes `transition` behavior is flagged as replay-breaking.

release-plz gives an additive change on a 0.x crate only a patch bump, so a
plain `cargo update` can bring this change into a running worker fleet.

So the PR adds an `### Upgrade notes` entry by hand under `## [Unreleased]` in
both `crates/paigasus-helikon-core/CHANGELOG.md` and
`crates/paigasus-helikon-runtime-temporal/CHANGELOG.md`. The entry says:

- the change is replay-breaking for in-flight Temporal runs that had a
  `Length` or `ContentFilter` turn with tool calls;
- drain in-flight runs, or use a blue-green task queue, before the upgrade;
- a `Length` or `ContentFilter` turn with tool calls now fails with
  `AgentError::ToolCallsNotRun` (or `AgentError::Other` with the same text on
  Temporal) instead of running the calls.

The builder checks that release-plz keeps hand-written `[Unreleased]` entries
(earlier releases carry such notes, for example core 0.7.x).

## 6. Non-goals

- `Length` or `ContentFilter` **without** a tool call does not change. The run
  ends with the text it has.
- The `Finalizing` and `RepairingOutput` states do not change. They already
  treat any tool call as a validation failure.
- A stream that ends with no `Finish`, and the provider stop-reason mappings, were non-goals in the first draft. Sven folded them into this PR as a final sweep (§10).
- The failed state carries no token usage. No `LoopState::Failed` arm keeps
  usage today. The per-turn `gen_ai.chat` span still records the usage of the
  cut turn. A `usage` field can come later (the variant is `#[non_exhaustive]`).
- The Temporal runtime keeps its error mapping.
  `ErrorKindPayload::from_agent_error` maps the new variant to
  `Other { message }`. That is lossy, so a Temporal caller cannot match the
  variant. A typed payload is a separate change; it must then bump core in the
  same PR (CLAUDE.md rule).
- A deferred call that a session stored before the upgrade runs on resume
  without `transition`. A human approved that call, so this does not change.

## 7. Tests

In `crates/paigasus-helikon-core/tests/transition_unit.rs` (the existing home of
the pure `transition` tests):

1. `Length` + one `ToolCall` gives `LoopState::Failed(ToolCallsNotRun {
   finish_reason: Some(Length), tool_names: ["<name>"], .. })`,
   `NextAction::Terminate`, a last event `RunFailed`, and no `ToolCallItem`.
   This is the acceptance-criteria test.
2. The same for `ContentFilter`.
3. A mixed `Length` turn with three calls: none runs, `tool_names` has all
   three in item order.
4. A `Length` turn whose only tool call is a handoff transfer call: no
   `ApplyingHandoff`; the run fails.
5. Assistant text + a cut call: `MessageOutput` comes before `RunFailed`.
6. Regression: `Stop` + a tool call still goes to `ExecutingTools`.
7. Regression: `Other("function_call")` + a tool call still goes to
   `ExecutingTools` (this is the case that rules out an allow-list).

End to end through the local driver (`crates/paigasus-helikon-core/tests/`,
with the scripted mock model and `MockTool` from `tests/common`):

8. A scripted turn with a tool call with no argument delta and `Length`:
   `MockTool::invocations()` is empty, and the run returns
   `RunError::Agent(AgentError::ToolCallsNotRun { .. })` through the failure
   slot.
9. A scripted turn with partial JSON arguments and `Length` (the §2.2 path):
   the same assertions.
10. The session holds the assistant message and no `tool_call` or
    `tool_returned` record for the cut call.
11. A `Display` test: more than 8 names, or a name longer than 64 characters,
    gives the capped text.

In the Temporal driver unit tests (`crates/paigasus-helikon-runtime-temporal/src/driver.rs`):

12. `apply_model` with a `ModelTurn` of `Length` + one `ToolCall` gives
    `DriverEffect::Finished` with `RunStatusPayload::AgentFailed(
    ErrorKindPayload::Other { .. })`, and never `ExecuteTools`.

## 8. Docs

- The new variant and any new accessor need a `///` comment (`missing_docs`).
- `docs/book/src/concepts/agent-loop.md` gets a sentence about this rule in its
  failure-mode text.
- The core README has no text about loop behavior or error variants, so it
  needs no edit.
- The two CHANGELOG upgrade notes in §5.

## 9. Left to the builder

- The exact `#[error]` text and the name-cap helper.
- The shape of the accumulator accessors in §2.2.
- Where the guard arm sits, inside the order that §2.1 sets.

## 10. Final sweep (folded in by Sven, 2026-11-27)

Sven asked to fold the two deferred items into this PR as a final phase,
instead of follow-up issues. The phase runs after §2-§8 are done and green.

### 10.1 Provider stop-reason sweep

Sweep every provider translator (Anthropic, Bedrock, Gemini, OpenAI Chat,
OpenAI Responses, LiteLLM) for wire stop reasons that cut the output or filter
it, but map to `Stop` or `Other(_)` today. Map each one to `Length` (output cut)
or `ContentFilter` (filter or guardrail). Known items:

- Anthropic and Bedrock: `model_context_window_exceeded` → `Length`. Change the
  Bedrock test that pins `Other("model_context_window_exceeded")`.
- LiteLLM: `guardrail_intervened` → `ContentFilter` (Bedrock already does
  this).

For each other wire value, the builder checks the vendor reference and records
the call in a table in this section (value, provider, old mapping, new mapping,
source). A value that is not clearly a cut or a filter stays as it is.

This changes a public value (`ModelTurn::finish_reason`) for those stop
reasons. Each touched provider crate gets an upgrade note in its CHANGELOG.

### 10.2 A stream that ends with no `Finish`

Today `ModelTurnAccumulator` sets `Stop` when no `Finish` comes, so a stream
that the server closes early can run a named call with `{}`.

Rule: the accumulator records whether it saw a `Finish`. When `finish()` runs
with one or more tool calls and no `Finish` was seen, it returns `Err` (a
reassembly error). The local driver maps this error to
`AgentError::ToolCallsNotRun { finish_reason: None, .. }` through the §2.2
path. A turn with no tool calls and no `Finish` keeps today's behavior
(`Stop`).

Why this shape (my call): it adds no field to `ModelTurn` or to
`TransitionInput::ModelResponse`, so there is no breaking change and no
Temporal payload change. On Temporal, the `call_model` activity fails with the
error; its result is in the history, so replay does not change.

Risk: a test mock or a provider that sends tool calls with no `Finish` now
fails. The builder finds each such site and fixes the mock or the provider.
The `Model::invoke` contract already requires `Finish` when a stop reason was
seen.

Tests: an accumulator unit test (tool call, no `Finish` → `Err`; text only, no
`Finish` → `Ok` with `Stop`), and an end-to-end driver test (`MockTool` not
invoked, `ToolCallsNotRun { finish_reason: None, .. }`).
