# SMA-804: Turn-boundary checkpoint on `RunContext`

- Linear: [SMA-804](https://linear.app/smaschek/issue/SMA-804)
- ADR: Helikon ADR-15 "Deferred tool calls and a turn-boundary checkpoint", which implements ADR-0028 decision (h) in `paigasus-core`.
- Sibling: SMA-803 (deferred tool calls), spec `docs/superpowers/specs/2026-11-18-sma-803-deferred-tool-calls-design.md` on its own branch. SMA-803 owns `StopReason`, `RunDeferred`, `AgentError::Incomplete` and the `collect()` change.
- Status: approved by Sven at GATE 1 on 2026-11-18, with Q1 decided as proposed (N6).

## 1. Intent

The always-on agent host runs Helikon's `TokioRunner`. Events can arrive while a run is in progress: a new inbox message, or an operator cancel from the console. Today the host has only two choices. It can wait for the run to end, or it can cancel the `CancellationToken`. A cancel can stop a tool call in the middle of its work. Nothing can add a user-role message to a run that is in progress.

The host needs a safe point between turns. At that point it can do one of three things:

- let the run continue,
- add user content (the `fold` and `interrupt` policies), or
- stop the run cleanly (the operator cancel).

"Cleanly" means that no tool call is half done, and that the session history stays valid for replay.

## 2. Goals, non-goals, anti-goals

**Goals**

- G1. An optional callback on `RunContext`. The loop calls it at every turn boundary (as defined in 5.2) of the top-level loop and of handoff children.
- G2. The callback returns `Continue`, `Inject(Vec<ContentPart>)` or `Stop { reason }`.
- G3. Injected content is user content. The loop emits a new `AgentEvent` for it, and `SessionRecorder` stores it as a `SessionEvent::UserMessage`. The next model call and the stored history both contain it.
- G4. The input guardrails check injected content. A tripwire stops the run with `StopReason::Stopped`.
- G5. `Stop` ends the run at the boundary with `StopReason::Stopped { reason }`. No tool call is half done.
- G6. The change is compile-compatible for callers. The field is optional, all new types are `#[non_exhaustive]`, and no existing variant gets a new field.

**Non-goals**

- N1. `TemporalRunner`. Its `DurableDriver` does not use the caller's `RunContext`, so the callback never fires there. `TemporalRunner` logs a `tracing::warn!` when the context it gets has a checkpoint. A workflow-signal form is a later issue (ADR-15, Consequences).
- N2. Workflow, swarm and graph sub-agents (`subagent_child`). The callback does not fire there (section 4, D2).
- N3. `AgentAsTool` sub-runs. The callback never fires there.
- N4. A checkpoint inside a tool call, or before the first model call of a run.
- N5. A host-side `fold` or `interrupt` policy. The host builds these on top of this callback.
- N6. The HTTP runtimes (axum, actix, agentcore). Their run status knows only `RunCompleted` and `RunFailed`. They clear the checkpoint on the context that they get from `ContextProvider`, and they log a `tracing::warn!` if one was set (see 6.2; decided at GATE 1, Q1 in section 10). This follows SMA-803 §4.2, where the same runtimes never opt in to deferral.
- N7. An audit marker that shows that a stored user message came from the checkpoint. The message is a plain `SessionEvent::UserMessage`. A marker would need a new `SessionEvent` field or variant, and it can be a later issue.

**Anti-goals** (the result is a failure even if it compiles)

- A1. An injected user message sits between a tool call and its tool result in the event stream or in the stored session. Replay then fails at the provider.
- A2. A `Stop` leaves a tool call without a result, or it shows as a run failure (`RunFailed`), or it shows as `Completed`.
- A3. The host cannot tell whether the run took ownership of the injected content. The host then loses an inbox item, or it sends the item two times. The commit rule in 5.3 prevents this.
- A4. One `Inject` reaches more than one concurrent loop.
- A5. Injected content makes the stored session unusable for later runs.

## 3. Constraints

- C1. **SMA-803 lands first.** SMA-803 adds:
  - `#[non_exhaustive] pub enum StopReason { Completed, Deferred { calls: Vec<PendingToolCall> }, Stopped { reason: String } }`,
  - the field `RunResult::stop_reason`,
  - one new terminal variant per stop reason (`RunDeferred { calls, usage }`), with no new field on `RunCompleted`. A variant field is not `#[non_exhaustive]`, so a new field on `RunCompleted` breaks every `RunCompleted { usage }` literal outside core,
  - the `collect()` rule: `stop_reason` comes from the terminal event, and it is `Completed` only after `RunCompleted`,
  - the typed-result rule for a run that did not complete (`AgentError::Incomplete { stop_reason }` from `collect_typed`; `parse_final` keeps its error type).

  SMA-804 rebases on SMA-803 and adds `StopReason::Stopped` support through its own terminal variant, `AgentEvent::RunStopped { reason: String, usage: TokenUsage }`. The two sessions agreed on this shape. It is subject to both spec gates. If SMA-803 is not on `main` when SMA-804 is ready, SMA-804 adds the same shape itself: the enum with `Completed` and `Stopped`, the field, `RunStopped`, and the `collect()` rule. SMA-803 then adds `Deferred`.
- C2. **"No call at a deferred boundary"** follows from SMA-803. A turn with a deferred call ends the run at that boundary, so the loop never reaches the checkpoint. SMA-804 adds a test for this after the rebase.
- C3. `RunContext` has no `Clone`. `handoff_child` and `subagent_child` are struct literals, so the compiler finds them. `to_tool_context`, the `AgentAsTool` rebuild and `runtime-temporal`'s `WorkerPosture::apply` use `new` plus builders, so the compiler does not find them. Each site needs an explicit decision (6.3).
- C4. Workspace MSRV is 1.94. The new trait uses the same object-safe `#[async_trait]` form as `PermissionPolicy<Ctx>` (`core/src/permission.rs:61`), which is generic and takes `&RunContext<Ctx>`.
- C5. Release: this is an additive `feat` in `paigasus-helikon-core` and some runtime crates, so release-plz makes patch bumps. There is no hand bump, unless the core-API-in-the-same-PR rule in `CLAUDE.md` applies. A runtime crate that calls `without_turn_checkpoint()` uses new core API in the same PR, so the plan must check that rule (see 6.5).

## 4. Decisions made with Sven

- **D1. Approach A: a trait object on `RunContext`.** `TurnCheckpoint<Ctx>` is an object-safe async trait, set with `RunContext::with_turn_checkpoint(Arc<dyn TurnCheckpoint<Ctx>>)`.
- **D2. Scope (a): only the top-level loop and handoff children.** `handoff_child` copies the field. `subagent_child` sets it to `None`. If the root agent is a workflow agent, the callback never fires. This is accepted for v1.

### Approaches that we considered

| | Approach | Trade-off | Result |
|---|---|---|---|
| A | `Arc<dyn TurnCheckpoint<Ctx>>` on `RunContext` | Same pattern as `PermissionPolicy<Ctx>`. Can get new default methods later. | **Chosen** |
| B | Boxed closure `Arc<dyn Fn(TurnBoundary) -> BoxFuture<'static, CheckpointDecision>>` | Easy to write in tests. The future cannot borrow the context, it is hard to extend, and no other core callback uses this form. | Rejected |
| C | New `HookEvent::OnTurnBoundary` plus `HookDecision::{InjectUserMessage, Stop}` | Reuses the hook registry. Hooks have many subscribers, so it needs merge rules, and each new decision needs a meaning at the seven other hook points. Does not match "one callback on `RunContext`". | Rejected |

## 5. The design

### 5.1 Public API (`paigasus-helikon-core`)

```rust
/// Where the loop is when it calls the checkpoint.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnBoundary {
    /// The turn whose tool results were just recorded (0-based, per loop).
    pub completed_turn: u32,
    /// The turn the next model call will start.
    pub next_turn: u32,
    /// The name of the agent whose loop is at the boundary (the handoff target in a child).
    pub agent: String,
    /// `RunContext::agent_depth()` of that loop.
    pub depth: u32,
}

impl TurnBoundary {
    /// Public so that hosts can unit-test their own checkpoint.
    pub fn new(completed_turn: u32, next_turn: u32, agent: impl Into<String>, depth: u32) -> Self;
}

/// What the loop does after the checkpoint.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum CheckpointDecision {
    /// Call the model as normal.
    Continue,
    /// Add these parts as one user message, then call the model.
    Inject(Vec<ContentPart>),
    /// End the run here with `StopReason::Stopped { reason }`.
    Stop { reason: String },
}

#[async_trait]
pub trait TurnCheckpoint<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    async fn at_boundary(&self, ctx: &RunContext<Ctx>, boundary: &TurnBoundary) -> CheckpointDecision;
}
```

On `RunContext`:

- the field `turn_checkpoint: Option<Arc<dyn TurnCheckpoint<Ctx>>>` (private, default `None`),
- `pub fn with_turn_checkpoint(self, cp: Arc<dyn TurnCheckpoint<Ctx>>) -> Self`,
- `pub fn without_turn_checkpoint(self) -> Self` (used by the HTTP runtimes, N6),
- `pub fn turn_checkpoint(&self) -> Option<&Arc<dyn TurnCheckpoint<Ctx>>>`.

Two new `AgentEvent` variants:

```rust
/// User content that the turn-boundary checkpoint added to the run. Not terminal.
UserMessageInjected { content: Vec<ContentPart> },
/// The run ended at a turn boundary because the checkpoint returned `Stop`
/// or a guardrail tripped on injected content. Terminal.
RunStopped { reason: String, usage: TokenUsage },
```

`is_terminal()` becomes `RunCompleted | RunFailed | RunDeferred | RunStopped`.

**The trait doc must state these rules:**

1. **Commit rule (5.3).** The run owns the injected content only after `UserMessageInjected` is in the event stream.
2. **Trust.** Injected content has user-role trust. A host that injects a message from a third party gives it more trust than a tool result has.
3. **Do not write to `ctx.session()`.** The recorder appends the run's events at finalize. An append from the callback lands before the run's own events.
4. **`Stop` is best effort.** A run that ends with a final answer has no next boundary, so a `Stop` decision can never apply to it.
5. **Time counts.** The wait in `at_boundary` counts against `RunConfig::timeout`. The loop has no separate timeout for the callback.
6. **Cancel safety.** If the runner drops the stream (cancel or timeout) while `at_boundary` waits, the future is dropped. `at_boundary` must be cancel-safe.
7. **Do not cancel from the callback** to stop the run. A cancel ends the run as `Cancelled`, which is the hard stop that this feature replaces. Return `Stop` instead.

The exact names are the builder's call within these semantics (ADR-0028 §8). The names above are the proposal. Public `///` docs must not link to `pub(crate)` items (for example `Interceptors` or `user_text_of`), or `rustdoc::private_intra_doc_links` fails the required `docs` gate.

### 5.2 Where the loop calls it

A **turn boundary** is the transition `ExecutingTools + ToolResults → CallingModel { turn: n }` in `transition()` (`loop_state.rs:408-447`). That is the point after the tool results of turn `n-1` are recorded and before the model call of turn `n`.

These are **not** boundaries:

- the first model call of a loop (turn 0),
- `CallingModel → Finalizing` (`loop_state.rs:365-390`): the structured-output final turn,
- `Finalizing → RepairingOutput` (`loop_state.rs:500-519`): a repair attempt,
- the SMA-803 resume step, which emits `ToolOutputItem` events before turn 0,
- a turn that ends in a handoff (the handoff child runs its own loop, with its own boundaries),
- a turn with a final answer (no tool calls). It ends the run.

Required event order at a boundary:

```text
ToolCallItem…  (turn n-1)
ToolOutputItem… (all results of turn n-1)      ← emitted by transition()
[checkpoint runs here]
UserMessageInjected { content }                ← only on Inject
TurnStarted { turn: n }                        ← last event of the transition() batch
TokenDelta… / MessageOutput… (turn n)
```

The `ToolOutputItem` events come from the *next* `transition()` call, not from the `ExecuteTools` arm in `agent.rs`. So the checkpoint must **not** run in the `ExecuteTools` arm. If it did, `UserMessageInjected` would come before the tool results, and `SessionRecorder` would store a user message between a tool call and its result (anti-goal A1).

**Mechanism (decided, so that the plan has one path).** The change is in the driver only. `transition()` does not change, because `runtime-temporal`'s `DurableDriver` shares it.

- The driver keeps the state before the call to `transition()`. When that state is `ExecutingTools` and `next_action` is `CallModel`, the batch is a boundary batch. Its last event is `TurnStarted { n }`.
- The driver yields every event of the batch before `TurnStarted { n }`. Then it calls the checkpoint, if one is set. Then it handles `TurnStarted { n }` as today (the turn span and the `OnTurnStart` hook).
- If the batch ends in `RunFailed` (the max-turns case), it is not a boundary batch, because `next_action` is `Terminate`.
- A `transition_unit` test locks the batch order that the driver depends on: all `ToolOutputItem` events, then `TurnStarted` last.

Decisions:

- On `Continue`, proceed as today.
- On `Inject(parts)`, follow 5.3 and 5.4.
- On `Stop { reason }`, follow 5.5.

**Max turns.** `transition()` checks `next_turn >= max_turns` before it produces `CallModel`. In that case it emits `RunFailed`, and the loop never reaches the checkpoint. So the callback does not run when the next turn would exceed `max_turns`, and the host keeps any event in its inbox. A test locks this in. The `CallingModel` max-turns check (`loop_state.rs:230`) is unchanged.

### 5.3 `Inject` details

Steps, in this order:

1. **Validate the parts.** The parts must be user parts: `Text`, `Image` or `Audio`. If a part is `ToolUse`, `ToolResult` or `Reasoning`, the run ends with `RunFailed` and a structured `AgentError` (a new variant on the `#[non_exhaustive]` enum, for example `InvalidInjection { reason }`). Nothing is yielded and nothing is recorded before this check. A `ToolResult` part with an unmatched `call_id` makes the provider reject the request. If the recorder stored it, `project()` would send it again on every later run of the session (anti-goal A5). This is a caller bug, so the run fails loudly.
2. **Empty list.** `Inject(vec![])` is treated as `Continue`. No event is emitted and no message is pushed. *(My call.)*
3. **Guardrails** (5.4).
4. **Commit.** Yield `UserMessageInjected { content: parts }`.
5. Push `Item::UserMessage { content: parts }` to `conversation`.
6. Append the same item to the `ModelRequest.messages` that `transition()` already built. The request was built from the conversation before the push, so this append is necessary.
7. Handle `TurnStarted { n }` and call the model.

**Commit rule (replaces the old A3).** The run owns the injected content if and only if `UserMessageInjected` is in the event stream (and so in `RunResult.events`). Before that event, the host owns it. Two cases follow from this rule, and tests lock both:

- After the commit, the run can still end before the model sees the content: an `OnTurnStart` `Deny`, a model error, a cancel or a timeout. The content is then stored, but no model answer follows. The host must not inject it again.
- Before the commit, a cancel or a timeout can drop the future while a guardrail waits (a guardrail can do network I/O). The content is then not stored, and the host still owns it.

Other rules:

- One `Inject` makes exactly one user message (one `Item::UserMessage`, one event, one `SessionEvent::UserMessage`). If several inbox items wait, the host merges them into one `Vec<ContentPart>`.
- `SessionRecorder::observe` gets one new arm: `UserMessageInjected { content }` maps to `SessionEvent::user_message(content)`. Then `project()` returns it as `Item::UserMessage` on the next run, with no new `SessionEvent` variant. So no session backend changes.
- Pending `OnTurnStart` hook injections (`pending_injections`, request-only `Item::System`) keep their current behaviour. They are added to the request after the injected user message, because the request is built first and the hook fires at `TurnStarted`.
- **Provider wire check (plan task).** After the boundary, the request holds `…ToolCall, ToolResult…, UserMessage`. Anthropic and Bedrock merge the tool result and the user message into one user turn. The Gemini translator emits two `role: user` contents one after the other (`providers-gemini/src/translate/request.rs:41-80`). The plan must verify this sequence on the wire, with a container where possible (repo practice). It must also verify that each translator drops or rejects an empty `Text` part, because Anthropic rejects empty text blocks. The same sequence can already occur today after a cancel during a tool call. If a provider rejects it, the fix is in that translator.

### 5.4 Guardrails on injected content

- The loop calls the existing `interceptors.run_input_guardrails(text)`. `text` joins the `Text` parts of the injected content, with the same join rule as `user_text_of`.
- The guardrails run even if there is no `Text` part (`text` is then `""`). This matches the seed-input path, which also always runs. A guardrail cannot check image or audio parts, because `GuardrailInput` has only `UserText`. This is a known limit, and the doc comment says so.
- A guardrail that returns `Err` counts as a tripwire (`control.rs:98-105`), as on the seed input. So an outage of a moderation API stops the run. A test locks this behaviour.
- On a tripwire:
  1. Yield `GuardrailTriggered { kind, info }`.
  2. Do **not** yield `UserMessageInjected`, do not push to the conversation, and do not record the content. Content that a guardrail rejected must not reach the model or the stored history. There is also a stronger reason: the seed guardrail checks the text of every `UserMessage` in the merged history (`agent.rs:811`, `runtime-tokio/src/lib.rs:93-95`). Stored rejected content would trip the guardrail on every later run, and the session would be blocked.
  3. End the run as for `Stop` (5.5), with `reason = "input guardrail tripwire"`.
- The issue requires `Stop`, so a tripwire ends with `RunStopped` / `StopReason::Stopped`, **not** with `RunFailed` / `AgentError::Guardrail`. This is different from a tripwire on the seed input, which stays a failure.
- The reason text alone does not identify a guardrail stop, because a host can use the same text. The reliable signal is a `GuardrailTriggered` event before `RunStopped`, with no `UserMessageInjected` between them. The trait doc says so.

### 5.5 `Stop` details

- `Stop` ends the run with `AgentEvent::RunStopped { reason, usage }`. It is not `RunFailed`, and the `FailureSlot` stays empty (anti-goal A2).
- **One exit path.** The `Stop` path yields `RunStopped` and then goes through the existing `NextAction::Terminate` arm (`agent.rs:1035-1047`). That arm fires `OnRunComplete` after the terminal event, and records usage on `run_span`. The order is: `RunStopped`, then `OnRunComplete`. The driver must not add a new early `return` that skips these steps. `OnRunComplete` fires once per loop, as today.
- `usage` is the usage that the loop state holds at the boundary (`ExecutingTools.usage`, which `transition()` moves to `CallingModel { usage }`). It covers every turn that ran.
- The open turn span of turn `n-1` closes as at any other run end. No span for turn `n` opens, because `TurnStarted { n }` is not handled.
- The `Stop` path does not use `LoopState::Done`. So the output guardrails do not run, and no `Finalizing` turn runs for an agent with `output_type`.
- `RunResultStreaming::collect()` returns `Ok(RunResult { stop_reason: Stopped { reason }, .. })`, with `usage` from the event.
- `final_output` and the typed results follow the SMA-803 rule for a run that did not complete. At the time of writing, the rule is:
  - `final_output` is the text of the last `MessageOutput`, or `""`. That text did not go through the output guardrails. The doc of `RunResult::final_output` must say this.
  - `collect_typed` keeps `Result<RunResult<T>, AgentError>`. It returns the new `AgentError::Incomplete { stop_reason }` when the stop reason is not `Completed`.
  - `parse_final` keeps `Result<RunResult<T>, serde_json::Error>`. It copies `stop_reason` into the typed result, and its doc tells the caller to check `stop_reason` first.
  - There is no `RunError::Incomplete`. The challenger found that a new error type on `parse_final` is a breaking change, and SMA-803 changed its spec because of this finding.

  SMA-804 follows whatever SMA-803 decides at its spec gate.
- **Handoff child.** A `Stop` in a handoff child ends the child loop with `RunStopped`. The parent's re-yield loop (`agent.rs:1126-1137`) today adds `parent_usage` only to `RunCompleted`. It needs an explicit `RunStopped` arm that adds `parent_usage` and records the span usage, the same as the `RunDeferred` arm in SMA-803 §4.7. Then the whole run ends with `Stopped`, and `usage` covers the parent turns and the child turns.
- The tool calls of turn `n-1` all have results before the checkpoint runs, so a `Stop` cannot leave a regular tool call half done. In a run **without a handoff**, `SessionRecorder::drain` therefore has no orphan `ToolCalled` to fix. A run with a handoff already has an orphan today: the transfer `ToolCallItem` (`loop_state.rs:298-302`) never gets a `ToolOutputItem`, so `drain` adds a synthetic `ToolReturned` at the end of the log. That behaviour existed before this change. It is out of scope. It is filed as SMA-810.
- **Observability.** The checkpoint call has its own span, `agent.turn_checkpoint`, with an attribute for the decision kind (`continue`, `inject`, `stop`). On `Stop`, `run_span` records the stop reason. A `Stop` is a success, so `otel.status_code` is not `ERROR`, also for a guardrail stop. The exact attribute names are the builder's call, and the `workspace-lints` tracing-target check must pass.

### 5.6 Cancellation

The loop does not poll the `CancellationToken`. `TokioRunner::controlled()` drops the stream when a cancel or a timeout happens. If the stream is dropped while `at_boundary` or an injected-content guardrail waits, those futures are dropped too. The commit rule (5.3) tells the host who owns the content in each case. The session then holds every tool result of turn `n-1`, because they were all recorded before the checkpoint.

## 6. Change map

### 6.1 `paigasus-helikon-core`

- A new module, for example `src/checkpoint.rs`, with `TurnBoundary`, `CheckpointDecision` and `TurnCheckpoint`. Re-export these at the crate root, with `///` docs on every public item. The trait doc has the seven rules in 5.1.
- `src/context.rs`: the field, the builders, the getter, and the child sites (6.3).
- `src/agent.rs`:
  - the boundary hook in the driver (5.2), the inject steps (5.3), the guardrail call (5.4), the `Stop` path through the `Terminate` arm (5.5), and the span (5.5),
  - the new `AgentError` variant for invalid injected parts (5.3),
  - `UserMessageInjected` and `RunStopped`,
  - `is_terminal()` and its doc, which today says "the two events",
  - the "Seventeen variants" doc (`agent.rs:348`),
  - `terminal_tests`: `classify`, `every_variant()`, the variant count, and the terminal-count test (`exactly_two_variants_are_terminal`, which counts 4 terminals after SMA-803 and SMA-804),
  - the handoff re-yield arm for `RunStopped` (5.5).
- `src/runner.rs`: `collect()` maps `RunStopped` to `StopReason::Stopped` and takes `usage` from the event. `collect_typed()` and `parse_final()` follow the SMA-803 rule.
- `src/session.rs`: the `observe` arm, and a recorder unit test.
- Tests and snapshots: `tests/serde_roundtrip.rs` plus new insta snapshots for `UserMessageInjected` and `RunStopped`. CI does not accept pending snapshots, so the `.snap` files must be committed. Check that `tests/loop_happy_path.rs` (`event_kind`) still compiles.

### 6.2 Other crates

- **`runtime-tokio`.** `run_streamed` finalizes the session only when `is_terminal()` is true (`runtime-tokio/src/lib.rs:219-231`). With `RunStopped` in `is_terminal()`, this needs no code change. A test locks it (7, test 13).
- **HTTP runtimes (N6).** `runtime-axum` (`server.rs:161`), `runtime-actix` and `runtime-agentcore` (`server.rs:173`) build the `RunContext` through `ContextProvider`. Each calls `without_turn_checkpoint()` on that context, and logs a `tracing::warn!` if a checkpoint was set. The doc of each `ContextProvider` says that the checkpoint is not supported. A test in each crate shows that a checkpoint from the provider never fires. Sven decided this at GATE 1 (Q1).
- **Exhaustive or semi-exhaustive matches outside core.** The builder checks each one and adds an explicit arm where the event has a meaning:
  - `runtime-agentcore`: `agui/map.rs` (today an unknown variant becomes a `helikon.unknown` CUSTOM frame at `map.rs:304-306`. The two new variants get explicit arms), `a2a/rpc.rs`, `invoke.rs`, `ws.rs`, `agui/sse.rs`, `agui/ws.rs`,
  - `runtime-axum`: `dto.rs`, `event_log.rs`, `registry.rs`,
  - `runtime-actix`: `dto.rs`, `event_log.rs`, `registry.rs`,
  - `paigasus-helikon-mcp`: `server.rs`,
  - `evals`: `evaluators/trajectory.rs`,
  - `cli`: `repl.rs` (it can print both events).
- **`runtime-temporal`.** No change to the driver. `TemporalRunner` logs a `tracing::warn!` when `ctx.turn_checkpoint().is_some()` (`runtime-temporal/src/runner.rs:184` already reads the context). Add a doc note on `TemporalRunner` that the checkpoint is not supported (N1).

### 6.3 Child-context sites

| Site | New field |
|---|---|
| `RunContext::handoff_child` | copy (`Arc` clone) |
| `RunContext::subagent_child` | `None` (D2) |
| `RunContext::to_tool_context` | not carried (`ToolContext` has no field for it) |
| `AgentAsTool` sub-context rebuild | not set (N3), so it is `None` because `RunContext::new` sets `None`. A regression test in `core/tests/subagent_propagation.rs`, next to the SMA-803 flag test. |
| `runtime-temporal` `WorkerPosture::apply` | not copied (N1). Its field-enumeration test does not change, because this is not a posture field. |

### 6.4 Documentation

- The mdBook page `docs/book/src/concepts/agent-loop.md`:
  - add both new variants to its event list (lines 56-66),
  - add a section "Turn-boundary checkpoint". The section covers the three decisions, the event order, the commit rule, the guardrail rule, the max-turns rule, the seven trait-doc rules, and the non-goals (Temporal, HTTP runtimes, workflow sub-agents, `AgentAsTool`).
- `crates/paigasus-helikon-core/README.md`: add one line or a short example only if the README lists `RunContext` capabilities. The builder checks this. The facade and root READMEs do not change, because the crate roster and the features do not change.
- The CHANGELOG files are written by release-plz from the commit messages. No hand edit is necessary.

### 6.5 Release check

If a runtime crate calls the new `without_turn_checkpoint()` in the same PR that adds it to core, the `CLAUDE.md` rule applies: `cargo publish --verify` builds the runtime against the registry core. The plan must then bump core and the facade by hand in this PR, or put the runtime change in a follow-up PR after core is released. The plan decides which, with Opus, as the release routing rule in `CLAUDE.md` requires.

## 7. Tests (the "Done when" list)

All core tests are in `paigasus-helikon-core/tests/` and use `common::MockModel`, `MockTool` and a recording model (the same as `injection.rs`). A test checkpoint returns scripted decisions and records each `TurnBoundary` that it gets.

1. **Continue.** Script: tool call, then a final answer. The checkpoint is called once, with `completed_turn = 0` and `next_turn = 1`. The run completes, and the event stream is the same as with no checkpoint.
2. **Inject.**
   - The model request of turn 1 ends with `Item::UserMessage { injected parts }`, after the `ToolResult`.
   - The event order is exactly: `ToolOutputItem`, `UserMessageInjected`, `TurnStarted { 1 }`.
   - `SessionRecorder` gives `…ToolReturned, UserMessage(injected)…`, and `project()` round-trips it.
   - A `runtime-tokio` test with a counting session shows that the stored session holds the injected `UserMessage` after the `ToolReturned`.
3. **Inject with empty parts** behaves as `Continue`.
4. **Inject with a tool part** (`ToolResult`) ends with `RunFailed` and the new `AgentError` variant. Nothing is yielded before it, and nothing is recorded.
5. **Stop.**
   - `collect()` returns `Ok` with `StopReason::Stopped { reason }`, and `usage` covers every turn that ran.
   - There is no second model call, no `TurnStarted { 1 }`, no `RunFailed`, and the `FailureSlot` is empty.
   - Every `ToolCalled` has a `ToolReturned`.
   - `OnRunComplete` fires once, after `RunStopped`.
6. **Guardrail tripwire on injected content.** A guardrail trips on the injected text. The stream has `GuardrailTriggered`, then `RunStopped`, and no `UserMessageInjected`. There is no second model call, and the content is not in the recorder output.
7. **Guardrail error on injected content.** A guardrail returns `Err`. The result is the same as test 6.
8. **Max turns.** With `max_turns = 1` and a tool call on turn 0, the checkpoint is never called, and the run fails with `MaxTurnsExceeded` as today.
9. **Handoff child.** The checkpoint fires at a boundary inside the handoff target, with `agent = target` and `depth = 1`. A `Stop` there ends the whole run with `Stopped`, and `usage` equals the parent turns plus the child turns. This test does not check "every `ToolCalled` has a `ToolReturned`", because of the transfer-call orphan that already exists (5.5).
10. **No call in an `AgentAsTool` sub-run.** The parent has a checkpoint, and the sub-agent makes a tool call inside its own loop. The checkpoint records only the parent's boundaries. Plus the regression test in `subagent_propagation.rs` (6.3).
11. **No call in workflow sub-agents.** A `SequentialAgent` root with an `LlmAgent` child that uses tools: the checkpoint is never called. Also a unit test that `subagent_child()` has no checkpoint and `handoff_child()` has the same `Arc` (`Arc::ptr_eq`).
12. **No call at the non-boundaries.** An `output_type` agent with one tool turn gets exactly one checkpoint call (not one for `Finalizing` or a repair).
13. **`runtime-tokio` `run_streamed` with `Stop`.** The session is finalized and holds every tool result.
14. **`Stop` on an agent with `output_type` and tools.** No `Finalizing` turn runs, and `collect_typed` returns `AgentError::Incomplete { stop_reason: Stopped { .. } }`.
15. **Cancel while `at_boundary` waits.** The result is `Cancelled`. The session holds all tool results of turn `n-1`, and no injected message.
16. **Commit rule after the commit.** `Inject`, then an `OnTurnStart` `Deny`. The injected content is stored, and the run fails with `HookDenied`.
17. **Commit rule before the commit.** `Inject`, then a cancel while a guardrail waits. The content is not stored, and the result is `Cancelled`.
18. **No call at a deferred boundary** (after the rebase on SMA-803). A turn with one deferred call ends with `Deferred`, and the checkpoint is never called.
19. **Event order lock** in `tests/transition_unit.rs`: the `ExecutingTools + ToolResults` batch is all `ToolOutputItem` events, then `TurnStarted` last.
20. **Object safety:** `TurnCheckpoint<()>` in `tests/object_safety.rs`.
21. `AgentEvent` serde round trip and the snapshots for both new variants, and the updated `terminal_tests` (`UserMessageInjected` is not terminal, `RunStopped` is terminal).
22. **HTTP runtimes:** a checkpoint from `ContextProvider` never fires (one test per runtime crate).

The CI gates in `CLAUDE.md` must pass: fmt, clippy `-D warnings`, `cargo test --workspace --all-features`, docs `-D warnings`, doc coverage, `build-no-default-features`, and `mdbook build docs/book`.

## 8. Left to the builder

- The exact names (trait, method, events, builders, `AgentError` variant, span attributes), within the semantics above.
- Whether `TurnBoundary` gets more fields (for example, the tool call ids of the completed turn). It is `#[non_exhaustive]`, so fields can be added later. A run-wide boundary counter is not necessary: a host can count calls itself, and `agent` plus `depth` identify the loop.
- The exact AG-UI event names.

## 9. Calls I made (please check)

1. The callback does not run when the next turn would exceed `max_turns` (5.2).
2. `Inject(vec![])` is `Continue` (5.3).
3. Tool and reasoning parts in injected content fail the run with a new `AgentError` variant, before anything is recorded (5.3). The first draft let them through. The challenger showed that they can make the stored session unusable.
4. On a guardrail tripwire, the injected content is not recorded anywhere (5.4).
5. The guardrails run even when the injected content has no text, and a guardrail error is a tripwire (5.4).
6. `OnRunComplete` fires on `Stop`, once per loop, after `RunStopped` (5.5).
7. The two new events get explicit arms in the AG-UI mapper and in the other runtimes (6.2).
8. The mechanism is in the driver only. `transition()` does not change (5.2).
9. The handoff-transfer orphan is a separate issue, SMA-810 (5.5).

## 10. Questions for Sven (answered)

- **Q1. The HTTP runtimes (axum, actix, agentcore).** A host can install a checkpoint through `ContextProvider`. Today a `Stop` there would show as `Failed` (axum/actix), as no run end (AG-UI) and as `Completed` (A2A). My proposal is N6: the runtimes clear the checkpoint and log a warning, as SMA-803 does for deferral. The other option is to map `RunStopped` in each runtime. But axum's `RunStatus` is not `#[non_exhaustive]`, so a new status there is a breaking change for those crates.
  **Decision (GATE 1):** use the proposal. The runtimes clear the checkpoint and log a warning.
