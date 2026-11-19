# SMA-803: Deferred tool calls — design

- **Issue:** [SMA-803](https://linear.app/smaschek/issue/SMA-803)
- **Design sources:** Helikon ADR-15 ("Deferred tool calls and a turn-boundary
  checkpoint") and paigasus-core ADR-0028, decision (g).
- **Related work:** SMA-804 (turn-boundary checkpoint, shares `StopReason`),
  SMA-805 (publishes `sessions-testkit`, adds a `ToolDeferred` conformance test),
  SMA-810 (the handoff transfer call has no adjacent result, filed by the SMA-804
  session), SMA-795 and SMA-793 (consumers in paigasus-core).
- **Review history:** one builder check ("nothing important left") and one
  adversarial challenge (verdict "needs rework"). §8 records how this revision
  handles each finding. Gate 1 approved by Sven on 2026-11-19.

## 1. Intent

A permission check can defer a tool call. The run then ends cleanly at the turn
boundary. A later run resumes the call with the approver's answer.

Today an ask blocks the loop on an `ApprovalHandler` future, and
`LoopState::NeedsApproval` returns `NotImplemented`. The always-on agent host in
paigasus-core needs deferral. An approval can take hours. The host must not hold
a lease and a worker slot for that time.

## 2. Goals, non-goals, anti-goals

**Goals**

- A policy, a guard `Ask` and an `AskUser` can all defer one call.
- A run with a deferred call ends with `StopReason::Deferred { calls }`. Its
  session holds one `SessionEvent::ToolDeferred` per deferred call, with the
  effective args.
- A resume with `Approved` runs the call once, with the recorded args, after a
  re-check. A resume with `Denied` records the denial result.
- Every provider request that the SDK makes after a deferral or a resume is
  valid: each `ToolCall` has exactly one `ToolResult`, and the results come
  before any new user message.
- Every change is compile-compatible for callers (new variants and fields go
  only on `#[non_exhaustive]` types, and no signature changes).

**Non-goals**

- Host-side storage, approval tokens, expiry, "superseded" rules and the args
  hash. These are SMA-795.
- Deferral inside `AgentAsTool` sub-runs and inside workflow-agent children. In
  v1 a `Defer` there is a `Deny`.
- Deferral in `TemporalRunner`, MCP and the HTTP runtimes (axum, actix,
  agentcore). In v1 a `Defer` there is a `Deny`.
- A fix for SMA-810 (the handoff transfer call in normal later runs).
- The turn-boundary checkpoint and `StopReason::Stopped`. These are SMA-804.

**Anti-goals** (the design fails if one of these occurs, even when it works)

- An approved call runs with args that are different from the args that were
  recorded for the approver.
- A resolution applies to a different call than the one it was made for.
- A `Deny` from any stage is overridden by an approval.
- A provider request has a `ToolCall` without a `ToolResult`, or a `ToolResult`
  after a later user message.
- A run that did not opt in to deferral ends with `Deferred`.

Note: while a deferral is open, the stored log holds a `ToolCall` without a
`ToolResult`. This is by design. `snapshot()` of such a session is not valid
provider input (for example for a host-side summarizer). Document this.

## 3. Approaches

The open choice was how a caller resumes a deferred run.

- **A. Resolutions on `AgentInput` (chosen by Sven).** The caller calls the
  usual `Runner::run` or `run_streamed` with
  `AgentInput::new().with_resolution(deferral_id, ToolResolution::Approved)`.
  The `Runner` trait gets no new method, and streaming works without extra code.
  Risk: a runner that does not know about deferrals damages the session.
  Mitigation: the detection and validation logic is in core (§4.6), and the
  `Runner` docs require every runner to reject a run on a session with open
  deferrals unless it supports resume.
- **B. New `Runner::resume_deferred` and `resume_deferred_streamed` methods**,
  with defaults that return "unsupported". The API is more explicit. But it adds
  two trait methods and a second path that must stay in line with `run`.

## 4. Design

### 4.1 Public types (all in `paigasus-helikon-core`)

The exact names are the builder's call (ADR-0028 §8). The names below are the
working names that SMA-804 and SMA-805 use, so change them only for a strong
reason, and tell those sessions.

- `PermissionDecision::Defer { prompt: String }`.
- `ApprovalOutcome::Defer`. An `ApprovalHandler` returns it to defer a guard
  `Ask` or an `AskUser`.
- `PendingToolCall` (`#[non_exhaustive]` struct; `Clone`, `Debug`, `PartialEq`,
  serde):
  - `deferral_id: String`: a UUID v4 that core makes at defer time (workspace
    `uuid` pin, feature `v4`). Every resolution uses this id.
  - `call_id: String`: the provider's call id, used only to pair the result
    with the `ToolCall` item. It can repeat across turns (Gemini makes `fc_0`,
    `fc_1`, … again on each stream), so it never identifies a deferral.
  - `tool: String`.
  - `args: serde_json::Value`: the effective args (after `PreToolUse` and any
    `Replace`).
  - `prompt: String`: every distinct prompt of the call's ask points, in
    pipeline order, joined with a newline.
  - `handoff_path: Vec<String>`: the agent names from the root agent to the
    agent that made the call, both included.
  - Accessor `agent()` returns the last name of the path. Give the struct a
    constructor and accessors, because callers outside core cannot use a struct
    literal.
- `ToolResolution` (`#[non_exhaustive]` enum; `Clone`, `Debug`, `PartialEq`,
  serde): `Approved` and `Denied { reason: String }`.
- `StopReason` (`#[non_exhaustive]` enum; `Clone`, `Debug`, `PartialEq`, serde,
  `Default = Completed`): `Completed` and
  `Deferred { calls: Vec<PendingToolCall> }`. SMA-804 adds
  `Stopped { reason: String }`.
- `RunResult` gets `pub stop_reason: StopReason`. `RunResult` is
  `#[non_exhaustive]` and derives `Default`, so this is compatible.
- `AgentEvent::RunDeferred { calls: Vec<PendingToolCall>, usage: TokenUsage }`.
  It is a terminal event. Do not add a field to `RunCompleted`, because variant
  fields are not `#[non_exhaustive]`. SMA-804 adds `RunStopped { reason, usage }`
  with the same pattern.
- `SessionEvent::ToolDeferred { deferral_id, call_id, name, args, prompt,
  handoff_path, ts }`, serde tag `"tool_deferred"`. `kind()`, `ts()` and
  `project()` get the new arm. `project()` adds no `Item` for it (contribution
  0, as for `HandoffOccurred`).
- `AgentError::Incomplete { stop_reason: StopReason }`, for `collect_typed`
  (§4.5). `AgentError` is `#[non_exhaustive]`.
- `RunError::InvalidResume(InvalidResume)`, where `InvalidResume` is a
  `#[non_exhaustive]` enum of reasons (§4.6). A host can then tell "no open
  deferrals" (for example on a retry) from a real error.
- `RunError::SessionWrite(...)` for a failed append on a deferring or resuming
  run (§4.5, §4.6). The builder chooses the payload.
- `RunContext::with_deferred_tool_calls(bool)` and
  `deferred_tool_calls_enabled() -> bool` (§4.2).
- `AgentInput::with_resolution(deferral_id, ToolResolution)`, and an
  `Option<ResumePlan>` that only a runner sets (§4.6). `AgentInput` is
  `#[non_exhaustive]`.
- `pub fn open_deferrals(events: &[SessionEvent]) -> Vec<PendingToolCall>` and
  `ResumePlan::from_events(...)` (§4.6).

### 4.2 Who may defer

A `RunContext` has a flag "deferred tool calls enabled". The default is `false`.

- The caller turns it on with `with_deferred_tool_calls(true)`. The host does
  this.
- `handoff_child()` keeps the flag. A handoff child is part of the top-level
  loop, so it can defer.
- `subagent_child()` (workflow agents) and the `AgentAsTool` sub-context set the
  flag to `false`. This replaces the ADR's "depth > 0" rule. Reason:
  `handoff_child()` also adds 1 to `agent_depth`, so a depth rule would deny
  every deferral after a handoff.
- `to_tool_context()` does not project the flag. `AgentAsTool` builds its
  sub-context with `RunContext::new`, so the flag is `false` there. Write a
  regression test in `core/tests/subagent_propagation.rs`. These are the four
  `RunContext` copy sites that every new field must pass through.
- Runners and runtimes that do not support deferral set the flag to `false` on
  the context that they receive, and log one warning when it was `true`:
  `TemporalRunner`, and the axum, actix and agentcore handlers (on the context
  that `ContextProvider::build` returns). MCP and evals build their contexts
  with `RunContext::new`, so the flag is `false` there. One test per runtime.
  The seven build sites (from the SMA-804 session; verify them):
  axum `handlers/runs.rs:220`, actix `handlers/runs.rs:257`, and agentcore
  `invoke.rs:165`, `ws.rs:203`, `agui/sse.rs:147`, `agui/ws.rs:267` and
  `a2a/rpc.rs:519`. Each crate gets one private helper
  `restrict_unsupported_run_features(ctx)`, called directly after each
  `build(..)`. SMA-804 adds its checkpoint clear inside the same helper.

When the flag is `false`, every point that would produce `Defer` produces
`Deny { reason: "approval deferred, but this run cannot defer: {prompt}" }`
instead. The exact text is the builder's call. The `ApprovalHandler` cannot
see the flag (`decide` has no context). Document that a handler that returns
`Defer` gives `Deny` in every context where the flag is off.

A direct `agent.run(ctx, input)` call with the flag on can end with
`RunDeferred`. Only a deferral-aware runner can resume it. Document this.

### 4.3 Authorization with `Defer`

`RunContext::authorize_tool` keeps its stage order: deny rules › guard rules ›
allow rules › mode › policy › approval handler. The loop uses a crate-private
function with a richer result (the decision, the mark, all prompts and the final
args). `authorize_tool` stays the public wrapper and returns
`PermissionDecision`. The rules:

1. **Every ask point asks.** Each guard `Ask` and a policy `AskUser` call the
   handler, as today, also after an earlier ask point of the same call deferred.
   A handler can answer differently per prompt, so a skipped ask point could
   lose a `Deny`.
2. `Allow` from the handler clears that ask point (as today). `Deny` returns
   `Deny`. `Defer` sets the "deferred" mark and records the prompt. The pipeline
   continues.
3. A policy `Defer { prompt }` sets the mark and records the prompt. It needs no
   handler. `AskUser` without a handler is still `Deny`.
4. **Any `Deny` wins**: a later guard `Deny`, mode `Plan` on a non-read-only
   tool, mode `DontAsk`, a policy `Deny`, or a handler `Deny`.
5. **With the mark set, every exit that would return `Allow` or `Replace`
   returns `Defer` instead.** This includes the allow-rule short-circuit, mode
   `Bypass`, mode `AcceptEdits` on a `Write` tool, "no policy installed", and a
   policy `Allow`. Guards exist to beat `Bypass`, so a deferred guard must not
   become an `Allow`.
6. A policy `Replace { args }` with the mark set gives `Defer`. The pending call
   records the replaced args as its effective args, so the approver sees them.
7. The prompt of the result is every distinct recorded prompt, in pipeline
   order, joined with a newline.
8. A call to a tool that the agent does not have is not deferred. It fails as
   today with "unknown tool: {name}". An approval for it would be wasted.

`authorize_tool` returns `Defer` only when the flag (§4.2) is on. Otherwise it
maps the deferral to `Deny`. `execute_tool_call` (`tool_exec.rs`, the durable
path) always maps `Defer` to `Deny`.

The matches without a wildcard in core (`agent.rs` `run_tools_concurrent`,
`tool_exec.rs`, both `ApprovalOutcome` matches in `context.rs`) change with the
new variants. Document in the `ApprovalHandler` docs that one tool call can
produce more than one `decide` call.

### 4.4 The loop: a turn with deferred calls

In `run_tools_concurrent`, a call whose decision is `Defer` is not invoked. It
returns a "deferred" outcome that holds a `PendingToolCall`. The other calls of
the turn run as today, and their results go into the conversation as today.

When a turn has one or more deferred calls:

- The loop does not call the model again.
- The transition moves to `LoopState::NeedsApproval { pending }` and then to
  `NextAction::Terminate`. `pending` keeps its current type
  (`Vec<ToolCallRequest>`). The full data travels in the events.
- On the last allowed turn, the deferral wins over `MaxTurnsExceeded`. The run
  does not call the model again, and a resume gets a new turn budget.
- The events, in this order: the `PermissionDenied` events (the driver yields
  them first, as today), the `ToolOutputItem` of each call that ran, one
  `ApprovalRequested { call_id, tool, args }` per deferred call, then
  `RunDeferred { calls, usage }`. `ApprovalRequested` exists and is never
  emitted today. Change its `args` doc from "arguments the model proposed" to
  "effective arguments".
- The output guardrails do not run. `OnRunComplete` fires once, as on every
  `Terminate`.
- The run span records usage for `RunDeferred`, as for `RunCompleted`, and
  gets an attribute with the number of deferred calls.
- A handoff in the same model response takes precedence over regular calls, as
  today. So a turn never has both a handoff and a deferral.

`TransitionInput::ToolResults { outcomes }` must stay compile-compatible, because
`runtime-temporal`'s driver constructs it. Add a new `TransitionInput` variant
for "results plus deferred calls", or an equivalent. This is the builder's call.
`transition()`'s arm `(NeedsApproval, _) => not_implemented("approval")` goes
away. `NeedsApproval` is now a terminal state.

**Secrets in effective args.** A `PreToolUse` `ReplaceInput` can add values to
the args. Today the effective args are never stored or emitted. Decision (my
call):

- `ToolDeferred` in the session stores the raw effective args, because the
  resume must run with exactly these args.
- `RunDeferred` and `ApprovalRequested` carry the args after redaction with the
  run's `SecretSet`, when output redaction is on. Events reach SSE and AG-UI
  clients, so they must not carry a secret that the run knows.
- `RunResult.stop_reason` comes from the event, so it also has the redacted
  copy. The source of truth for the host is the stored `ToolDeferred`.
- Document that a hook that adds a secret to the args of a call that can defer
  makes the secret persist in the session store.

### 4.5 Recording a deferral

- `SessionRecorder::observe` maps `RunDeferred` to one
  `SessionEvent::ToolDeferred` per call. But the event has the redacted args, so
  the loop gives the recorder the raw effective args by a separate channel (for
  example a shared buffer that the runner passes to the recorder). The builder
  chooses the mechanism. A test must prove that the stored args are the raw
  args, and that the event args are redacted.
- `SessionRecorder::observe` does not record `ApprovalRequested`.
- `SessionRecorder::drain` does not write a synthetic result for a call that has
  a `ToolDeferred` in this run.
- `TokioRunner::run_streamed` finalizes before it yields `RunDeferred`, because
  `is_terminal()` includes it.
- **A deferring run must not lose its deferral.** If the append fails for a run
  that ends with `RunDeferred`, `TokioRunner::run` returns
  `Err(RunError::SessionWrite(..))`, and `run_streamed` yields `RunFailed`
  instead of `RunDeferred`. Other runs keep the best-effort rule.
- `RunResultStreaming::collect()` returns `Ok`. `final_output` is the text of
  the last `MessageOutput` (as today, empty if there is none). `usage` comes from
  `RunDeferred`. `stop_reason` is `Deferred { calls }`. A stream that ends with
  no terminal event keeps today's `Ok` and gets `stop_reason = Completed`.
- `collect_typed()` returns `Err(AgentError::Incomplete { stop_reason })` when
  the stop reason is not `Completed`. Its return type stays
  `Result<RunResult<T>, AgentError>`.
- `parse_final()` keeps its return type `Result<RunResult<T>, serde_json::Error>`.
  A changed return type breaks callers, and cargo-semver-checks would force a
  minor bump. It copies `stop_reason` into the typed result. Its docs tell the
  caller to check `stop_reason` first.
- `CompactingSession` does not compact while `open_deferrals(events)` is not
  empty. Otherwise the summary can remove the deferred `ToolCall` item, and a
  resume cannot pair the result.

The session log after a deferred turn with calls `a` (ran), `b` (deferred) and
`c` (ran):

```text
assistant_message?  tool_called a  tool_called b  tool_called c
tool_returned a  tool_returned c  tool_deferred b
```

### 4.6 Resume

The caller sends the resolutions with the normal run call. The key is the
`deferral_id` from `PendingToolCall`:

```rust
let input = AgentInput::new()
    .with_resolution(pending.deferral_id(), ToolResolution::Approved);
let result = TokioRunner.run(&agent, ctx, input, config).await?;
```

New user messages in the same input are allowed. They go after the results.

**Precondition.** One writer per session at a time. The host's lease gives this.
Two resumes of one session at the same time can both run an approved tool.
Document this on the `Runner` trait and on `with_resolution`.

**Core logic** (so that every runner, `CompactingSession` and the host use one
definition):

- `open_deferrals(events)`: every `ToolDeferred` event that has no
  `ToolReturned` for the same `call_id` **after it** in the log. The position
  rule handles a `call_id` that repeats.
- `ResumePlan::from_events(events, snapshot, root_agent_name, input,
  max_agent_depth) -> Result<ResumePlan, InvalidResume>`. `ResumePlan` has
  private fields, so only this function can make one. It rejects:
  - `NoOpenDeferrals`: the input has resolutions, but no deferral is open.
  - `UnresolvedDeferrals`: deferrals are open, but the input has no resolutions,
    or at least one open deferral has no resolution. All open calls of the
    deferred turn must be resolved together.
  - `UnknownDeferral`: a resolution names a `deferral_id` that is not open.
  - `DuplicateResolution`: two resolutions name the same `deferral_id`.
  - `SessionModified`: a `UserMessage`, `AssistantMessage` or `ToolCalled` event
    comes after the first open `ToolDeferred`.
  - `CallNotInHistory`: the snapshot has no `ToolCall` item for an open call,
    with the same name, before the deferral. A third-party compactor can cause
    this. No resume can fix such a session. Document it.
  - `ToolItemsInInput`: the new input contains `ToolCall` or `ToolResult` items.
  - `PathTooDeep`: the handoff path is longer than `max_agent_depth + 1`.
- A root agent name that is not the first name of the path is not an error.
  The root then resolves every pending call as
  `Denied { reason: "agent no longer available: {name}" }` (as in §4.7). So a
  renamed agent does not lock the session.

**Runner steps (`TokioRunner`):**

1. Read `session.snapshot()` and `session.events(None)`. (Two reads. A local
   `project()` is not possible, because a custom `Session` can override
   `snapshot()`.)
2. If the input has resolutions or the log has open deferrals, make the
   `ResumePlan`. A failure returns `Err(RunError::InvalidResume(..))`, before
   the agent starts, and nothing is recorded.
3. Give the agent the snapshot as history, the plan, and the new input messages
   as a separate part. The runner records the new input as today, and gives
   the recorder the plan (see "Session order").
4. Run the agent. Finalize. If the append fails on a resuming run, return
   `Err(RunError::SessionWrite(..))` (`run_streamed`: yield `RunFailed`). The
   approved tool may have run. The error tells the host that the session did not
   store the result.

**Agent steps (`LlmAgent`):**

1. `RunStarted`, the `OnRunStart` hook and the input guardrails run first, as
   today. The input guardrails check the history plus the new input. A deny or a
   tripwire ends the run. Each resolved call then gets a synthetic result
   "tool call did not run: the run ended before the resume completed", placed
   before the new input.
2. Route to the recorded agent (§4.7). The rest runs on that agent.
3. Execute the pairs through the normal tool-execution path, without
   `PreToolUse`: concurrent, with `parallel_tool_call_limit`, and with a
   `tool.execute` span per call (with a "resumed" attribute):
   - `Denied { reason }`: the result is `Err("permission denied: {reason}")`.
     Emit `PermissionDenied`.
   - `Approved`: run the re-check (§4.8). If it passes, invoke the tool with the
     recorded args. Then fire `PostToolUse` and apply redaction, as for a normal
     call. If the re-check fails, the result is the denial, and the loop emits
     `PermissionDenied`.
4. Emit one `ToolOutputItem` per result, in the order of the original calls.
   Push each result into the conversation in that order.
5. Append the new input messages to the conversation, and continue the loop
   with `CallingModel`. The turn count starts at 0, as for every run.

If `LlmAgent` gets resolutions without a `ResumePlan` (a direct `agent.run`
call), it yields `RunFailed` with "resolutions require a deferral-aware
runner".

**Session order.** The runner records the new input before the stream starts.
So `SessionRecorder::drain` puts the `ToolReturned` events of the plan's calls
before the recorded input events. The recorder takes them by position: the
`ToolOutputItem`s that the agent emits before its first `TurnStarted`. If a
planned call has no result when the run ends (cancel or timeout), `drain` writes
the existing synthetic result ("tool call did not complete (run cancelled/timed
out)") for it, in the same place. This closes the deferral, and the log stays
valid.

**Other runners.**

- `TemporalRunner` returns `Err(RunError::InvalidResume(..))` when the input has
  resolutions or the session has open deferrals. It never records anything in
  these cases. (The reason variant for "this runner cannot resume" is the
  builder's call.)
- The `Runner` trait docs state: a runner that does not support resume must
  reject a run on a session with open deferrals, with `open_deferrals()`.
  Otherwise its run puts a user message after an unanswered `ToolCall`.
- `Runner::resume` and `resume_streamed` send an empty input. On a session with
  open deferrals they return `InvalidResume`. Document this next to them.

**Workflow agents.** `SequentialAgent`, `ParallelAgent`, `LoopAgent`,
`GraphAgent` and `SwarmAgent` yield `RunFailed` ("this agent cannot resume
deferred calls") when the input has resolutions or a plan. Otherwise a
`ParallelAgent` could run one approved call once per branch. Their children
cannot defer (§4.2), so a valid session never needs this path.

### 4.7 Handoffs

- `RunContext` gets a handoff path. `LlmAgent::run` adds its own name to it.
  Copy sites: `handoff_child()` copies it, `subagent_child()` resets it,
  `AgentAsTool` starts it empty (`RunContext::new`), and `to_tool_context()`
  does not project it.
- On resume, the root agent compares the recorded path with its own name. If
  the path continues, it sends the run to the handoff target with the next name.
  It does not fire `OnHandoff` and does not emit `HandoffItem`, because the
  session already holds that handoff. It emits `AgentUpdated`, so that the
  recorder knows the current agent. `OnSubagentStop` fires when the child run
  ends, as for a normal handoff.
- **The child gets the conversation that it saw when it made the call:**
  `thread_handoff_transcript(...)` of the history before the last
  `HandoffOccurred` to this child, then the history after it. The builder
  derives the split from the events. This history has no transfer call and no
  root tool calls, so it does not depend on SMA-810.
- If an agent in the path is not a handoff target any more, the deepest agent
  that the path reaches resolves every pending call as
  `Denied { reason: "agent no longer available: {name}" }` and continues its
  own loop.
- The handoff re-yield adds the child usage to `parent_usage` for
  `RunDeferred`, as it does for `RunCompleted`. This needs an explicit arm,
  because other events pass through `other => yield other`.
- Test: a structural check that every provider request in a handoff resume has
  each `ToolCall` paired before the next message. A test that sends the history
  through the OpenAI Chat and Anthropic request translators is welcome, if the
  dev-dependency graph allows it.

### 4.8 Re-check on `Approved`

The re-check is the same pipeline function as §4.3, with one parameter that
marks the ask points as answered. So the two paths cannot drift. The rules
(ADR-0028 (g)):

- Do not fire `PreToolUse` again.
- Run the deny rules, the guard rules, the allow rules, the mode and the policy
  again.
- `Defer`, `AskUser` and guard `Ask` count as answered by this approval. The
  approval handler is not called. This also applies to an ask point that did
  not fire at defer time (see §7, question 1).
- Any `Deny` wins.
- A `Replace` with args that are different from the recorded args denies the
  call ("arguments changed on re-check"). A `Replace` with equal args passes.
  Document that a policy `Replace` must be idempotent.
- A tool that the agent does not have any more is denied with
  "tool no longer available". The tool lookup gives the `ToolEffect` for the
  mode check.
- The call runs on the recorded agent (§4.7).

The deferral flag (§4.2) has no effect on the re-check, because the re-check
cannot defer. A resume works with the flag off. A new deferral in the resumed
run follows the flag.

### 4.9 Temporal

`TemporalRunner` v0 does not support `Defer`:

- It sets the deferral flag to `false` (§4.2). The worker-built context has
  `false` by default, and `execute_tool_call` maps `Defer` to `Deny`.
- It rejects resolutions and sessions with open deferrals (§4.6).
- Its driver keeps its wildcard arms. It never sees the new transition input.

### 4.10 Compatibility, docs and release

- All new variants and fields are on `#[non_exhaustive]` types. No public
  signature changes. In-crate struct literals of `AgentInput` and `RunResult`
  (for example the handoff in `agent.rs` and `parse_final`) change with the new
  fields.
- Tests and docs that hard-code the event set change:
  `every_variant_covers_the_whole_enum` (17 → 18),
  `exactly_two_variants_are_terminal` (2 → 3), the test function `classify`,
  the `AgentEvent` doc ("Seventeen variants"), the `is_terminal` doc ("the two
  events"), and the `Runner::run` doc ("terminal (`RunCompleted`/`RunFailed`)").
- **Rollback note.** "Once a host stores a `tool_deferred` event, it cannot
  roll back below this version, because an older reader cannot parse the row.
  Enable deferral only after every reader of the session store runs this
  version." It goes into the core crate docs, the mdBook page and the PR body.
  release-plz makes the changelog from commits. The plan checks how release-plz
  renders a commit body. If it does not show the note, edit the release PR's
  `CHANGELOG.md` before merge.
- Release: pure-auto release-plz, with no version bump by hand.
  `runtime-tokio`, `runtime-temporal` and the HTTP runtimes are already
  released, so release-plz publishes core first in one release PR (proven in
  SMA-418). The commit type is `feat(core)`. The PR title scope must be on
  `main`'s allowlist.
- Docs: update the mdBook page that describes permissions and approvals, and the
  core README if it shows the approval flow. The builder finds the pages.

## 5. Tests

- **Authorization unit tests** (`core/src/control.rs` `authorize_tests`):
  - a policy `Defer`;
  - a handler `Defer` on a guard `Ask` and on an `AskUser`;
  - two guard `Ask`s: the handler is called twice; `Defer` then `Deny` gives
    `Deny`;
  - `Defer` then a guard `Deny`, then `Plan` on a write tool, then `DontAsk`,
    each gives `Deny`;
  - a guard `Defer` combined with an allow rule, `Bypass`, `AcceptEdits` on a
    `Write` tool, no policy, and a policy `Allow`: each gives `Defer`;
  - a guard `Defer` and a policy `Replace`: `Defer` with the replaced args;
  - an unknown tool does not defer;
  - the flag off gives `Deny`.
- **Re-check unit tests:** each rule in §4.8, including an allow rule, `Replace`
  with equal and with different args, and a missing tool.
- **Loop tests** (`core/tests/`): a turn with an allowed call and a deferred
  call. Assert the event order of §4.4, `RunDeferred`, and that the model is
  called only once. A deferral on the last allowed turn ends with `RunDeferred`.
- **Runner tests** (`runtime-tokio/tests/`):
  - the deferral round trip: the session holds `ToolDeferred` with the raw
    effective args after a `PreToolUse` replace, and the event has the redacted
    args;
  - resume with `Approved`: the tool runs once, with the recorded args, and the
    next model request has the result before the new user message;
  - resume with `Denied`: the denial result is recorded, and the tool does not
    run;
  - a repeated `call_id` in a later turn does not match an old deferral;
  - each `InvalidResume` reason of §4.6;
  - a renamed root agent denies the pending calls and continues;
  - an input-guardrail tripwire on resume closes the deferral before the input;
  - a cancelled resume closes the deferral with the synthetic result;
  - a failed append on a deferring run returns `SessionWrite`;
  - `run_streamed` ends with `RunDeferred`, and the session was finalized.
- **Handoff tests:** a deferral in a handoff child, and a resume that runs the
  call on the child with the conversation of §4.7. A missing handoff target
  denies the calls.
- **Propagation tests:** the flag is off in an `AgentAsTool` sub-run and in a
  workflow child, so a `Defer` there is a `Deny`. The HTTP runtimes clear the
  flag.
- **Workflow agents:** each of the five returns `RunFailed` with resolutions.
- **Temporal:** resolutions and an open deferral give `InvalidResume`.
- **Serde snapshots:** `SessionEvent::ToolDeferred`, `AgentEvent::RunDeferred`,
  `StopReason`, `ToolResolution`.
- **Projection:** `project()` ignores `ToolDeferred`. `open_deferrals()` uses
  the position rule.
- **CompactingSession:** no compaction while a deferral is open.

## 6. Left to the builder

- Exact names, within §4.1 (tell SMA-804 and SMA-805 about any change).
- The internal shape of `ResumePlan` and of the crate-private authorization
  result.
- The channel that gives the recorder the raw effective args (§4.5).
- The new `TransitionInput` variant, or an equivalent.
- The payload of `RunError::SessionWrite`.
- Exact error, denial and synthetic-result texts.
- Which book pages and README files change.

## 7. Decisions on the open questions

Sven approved the spec on 2026-11-19 with the picks below.

1. On re-check, an ask point that did not fire at defer time (because the
   configuration changed) counts as answered. This follows the ADR text
   literally.
2. A `PreToolUse` deny that would apply at resume time is never evaluated,
   because the ADR says "do not fire `PreToolUse` again". The deny rules,
   guards, mode and policy still run.
3. With the flag off, a policy `Defer` is a `Deny`, as the ADR says for runners
   without support.

## 8. Calls I made (for review)

1. The deferral flag is an opt-in on `RunContext`, not a property of the runner
   or of the depth. Runtimes without support clear it and log a warning (§4.2).
2. A core-made `deferral_id` identifies each deferral. The provider `call_id`
   only pairs the result (§4.1).
3. `RunDeferred` is a new terminal event, and `ApprovalRequested` is emitted per
   deferred call (§4.4).
4. Every ask point calls the handler. With the mark set, every `Allow` exit
   becomes `Defer` (§4.3).
5. Events carry redacted args. The session stores the raw effective args (§4.4).
6. `collect_typed` returns `AgentError::Incomplete`. `parse_final` keeps its
   signature (§4.5).
7. A failed append on a deferring or resuming run is an error, not a log line
   (§4.5, §4.6).
8. All open calls must be resolved together, and the validation lives in core
   as `ResumePlan` (§4.6).
9. A renamed root or a missing handoff target denies the pending calls, so the
   session does not lock (§4.6, §4.7).
10. A resume in a handoff child gets the conversation that the child saw, and
    fires no `OnHandoff` (§4.7).
11. `PostToolUse` and redaction apply to an approved call (§4.6).
12. `CompactingSession` does not compact during an open deferral (§4.5).
13. Pure-auto release, no version bump by hand (§4.10).

## 9. Challenge findings not taken

- **"Read the session log once and project it locally."** Not taken. A custom
  `Session` (for example the host's `PgRunSession`) can override `snapshot()`.
  A local `project()` would skip its projection. The cost is one more read per
  run.
- **"Bind `Approved` to a digest of the args."** Not taken in core. The
  `deferral_id` binds the resolution to one recorded call, and the call runs
  with the recorded args. The args hash in the approval token is SMA-795 (host
  side, ADR-0028 (g)).
- **"Let all-`Denied` resolutions close a deferral whose `ToolCall` is gone."**
  Not taken. The result would have no `ToolCall` to pair with, and providers
  reject an orphan result too. The case returns `CallNotInHistory`, and the
  docs say why.
- **"A core `DeferringApprovalHandler`."** Not taken. The host writes its own
  handler in a few lines. The docs explain the flag.
