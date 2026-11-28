# SMA-869: a cancelled run ends as `Cancelled`, not `Completed`

Linear: [SMA-869](https://linear.app/smaschek/issue/SMA-869/a-cancelled-run-ends-as-completed-with-cut-output-when-the-model)

## Intent

A run that is cancelled through the `RunContext` cancel token must end with
`RunError::Cancelled` through a runner. Today it can end `Ok` with
`StopReason::Completed`. Then the output is cut text or the text of an earlier
turn, and `TokioRunner` stores that text in the session as a normal assistant
message. This is against the `Runner::run` rustdoc and
`docs/book/src/concepts/agent-loop.md`.

## Cause

`crates/paigasus-helikon-core/src/agent.rs` checks the token at one point
only: when `ModelTurnAccumulator::finish` returns `Err` (a cut stream with tool
calls, `agent.rs:1515`). A model that obeys the token ends its stream with no
`Finish` event. A text turn with no `Finish` returns `Ok` with the default
`FinishReason::Stop` (on purpose, for OpenAI-compatible servers that send no
`finish_reason`). The loop then emits `MessageOutput` and `RunCompleted`. A
real terminal event wins over a recorded cancel in `effective_interrupt`
(`runner.rs:779-788`), so the runner discards the cancel.

The workflow agents have a second gap. `SequentialAgent`, `ParallelAgent`,
`LoopAgent` and `GraphAgent` treat a child stream with no terminal event as a
success and then emit their own terminal event (`workflow.rs:204-243`,
`390-456`, `587-636`, `graph.rs:291-379`). Today the child emits
`RunCompleted` with cut text. After the loop fix, the child emits no terminal
event, so the workflow gap must close in the same change. Otherwise a
workflow with a token that already fired ends `Completed` even where it ends
`Cancelled` today (a model that does not end its stream on a pre-fired token).

## Goals

1. A cancel while the model streams text ends the run as `Cancelled`.
2. A cancel between two turns ends the run as `Cancelled`. No model call
   starts, and the turn checkpoint and the `OnTurnStart` hook do not run.
3. A cancel while a tool runs ends the run as `Cancelled`.
4. A cancel after `Finish(ToolCalls)` and before the tools start ends the run
   as `Cancelled`, and no tool starts (agreed in the brainstorm). The same is
   true for a handoff: no `OnHandoff`, no `HandoffItem`, no transfer result.
5. A run whose token fired before the run starts calls no model and ends as
   `Cancelled` (choice A, agreed in the brainstorm).
6. A resume with a token that already fired runs no approved deferred call.
   Each call gets the "did not run" result, so the deferral closes, and the run
   ends as `Cancelled`.
7. `AgentAsTool` returns a `ToolError` for a cancelled text-only sub-run.
8. `SequentialAgent`, `ParallelAgent`, `LoopAgent` and `GraphAgent` end with
   no terminal event when the token fired and a child ended with no terminal
   event. Through a runner, the run ends as `Cancelled`.
9. The MCP agent server returns `CallToolResult::error` for a cancelled run,
   not `success` with empty or earlier-turn text.
10. A model error (from `invoke` or from the stream) after the token fired
    ends the run as `Cancelled`, not `RunFailed(Model)`. This covers the Gemini
    `Auth::Token` case, where `invoke` returns `Err(Unavailable)` when the token
    fires during the token fetch (`providers-gemini/src/model.rs:92-95`).
    *(My call, see "Calls made in triage".)*
11. The session does not store the cut text as a completed assistant message.
12. A cancel that comes after a `Finish` event of a final text turn still ends
    `Completed`. The SMA-421/422 rule "a real terminal event wins" stays.

## Non-goals

- No public API change. No change to `transition` or `loop_state.rs`, so
  Temporal replay does not change.
- No change in the runner crates (runtime-tokio, runtime-axum, runtime-actix,
  runtime-agentcore). They already turn "the stream ended with no terminal
  event and the token fired" into `Cancelled`.
- runtime-temporal: `TemporalRunner` does not run the `LlmAgent` loop of
  `agent.rs`. Its `call_model_inner` (`activities.rs:98-112`) has its own
  `acc.finish()`. A client cancel is already correct, because the workflow's
  cancel branch wins (`runtime-temporal/src/workflow.rs:301-305`). A worker
  shutdown that cancels only the activity is out of scope.
- Core with no runner (`RunResultStreaming::collect` on the raw `Agent::run`
  stream) still returns `Ok` for a stream with no terminal event. This is
  documented. A change there is a separate decision.
- Live stream clients still see the `TokenDelta` text that the run then does
  not keep. The A2A task of runtime-agentcore keeps its `canceled` state, and
  its artifact can hold that streamed text (`a2a/rpc.rs:585-640`).
- No new span field and no usage record for a cancelled turn (see "Rejected
  findings").

## Anti-goals

- A cancel must not turn a run that already emitted a terminal event into
  `Cancelled`.
- A text turn with no `Finish` and no cancel must still complete as before.

## Approaches

| | What | Trade-off |
|---|---|---|
| **Token checks in the core loop and the core workflow agents (picked)** | Add early returns with no terminal event in `agent.rs`, and the `AgentAsTool` guard rule in the workflow agents and the MCP server. | Small, no public API. Each runner gets the fix through the existing "no terminal + token fired" rule. |
| A cancelled flag in `TransitionInput::ModelResponse` | The state machine decides. | Breaks the public API, needs a breaking cascade, breaks Temporal replay. Rejected in the ticket. |
| A runner check of the token before it accepts `RunCompleted` | Each runner overrides a terminal. | Breaks the SMA-421/422 precedence, must be copied into each runner, does not fix core or `AgentAsTool`. Rejected in the ticket. |

For the check before `model.invoke`, three options were considered:
**A** (before every call, picked), **B** (only after a tool turn, so the first
turn has a different rule), and **C** (no check before the call, so a model
that ignores the token can still complete).

## Design

Each check is "if `ctx.cancel().is_cancelled()`, return with no further
event". The `controlled` wrapper in runtime-tokio then records `Cancelled`,
and `effective_interrupt` reports it because no terminal event came.

### Loop driver (`agent.rs`)

1. **Resume batch.** Before `run_resolved_calls` (`agent.rs:1069-1195`, both
   the local path and the handoff-routed path): if the token fired, yield the
   "did not run" result (`resume_exec.rs:21`) for each resolved call and
   return. The recorder then closes the deferral, as
   `cancelled_resume_closes_the_deferral` (`runtime-tokio/tests/deferred.rs:897-929`)
   already expects for a cancel during the call.
2. **`TurnStarted` arm** (`agent.rs:1250`), before the turn checkpoint and the
   `OnTurnStart` hook. The tool results of the turn before are already yielded
   at this point, because they come first in the batch
   (`loop_state.rs:505-527`). Return without yielding `TurnStarted`.
3. **Before `model.invoke`** (start of the `NextAction::CallModel` arm, before
   the chat span is made, so no empty `gen_ai.chat` span is emitted). This
   covers the first turn (goal 5), the `RepairStarted` path that has no
   `TurnStarted`, and models that ignore the token.
4. **`model.invoke` returns `Err`**, and the token fired: return.
5. **The stream yields `Err`**, and the token fired: return.
6. **After the stream ends, before `acc.finish()`**: if the token fired and
   `acc.finish_seen()` is `false`, return. This covers goals 1 and 3. The
   existing check in the `Err` branch of `acc.finish()` stays.
7. **Before `run_tools_concurrent`** (start of `NextAction::ExecuteTools`).
8. **Start of `NextAction::Handoff`**, before `OnHandoff`. The recorder then
   closes the transfer calls (`session.rs:999-1012`).

A turn that ended with `Finish` and then sees a cancel is not changed by
check 6. If it is the final text turn, the run ends `Completed` (goal 12). If
it holds tool calls, check 7 or 8 stops the run.

A handoff child runs the same loop with the same token (`handoff_child`), so
the child gets the same checks. The parent re-yields the child's events. A
child with no terminal event gives a parent with no terminal event. `SwarmAgent`
passes the stream through in the same way (`swarm.rs:316-360`).

### Workflow agents (`workflow.rs`, `graph.rs`)

After a child stream drains in `SequentialAgent`, `ParallelAgent`, `LoopAgent`
and `GraphAgent`: if the token fired and the child emitted no terminal event,
return with no further event. This is the rule of the `AgentAsTool` guard
(`agent_as_tool.rs:197`). For `ParallelAgent`, apply it after all branches
drain, if any branch had no terminal event.

### `AgentAsTool`

No code change. With the loop fix, a cancelled text-only sub-run has no
terminal event, so the existing guard returns a `ToolError`.

### MCP agent server (`paigasus-helikon-mcp/src/server.rs`)

After `collect`, apply the same guard: if the token fired and the run had no
terminal event, return `CallToolResult::error` with a text such as "agent run
was cancelled" (`server.rs:331-367`).

## Tests

Each new behaviour test must fail on the current `main` and pass with the fix.
A test that passes on both is a regression guard and is labelled as one.

Test-design rules (from the challenge):

- Fire the **run** token: a captured clone of the `RunContext` token, or a
  hook. `ToolContext::cancel()` is a child token (`context.rs:840-858`), so a
  tool that cancels it does not fire the run token.
- Do not let the test pass through the `cancelled()` branch of `controlled`
  (`runtime-tokio/src/lib.rs:75-78`). The model or tool must end on the token
  without a pending poll, so that the agent-side check decides.

Tests, through `TokioRunner::run` and `run_streamed` where it applies:

- case 1: cancel during the text stream → `Err(RunError::Cancelled)`.
- case 2: cancel between two turns → `Cancelled`, the model gets no second
  call, and a turn checkpoint does not run.
- case 3: cancel while a tool runs → `Cancelled`.
- a cancel after `Finish(ToolCalls)` → `Cancelled`, and the tool does not run.
- a cancel after `Finish(ToolCalls)` with a transfer call → `Cancelled`, no
  `HandoffItem`.
- a pre-fired token → `Cancelled`, no model call. This replaces
  `prefired_cancel_still_completes_ready_run`
  (`runtime-tokio/tests/run_control.rs:65`), which pins the old behaviour.
  Rename it and change it.
- a pre-fired token on a resume with an approved call → `Cancelled`, a tool
  count of 0, and a stored "did not run" result.
- a model `invoke` error and a stream error after the token fired →
  `Cancelled`.
- **goal 12**: a model that yields `TokenDelta`, then `Finish(Stop)`, then
  calls `cancel()` and ends the stream → `Ok`, `StopReason::Completed`, the
  full text, and a stored assistant message. Mutation-check it: the test must
  fail when `!acc.finish_seen()` is removed from check 6.
- the session holds no assistant message with the cut text (case 1).
- one test for each workflow agent (Sequential, Parallel, Loop, Graph) with a
  pre-fired token → `Cancelled`.
- core, `AgentAsTool`: a cancelled text-only sub-run gives a `ToolError`.
- MCP server: a cancelled run gives `CallToolResult::error`.
- regression guards: the SMA-421 tests (`CancelOnRunCompleteHook`) still end
  `Completed`, and a text turn with no `Finish` and no cancel still completes.

## Documentation

- `docs/book/src/concepts/observability-evaluation.md:338-353`: the
  paragraph "Cancellation is a *model*-boundary event" says that the loop
  treats a cut turn as complete and runs its tool call. Both parts are false.
  The text about a pre-cancelled `invoke` that uses its script is true for the
  model, but through an agent a cancelled turn now makes no `invoke`. Say so.
- `crates/paigasus-helikon-evals/README.md:53`: the same point about the
  script.
- `docs/book/src/concepts/agent-loop.md`: line 69 (terminal events) and line
  149 (best-effort cancel). Say that a token that fired before a model call, a
  tool batch, a handoff or a resume batch ends the run as `Cancelled` through a
  runner, and that only a terminal event that already happened wins.
- `docs/book/src/concepts/runtimes.md:64`: the stored turn is "whatever the
  run had produced". Say that a cut turn is not stored as complete.
- The `Runner::run` rustdoc, if it describes the pre-fired case.
- The core CHANGELOG is written by release-plz from the `fix(core)` commit.
  The MCP crate gets its own patch bump from its file change.

## Calls made in triage

- **Goal 10 (a model error after the token fired is `Cancelled`).** The
  challenger proposed this for the `invoke` error only. I extended it to the
  stream error too, so that both model error paths have one rule. The risk: a
  real provider error that happens at the same time as a cancel is reported
  as `Cancelled`. Reject this goal if you want model errors kept as
  `RunFailed(Model)`.
- **Workflow agents and the MCP server are in scope**, although the ticket
  proposed `agent.rs` only. Without the workflow change, the loop fix makes a
  pre-fired cancel through a workflow worse than today.

## Rejected findings

- **A trace mark for a cancelled exit** (a `paigasus.cancelled` span field and
  the usage of the cut turn on `gen_ai.chat`). This adds to the observability
  contract and is not needed to fix the run result. File it as a follow-up if
  you want it.
- **Make `collect` with no runner return an error for a stream with no
  terminal event.** This changes a documented public behaviour. Out of scope.
- **Stop before `OnRunStart` and the input guardrails on a pre-fired token.**
  The first model call does not start, which is the goal. The runner can still
  store the user input of a run that did nothing. This is accepted.
- **Is a `fix(core)` patch bump correct for a behaviour reversal?** Yes. The
  old behaviour is a bug against the documented contract.
  `prefired_cancel_still_completes_ready_run` pinned that bug.

## Left to the builder

- The model helpers for the tests (a mock that obeys the token and stops with
  no `Finish`, a mock that cancels after `Finish`), and where they live.
- Whether other tests in the workspace depend on a run that completes with a
  token that already fired. Run the full workspace tests to find them.
