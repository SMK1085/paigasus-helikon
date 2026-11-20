# SMA-810: a tool result for the handoff transfer call

Linear: [SMA-810](https://linear.app/smaschek/issue/SMA-810). Size: small change (short design).

## Intent

In a run with a handoff, the transfer tool call gets no tool result in the event stream.
`SessionRecorder::drain` then closes the call with a synthetic `ToolReturned` at the **end**
of the log, after all child events. When the session goes to a model again, the transfer
call and its result are far apart. Some providers require the result directly after the
call, and they reject the request.

SMA-803 adds a second effect. A provider can give the same call id to the transfer call
and to a deferred call of the child (the Gemini fallback id `fc_{n}`). The late transfer
result then closes the deferral, so SMA-803 denies such a deferral ("deferral not
possible: call id {id} repeats in this run"). When the transfer result comes directly
after the handoff, this deny is not necessary.

**Release state.** `paigasus-helikon-core` 0.6.0 is on crates.io (published 2026-11-20).
It contains SMA-803. So the deny and the old log layout (late transfer result) are
already in a release, and sessions with the old layout can exist.

## Goals

- G1. The parent emits a `ToolOutputItem` for each transfer call of the handoff response.
  The stored log of a handoff run is then:
  `[AssistantMessage]` `ToolCalled(t)` [`ToolCalled(u)` …] `HandoffOccurred`
  `ToolReturned(t)` [`ToolReturned(u)` …] … child events.
  `HandoffOccurred` projects to no item. So in the projected conversation the results come
  directly after the transfer calls of the same response, in call order.
- G2. Remove the SMA-803 deny for a repeated call id. A handoff child can defer a call
  that has the same call id as a transfer call above it, at any depth.
- G3. Tests check the adjacency on two levels:
  - **Stored log:** between a transfer `ToolCalled` and its `ToolReturned`, only
    `HandoffOccurred` and the other transfer calls and results of the same response occur.
  - **Projection:** every `ToolCall` has its `ToolResult` in the same contiguous tool
    block.

  Limit: under `ParallelAgent`, branch events interleave live (`workflow.rs`). The parent
  awaits `OnHandoff` between `ToolCallItem(t)` and `ToolOutputItem(t)`, so a sibling
  branch's items can be recorded between them. This is already true today for every tool
  call in a parallel branch. G3 applies to runs without `ParallelAgent`.

  Sven accepted on 2026-11-20 that "next to it" means next to it in the projected
  conversation (the model request), not in the raw log.
- G4. **Read-time repair of old sessions** (added at Gate 1 on Sven's request).
  `project()` moves the late result of each **transfer call** to directly after the
  transfer calls of its handoff response, and replaces the synthetic cancel text with
  the transfer text. A session stored before SMA-810 (any earlier core version) then
  projects as a new-layout session, with the exceptions listed in the G4 design. Every session backend (in-memory, SQLite, Postgres, Redis) builds
  its snapshot with `core::project`, so the repair covers all of them. The stored log
  does not change. No other result moves.
- G5. **AG-UI call state and AG-UI id per call occurrence** (added at Gate 1
  on Sven's request). The AG-UI `EventMapper` keys `call_names`, `completed_calls` and
  `buffered_calls` by call id for the whole run. A provider that repeats call ids across
  model streams (Gemini `fc_{n}`) then gets a wrong tool name, a missing call, or merged
  arguments. Also, a client that keys on `toolCallId` merges two calls with one id. The
  mapper must give each call occurrence its own state and its own AG-UI id.

## Non-goals

- The stored log of an old session does not change. G4 repairs only the projection.
- The child transcript does not change. `thread_handoff_transcript` still drops all tool
  items, so the child never sees the transfer call or its result.
- Non-transfer calls in a handoff response stay out of scope. `transition()` drops them
  from the events, so the session does not record them. Their `ToolCallDelta`s can still
  reach AG-UI as START/ARGS with no result. This is not caused by this change.
- No change to the Temporal driver. It does not support handoffs.

## Anti-goals

- AG1. A "transferred" result for a handoff that did not occur. The result must come only
  after `OnHandoff` permits the handoff, the target agent is known, and the target run
  started.
- AG2. A change that breaks the resume of a session that core 0.6.0 stored (old layout).
- AG3. A second result for a transfer call (for example on a resumed run).

## Approaches

1. **Emit the result after `HandoffItem`, in the handoff arm of `agent.rs` (chosen).**
   The result comes after the `OnHandoff` veto, the depth check and the target lookup.
   `transfer_calls_before` (`deferral.rs`) scans back from `HandoffOccurred` over
   `ToolCalled` and `AssistantMessage` only. A result after `HandoffOccurred` keeps this
   scan valid for old and new logs.
2. Emit the result in `transition()`. Pure and easy to unit-test, but it runs before the
   `OnHandoff` veto and the target check, and it puts the result before
   `HandoffOccurred`, which breaks `transfer_calls_before`. It also writes a result before
   a swarm can drop the handoff. Rejected.
3. Fix only `SessionRecorder` (write a result on `HandoffItem`). The live event stream
   keeps the orphan call, so AG-UI clients still see a call with no result. Rejected.

Sven chose approach 1 on 2026-11-20.

## Design

### Emission (`crates/paigasus-helikon-core/src/agent.rs`, `NextAction::Handoff` arm)

The event order of a successful handoff becomes:

1. `ToolCallItem` for each transfer call (from `transition()`, unchanged).
2. Depth check, target lookup, `OnHandoff` (unchanged). A failure ends the run as today.
3. `HandoffItem { from, to }` (unchanged).
4. `target_agent.run(child, input)` is awaited. This only builds the child stream; it
   yields no event. (Today this call comes after step 6. Moving it earlier changes no
   observable order, because it emits nothing.)
5. **New:** one `ToolOutputItem` per transfer call, in the order of the `ToolCallItem`
   events.
   - On `Ok`: the first transfer call (the call that names the target) gets
     `transferred to {target}`. Each other transfer call of the same response gets
     `transfer not performed: this response already transferred to {target}`.
   - On `Err`: every transfer call gets `transfer failed: {target} did not start`, and
     the run ends with `RunFailed` as today. The text does not include the error, because
     the result goes to a model and the error can hold internal detail.
6. `AgentUpdated { agent: target }` (on `Ok` only), then the child events.

The content of each result is one `ContentPart::Text`. The parent does not push the
results into its own `conversation`: after the handoff arm, the parent only re-yields
child events and returns, so that state is never read.

The transfer call ids are the ids that `agent.rs` already collects as
`transfer_call_ids` from the `ToolCallItem` events of the handoff transition.

The early exits in step 2 (depth exceeded, unknown target, `OnHandoff` deny) emit no
result. The run ends there with `RunFailed`, so the recorder's synthetic result is the
last event, directly after the call. This is the behaviour of today.

### Remove the repeated-id deny

- Delete the `open_call_ids` check in the `Authorization::Defer` arm (`agent.rs`).
- Delete `RunContext.open_call_ids`, `open_call_ids()`, `with_open_call_ids()` and their
  copies in `handoff_child` / `subagent_child` (`context.rs`). They are `pub(crate)` and
  have no other use, so this is not an API change. Delete the unit test
  `handoff_child_keeps_open_call_ids_and_subagent_child_clears_them`.
- `transfer_call_ids` stays: step 5 uses it.

### Read-time repair in `project()` (G4, `crates/paigasus-helikon-core/src/session.rs`)

The second spec challenge rejected a first version that moved every result to the end
of its "tool block". A tool-only model response makes no `AssistantMessage`
(`model.rs` `build_items`), so one block can hold several responses, and the OpenAI and
LiteLLM translators then still send an invalid request. That version also moved results
in `ParallelAgent` logs with repeated ids, where it could swap the content of two
results. This version moves **only transfer results**, to a fixed place.

**Step 1: find the transfer calls.** For each `HandoffOccurred` at index `k`, the
transfer calls are the `ToolCalled` events in the run of `ToolCalled` /
`AssistantMessage` events directly before `k`. This is the scan of
`transfer_calls_before` (`deferral.rs`). The two places share only this boundary scan
in one private helper. (`transfer_calls_before` also counts `later_calls` with a forward
scan per call; `project()` must not call that part, or it becomes O(n × calls).)

Scope: step 1 is exact for **sequential** logs. In a sequential log the scan stops at
the resume block, at `UserMessageInjected`, at recorded input and at an earlier
`HandoffOccurred`, and `transition()` never records the non-transfer calls of a handoff
response. Two cases can put another call into the scanned run: a `ParallelAgent`
sibling's `ToolCalled` (the `OnHandoff` await is a yield point), and a run whose new
input ends with a `ToolCall` item. G4 does not support these cases (same limit as G3).

**Step 2: find the result of each transfer call** (one O(n) pass over the events). Keep
the list of open calls, each with a flag "transfer". A `ToolReturned` or `ToolDeferred`
with call id `x` closes:

1. the most recent open **non-transfer** call with id `x`, if one exists;
2. else the earliest open **transfer** call with id `x`.

Why: in the old layout a child call can repeat the transfer call id (Gemini `fc_0`).
The log is then `TC fc_0 (transfer)`, `HandoffOccurred`, `TC fc_0 (child)`,
`TR fc_0 (child)`, …, `TR fc_0 (late cancel)`. The rule gives the child's result to
the child's call and the late cancel to the transfer call. The rule changes nothing for
non-transfer calls, because non-transfer results do not move.

**Step 3: project as today, and keep the source.** `project()` keeps a parallel list
`sources` with the event index of each message. `Compacted` truncates `sources` with
`messages`. (A `Compacted` event counts **events**, so the repair must not reorder
events before projection: that would change which events a later `Compacted` removes.)

**Step 4: move, after projection.** For each handoff response whose transfer calls are
still in `messages`: the target position is directly after the last transfer call
message of that response. Each transfer result that is in `messages` but not at its
target moves there, in the order of its call. In one O(n) rebuild pass (no repeated
`Vec::remove`/`insert`):

- A result that is already at its target (the new layout) does not move.
- A result whose call was removed by `Compacted` stays where it is (today's behaviour).
- A result removed by `Compacted` while its call stays: the call keeps no result, as
  today.

**Step 5: correct the text of a moved late result.** If a moved result has exactly
the synthetic cancel text (`tool call did not complete (run cancelled/timed out)`), the
repair replaces it with the G1 text: `transferred to {to}` for the first transfer call
of the response (`to` comes from the `HandoffOccurred` event), and `transfer not
performed: this response already transferred to {to}` for each other call. Each level
uses the `to` of its own `HandoffOccurred`.

Why this is true: a result moves only when a recorded event lies between the transfer
call's handoff and the result. In a sequential log, such an event comes from the target
agent, so the target started and the transfer occurred. (`SequentialAgent` stops on a
failure; only `ParallelAgent` can break this, see the scope note in step 1.) When
`target_agent.run()` failed in the old code, the log is `TC t, HO, TR t(cancel)`: the
result is already at its target, so it does not move and keeps the cancel text.
Without this step, the model sees "did not complete" after a transfer that succeeded,
and it can call the transfer tool again. A result with any other text keeps its text.

The cancel text is one `const` in `session.rs`. `cancelled_result`, the repair and the
T7 helper all use it.

The repair is one named private function (for example
`repair_legacy_transfer_results`), so it is easy to find and to test. It has no removal
date: sessions stored before SMA-810 can live for a long time.

Exceptions (an old log does not give exactly the new layout):

- An old log where `target_agent.run()` failed keeps the cancel text. The new code
  writes `transfer failed: {target} did not start`.
- An old successful handoff whose child recorded no event at all keeps the cancel text
  (no event lies between, so the result does not move and is not rewritten). The model
  can still call the transfer tool again in this rare case.

Properties:

- A log that has no `HandoffOccurred`, and a sequential new-layout log, project exactly
  as today. This includes `ParallelAgent` logs with repeated ids and no handoff.
- Final-review hardening (2026-11-20): a `ToolReturned` in the run directly after a
  `HandoffOccurred` closes a transfer call of that response first. A result whose text
  is **exactly** one of the three transfer texts for that handoff's target closes an open
  transfer call with its id before any non-transfer call (a prefix match was replaced at
  the local review, so a child tool output such as `transferred to archive` keeps its
  own call). So in a new-layout `ParallelAgent` log where a sibling call with the same id
  comes between the handoff and its result, the repair moves the transfer result next to
  its call instead of swapping two results.
- Local-review hardening (2026-11-20): in a new-layout response (a run of `ToolReturned`
  events directly after the `HandoffOccurred`), the transfer calls are only the scanned
  calls that a result in that run closes. Another `ToolCalled` in the scanned run (a
  `ParallelAgent` sibling's open call) is a non-transfer call, and its result never
  moves. Known limit: `transfer_calls_before` (`deferral.rs`, used by `ResumePlan`) still
  counts every scanned call as a transfer call. The two rules differ only for such a
  `ParallelAgent` sibling call, which is outside the G4 scope.
- The repair adds and removes no message, so the message count stays the same.
  (`CompactingSession` checks `len()`, which does not change. Step 5 changes the text
  length, so its token estimate can change slightly. Tests must not assert equal token
  counts before and after the repair.)
- After the repair, a transfer call and its result are adjacent in the sense of
  `close_open_transfers` (result directly after the transfer calls of its response). So
  FIFO (`paired_result`, `agent_view`) and the step 2 rule give the same pairs for
  transfer calls. `close_open_transfers` keeps its "move" branch as a defence for a
  custom `Session` that overrides `snapshot()` without `project()`.
- `agent_view` (`deferral.rs`) calls `project()` per log segment. A segment that does
  not contain the `HandoffOccurred` of a transfer call does not move that call's result.
  The child segment drops the leading transfer result as today (it closes no open call).
- `ResumePlan::from_events` never maps snapshot indices to event indices. The repair
  moves results only to earlier positions and never reorders `ToolCall` items, so
  `CallNotInHistory` and `nth(later_calls)` keep their answers.
- `project()` emits one `tracing::debug!` (target `paigasus::core::session`) with the
  number of moved results when the number is not zero.
- The rustdoc of `project` states the repair, and that the snapshot order no longer
  follows the event order one-to-one for old-layout handoff logs.

### AG-UI call state (G5, `crates/paigasus-helikon-runtime-agentcore/src/agui/map.rs`)

Two spec challenges shaped this section. The first showed that removing state at
`ToolOutputItem` is not enough, and that two spans with one `toolCallId` break the
mapper's own client contract ("one call id maps to exactly one span", module doc). The
second showed that a reset at `TurnStarted` makes `ParallelAgent` output worse for every
provider: a sibling branch's `TurnStarted` can arrive between a call's deltas and its
`ToolCallItem`, and a reset there splits one call into two or renders it as `"unknown"`.
So G5 has **no reset at `TurnStarted`**. It starts a new occurrence of a provider id
only when the events prove that a new call started.

**Call occurrences.** The mapper keys all per-call state (`call_names`,
`completed_calls`, `buffered_calls`, `open_call`) by the **AG-UI id** of the occurrence,
not by the provider id. It keeps:

- `current: HashMap<String, Occurrence>`: provider id → the latest occurrence (its
  AG-UI id, whether a name-carrying delta was seen, whether its `ToolCallItem` was seen).
- `used_ids: HashSet<String>`: every AG-UI id that the run already emitted.

A new occurrence of provider id `x` starts when:

1. a `ToolCallDelta` for `x` with `name: Some(..)` arrives, and the current occurrence
   of `x` already had a name-carrying delta (the core contract gives `name: Some`
   exactly once per call, `model.rs`; the mapper already relies on this);
2. a `ToolCallItem` for `x` arrives, and the current occurrence of `x` already had its
   `ToolCallItem` (a non-streamed call that reuses the id);
3. there is no current occurrence of `x`.

Local-review changes (2026-11-20):

- Rule 1 also fires for **any** `ToolCallDelta` for `x` (named or not) when the current
  occurrence of `x` already had its `ToolCallItem`. The core contract lets the name
  arrive on a later delta than the first argument chunk, so an unnamed first delta of a
  reused id belongs to the new call. Unnamed deltas before the item stay on the existing
  occurrence (a revisit of a closed span).
- For such a new occurrence of a **reused** id, the mapper holds back the arguments
  until the name arrives, then opens the span with the right name and sends the held
  arguments. A first-use id streams exactly as before (the span opens on the first
  delta).

PR review changes (2026-11-20):

- At `HandoffItem`, the mapper only counts the handoff. It does not flush or retire a
  call. Each occurrence records the count when it starts.
- An unnamed delta for `x` is held when the current occurrence of `x` had no item and
  started before the latest `HandoffItem`. More unnamed deltas for `x` join the held
  chunks.
- A named delta for `x` after held chunks is a new call, because core names a call
  once. It starts a new occurrence (alias rule as before). The held chunks are its
  first args.
- The item for `x` before a name means that the held chunks belong to the current
  occurrence. They go to it in order, before the item. A result for `x` and the run end
  do the same.
- So under `ParallelAgent` with unique ids, a sibling's unfinished call keeps its span
  and its id across a handoff, as on `main`. The parent's dropped call is flushed at
  the run end, as on `main`.
- The hold-back rule is: an unnamed delta that starts an occurrence with an alias (AG-UI
  id differs from the provider id) waits for its name. This covers a reused id and an id
  that an orphan result used.
- A result with no occurrence uses its provider id only when the run has not emitted
  that id. Otherwise it gets a new id by the same `{id}~{n}` rule.

The AG-UI id of a new occurrence is `x` when `x` is not in `used_ids`. Otherwise it is
`x~n` with the smallest `n` ≥ 2 such that `x~n` is not in `used_ids` (so an alias never
equals a provider id that the run already emitted). Every frame for the call (`START`,
`ARGS`, `END`, `RESULT`) uses the AG-UI id.

**Results and approvals.** A `ToolOutputItem` for `x` uses the AG-UI id of the current
occurrence of `x`. A `helikon.approval` CUSTOM frame (`ApprovalRequested`) keeps
`call_id` as the provider id (so it still matches `RunDeferred.calls[].call_id`) and adds
`tool_call_id` with the AG-UI id of the current occurrence, so a client can connect it to
the span. (Changed at the final review on 2026-11-20; the first version overwrote
`call_id`, which broke the correlation with `RunDeferred`.) A
result for an id with no occurrence (only possible under `ParallelAgent` or for a
resume-window result) uses the provider id, as today.

- A result for an occurrence that is still in `buffered_calls` (only possible under
  `ParallelAgent`) flushes that buffered call first, so its span gets its name.
- Sequential flows: a call's result always comes before the next `TurnStarted`
  (`loop_state.rs`), and the transfer result comes after `HandoffItem` and before the
  child's first `TurnStarted`. So the "current occurrence" of a result is its own call.
- `ParallelAgent` with unique provider ids (OpenAI, Anthropic): no rule above fires a
  second time, so the output is exactly as today.
- Limit: `ParallelAgent` branches that both use one provider id can still mix two calls.
  This is the same as today.
- Limit: AG-UI ids are unique **per run**. The mapper is new for each request
  (`agui/sse.rs`), so two runs in one AG-UI thread can both emit `fc_0`.
- The module doc, the field docs and the invariant "one `toolCallId` maps to exactly one
  span" stay. Update the docs for the occurrence rule.
- Core contract wording: provider call ids are unique **at item level** in one stream
  (the accumulator merges deltas by id; `model.rs`).

**Oracle `assert_tool_calls_well_formed`.** Check (3) today keys `received` on the
provider `call_id` and `emitted` on the AG-UI `toolCallId`, so it cannot hold when an id
repeats. Change it as follows, with a rule that does not copy the mapper's logic:

- Split `received` per occurrence by the fixture's own structure: in the sequential
  fixtures, cut at each `TurnStarted` and at each `HandoffItem`. Occurrence `n` of
  provider id `x` maps to `x` (n = 1) or `x~n`.
- Assert that the emitted args of each AG-UI id equal the received args of its
  occurrence.
- Assert that each `TOOL_CALL_RESULT` uses the AG-UI id of the call that its
  `ToolOutputItem` closes.

### Wording updates

Public text that documents the removed deny (must change):

- `docs/book/src/concepts/permissions-guardrails-hooks.md` ("Repeated call ids"
  paragraph): say that the deferral works, and why.
- Rustdoc of `RunContext::with_deferred_tool_calls` (`context.rs`): remove the deny text.

Public text on the handoff event order:

- `docs/book/src/concepts/multi-agent-patterns.md` ("`HandoffItem` followed by
  `AgentUpdated`"): add the transfer results between them.
- `docs/book/src/concepts/agent-loop.md`: check it, and update it if it lists the order.

Internal comments that say the recorder closes transfer calls at the end of the run, or
that the transfer calls get no result while the child runs:

- `agent.rs` (comment at `transfer_call_ids`).
- `resume_exec.rs` (`close_open_transfers` doc).
- `deferral.rs` (`agent_view` doc; the comment in the
  `child_history_drops_the_recorder_result…` test).
- `crates/paigasus-helikon-core/tests/deferred_resume.rs` (the helper docs near
  `text_then_call_b_session`, `handoff_session_with`, and the comment near line 969).

Each rewrite says that the parent emits the result after the handoff, and that a session
from core 0.6.0 can still hold a late result.

### Interaction checks (verified against the code by the spec challenge)

- **Resume.** A resumed run descends the handoff path with no `HandoffItem`
  (`agent.rs`, resume routing), so it emits no second transfer result.
- **`close_open_transfers`.** It finds each result with `paired_result` and moves it to
  directly after its call. With one transfer call this is no change. With two or more
  calls the projection `[TC t, TC u, TR t, TR u]` becomes `[TC t, TR t, TC u, TR u]` in
  the model request. Both forms are valid provider input, and SMA-803 does the same on
  old logs.
- **Child view.** `agent_view` drops a `ToolResult` that closes no open call. The new
  transfer result comes first in the child part of the log and the child view has no
  transfer call, so the view drops it.
- **Open deferrals.** `open_deferral_positions` treats a deferral as closed when a later
  `ToolReturned` has the same call id. In the old layout, the late transfer result came
  after the child's deferral with the same id, so the deferral was not open. This was the
  main reason for the deny. In the new layout the transfer result comes before the
  child's call, so the deferral stays open.
- **Resume validity.** `ResumePlan::from_events` finds the last `ToolCall` with the
  deferred call id and requires no later `ToolResult` with that id. The new layout
  passes this check too.
- **Three levels with one id.** The challenger traced `triage(fc_0) → budgeting(fc_0) →
  savings defers fc_0` through `open_deferral_positions`, `CallNotInHistory`,
  `transfer_calls_before` (`later_calls` 2 and 1), `agent_view`, `drain` and
  `close_open_transfers`. All are correct for the new layout. The safety now rests on a
  positional contract between the emission order in `agent.rs`, the scan in
  `transfer_calls_before`, and the `nth(later_calls)` search in `close_open_transfers`.
  Tests lock it (below).
- **Recorder.** The result arrives after the first `TurnStarted`, so it is outside the
  resume window and goes to the normal log.
- **Swarm.** When `max_handoffs` is exceeded, the swarm drops the child stream at the
  `HandoffItem`, before the new result is polled. The run fails, and the recorder's
  synthetic result is the last event, next to the call.
- **AG-UI.** `ToolOutputItem` maps to `TOOL_CALL_RESULT`. Clients now get a result for
  the transfer call.

## Tests

Core integration (`crates/paigasus-helikon-core/tests/`), each with a session recorder:

- T1. One handoff: the event order is `ToolCallItem(t)`, `HandoffItem`,
  `ToolOutputItem(t, "transferred to budgeting")`, `AgentUpdated`. The drained log has
  `ToolReturned(t)` directly after `HandoffOccurred` and no synthetic "did not complete"
  result. The G3 checks pass on the log and on the projection.
- T2. A response with two transfer calls gets two results, in call order, with the two
  texts above.
- T3. No result for a handoff that did not occur (AG1): an `OnHandoff` deny, the depth
  limit, an unknown target, and a swarm with `max_handoffs(0)`. In each case there is no
  `ToolOutputItem` for the transfer call and no "transferred" text. The swarm log is
  `[user, tool_called(call-1), tool_returned(call-1, CANCELLED)]` with no `handoff`.
- T4. A target whose `run` returns `Err`: the result is `transfer failed: {target} did
  not start`, then `RunFailed`.
- T5. No second result on a resume (AG3): extend
  `handoff_resume_runs_the_call_on_the_child_with_the_child_conversation` with "no
  `ToolOutputItem` for `t1`" and "the session holds exactly one result for `t1`". Also
  cover the resume-route exits (depth limit, target `run` error) on a new-layout log:
  they emit no new `t1` result.
- T6. Three levels with one id (G2): `triage(fc_0) → budgeting(fc_0) → savings` defers
  `fc_0`.
  - (a) The run defers. The labels are `tool_called(fc_0), handoff,
    tool_returned(fc_0, transferred to budgeting), tool_called(fc_0), handoff,
    tool_returned(fc_0, transferred to savings), tool_called(fc_0), tool_deferred(fc_0)`,
    with no cancel result.
  - (b) A resume runs the call, and `assert_tool_calls_paired` holds on every request.
  - (c) A renamed root and a missing middle agent each deny the call. Neither emits a
    transfer `ToolOutputItem`, and the session gets no second transfer result.
- T7. Old layout (AG2): a test helper `with_late_transfer_results(session)` turns a
  new-layout log into the core 0.6.0 layout. It removes each transfer `ToolReturned` after
  `HandoffOccurred` and appends a cancel result (the shared `const` text) at the end of
  the handoff run's events, which is where core 0.6.0 `drain` put it. The four
  "moves the recorded transfer result" tests in `deferred_resume.rs`
  (`renamed_root_on_a_recorded_log_moves_…`, `missing_middle_agent_on_a_recorded_log_moves_…`,
  `missing_target_moves_every_recorded_transfer_result_…`,
  `missing_middle_agent_moves_two_recorded_transfer_results_that_share_one_call_id`) run
  on that helper. Copies run on the new layout.
- T8. `deferral.rs` unit fixture in the new layout (`TC t, TC u, HO, TR t, TR u, …`) for
  `transfer_calls_to_next`.
- T9. The bug that providers reject: a second, normal `TokioRunner` run on the same
  session after a handoff run. The root model request satisfies `assert_tool_calls_paired`
  and has each transfer result directly after its call block.
- T10. `project()` repair (G4), unit tests in `session.rs`:
  - Old layout, one transfer call: the late cancel moves to directly after the transfer
    call, and its text becomes `transferred to {to}`.
  - Old layout, two transfer calls in one response: both results move to directly after
    the two calls, in call order, with the G1 texts.
  - Old layout, child makes two tool-only rounds (challenge Case 1):
    `user, TC t, HO, TC a, TR a, TC b, TR b, AM, TR t` projects to
    `user, TC t, TR t, TC a, TR a, TC b, TR b, AM`.
  - Old layout, `fc_0` at the transfer call and at a child call, with no text between
    (challenge Case 2, the fixture `child_history_pairs_each_result_with_one_open_call`):
    each result pairs with its true call, and the child's own result keeps its content.
    Also with three levels.
  - Unchanged: a log with no handoff; a new-layout handoff log; a `ParallelAgent`
    interleaving with a repeated id (`AM(A), TC fc_0(search), AM(B), TC fc_0(lookup),
    TR(search data), TR(lookup data)`).
  - A late result with a text that is not the cancel text moves but keeps its text.
  - `Compacted`: a compaction that removes the transfer call leaves the result in place;
    the message count never changes.
  - `Compacted` before the handoff (the real-session shape):
    `user, AM, Compacted(2), user, TC t, HO, AM(child), TR t(cancel)` repairs the
    handoff after the summary.
  - AG1 boundary: `user, TC t, HO, TR t(cancel)` (a failed `run()` in the old code)
    projects unchanged, with the cancel text.
  - A running child call keeps its own cancel: `TC fc_0(T), HO, TC fc_0(C),
    cancel, cancel` gives `transferred to …` to T and the cancel text to C.
  - The three-level case asserts `transferred to budgeting` and `transferred to savings`
    at their own positions.
- T11. A second, normal run on an old-layout session (built with the T7 helper): the root
  model request satisfies the round check (below), and the transfer result is directly
  after the transfer call.
- T12. Pairing oracle: **add** the **round rule** to the existing checks of
  `assert_tool_calls_paired` (core `tests/deferred_resume.rs` and runtime-tokio
  `tests/deferred.rs`): a `ToolCall` that comes after a `ToolResult` must find no
  outstanding call. Keep the existing check that rejects a `UserMessage` or
  `AssistantMessage` while calls are outstanding. The G3
  projection check uses the same rule. The old block rule passed both invalid outputs of
  the rejected G4 version.
- T13. `close_open_transfers` on a repaired same-id history with no text: the model
  request pairs each result with its true call. Also one test with a custom `Session`
  whose `snapshot()` does not call `project()`, so the "move" branch of
  `close_open_transfers` keeps test coverage.
- T14. AG-UI (G5), unit tests in `agui/map.rs`:
  - Turn 1 calls tool `a` with id `fc_0` and gets its result; turn 2 calls tool `b` with
    id `fc_0`. Streamed and not streamed: the second call gets AG-UI id `fc_0~2`, name
    `b`, and a complete span; its result uses `fc_0~2`.
  - Handoff: the parent's turn 1 uses `fc_1` for a normal call; the handoff response is
    `[search fc_0, transfer fc_1]` (only the transfer gets a `ToolCallItem`). So the
    transfer call is the second use of `fc_1` (`fc_1~2`), and its result after
    `HandoffItem` uses `fc_1~2`. The child response `[lookup fc_0, pay fc_1]` renders
    each call with its own name and a fresh AG-UI id, and no args merge.
  - The same handoff with the transfer delta streamed first, so `search` is buffered.
  - `ParallelAgent` with unique ids: branch B's `TurnStarted` arrives between branch A's
    deltas and A's `ToolCallItem`. The output is exactly as today (one span, the right
    name, no second span).
  - An alias never equals an emitted provider id: a run that emits `x~2` as a real
    provider id and then reuses `x` gets `x~3`.
  - `ApprovalRequested` for a reused id carries the AG-UI id of the current occurrence.
  - The changed `assert_tool_calls_well_formed` (see G5) holds on every case.

Runtime-tokio (`crates/paigasus-helikon-runtime-tokio/tests/deferred.rs`):

- Change `deferral_with_the_call_id_of_an_open_transfer_call_is_denied` to assert that
  the child's `fc_0` call now defers, and that a resume of that deferral runs the call.
  Keep `assert_tool_calls_paired` on every child request. Rename the test to match.

## Release and docs

- Commit types: `fix(core)` for G1 to G4, `fix(runtime-agentcore)` for G5 (separate
  commits on the branch). The `fix(core)` body names the three behaviour changes: (1) a
  handoff run now emits a `ToolOutputItem` for each transfer call, (2) a deferral with a
  call id that repeats a transfer call id is no longer denied, and (3) `project()` moves
  the late result of a transfer call in a session stored before SMA-810 to directly
  after the call, with the transfer text. release-plz writes the CHANGELOG from
  the commits.
- **Release order (Anthropic).** The Anthropic translator sends `tool_result` before
  `tool_use` for `[User, TC, TR, AM]` until SMA-819 ships. The old layout projects to
  `user, TC t, AM(child), TR t`; the SMA-819 session found that the Anthropic API merges
  adjacent same-role turns, so that old shape is probably accepted. After G1 and G4 the
  projection is `user, TC t, TR t, AM(child)`, which the unfixed translator sends in the
  wrong order. So SMA-810 must not reach crates.io before SMA-819: merge the SMA-810 PR
  only after SMA-819 is merged (both then go out in the same or a later release PR). The
  PR body states this.
- mdBook and rustdoc: see "Wording updates". Also check `docs/book/src/concepts/sessions.md`
  (or the page that describes `project`/snapshots) and add one sentence on the repair.
- Crate READMEs: no public API change, so no README edit.

## Left to the builder

- The exact test file and helper names.
- Whether the result texts come from a small private function in `agent.rs`.

## Follow-up candidates (not in this change)

- The read-time repair (G4) and the AG-UI call state (G5) were folded in at Gate 1.
- **Provider translators.** The second spec challenge found two
  translator bugs that this change does not cause:
  - Anthropic (`providers-anthropic/src/translate/request.rs`): for the items
    `[User, ToolCall, ToolResult, AssistantMessage]` the `AssistantMessage` arm flushes the
    pending `tool_result` before the pending `tool_use`. The wire becomes `user,
    user[tool_result], assistant[tool_use], assistant[text]`. Confirmed by reading the
    code. This shape occurs on every later run of a session after a tool-only response.
    **Decision (Sven, 2026-11-20):** this goes into SMA-819 (Anthropic translator, in
    progress). The SMA-819 session confirmed that SMA-819 already covers all three shapes,
    with unit tests and a live check against the Anthropic API. SMA-810 needs no change.
  - Gemini (`providers-gemini/src/translate/request.rs`): the `functionResponse` name is
    found by call id, and the last call with that id wins. With a repeated `fc_0`, an
    earlier result gets a later call's tool name.
    **Decision (Sven, 2026-11-20):** added to SMA-820 as part 2 (same Gemini file).
    SMA-810 needs no change for this. Release note: until SMA-820 part 2 ships, a Gemini
    request whose history holds two calls with one id (for example a transfer call and a
    deferred child call, both `fc_0`) still sends the earlier result with the later
    call's tool name. SMA-810 does not make this worse: before SMA-810 the same two calls
    were in the history (the child call was denied, not deferred, but it was still
    recorded). So SMA-810 does not wait for SMA-820.
