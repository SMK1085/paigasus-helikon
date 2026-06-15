# SMA-392 — Wire Session persistence into the run lifecycle

**Status:** Design approved
**Date:** 2026-06-15
**Linear:** [SMA-392](https://linear.app/smaschek/issue/SMA-392/wire-session-persistence-into-the-run-lifecycle-history-load-event)
**Branch:** `feature/sma-392-wire-session-persistence-into-the-run-lifecycle-history-load`

## Problem

The `Session` backends (`MemorySession`, `SqliteSession`), the `SessionEvent` log
shape, and the `project` projection all exist (SMA-318), and `TokioRunner` exposes
a `finalize()` seam guaranteed to run on every run exit (SMA-321). But nothing
connects a `Session` to a run:

- `LlmAgent::run` seeds its conversation only from `AgentInput::messages` and never
  reads `ctx.session()`, so multi-turn sessions never resume.
- `TokioRunner::finalize()` is a placeholder empty `session.append(&[])`, so no run
  output is ever persisted.

This ticket wires both halves: **load** persisted history at run start and **write**
the run's semantic items at run exit.

## Decisions

1. **Load precedence — concatenate.** `conversation = snapshot.messages ++ input.messages`.
   The session owns durable history; `input.messages` is the *new turn*. Multi-turn
   resume "just works" — callers send only the new user message each run.
2. **Failure policy — partial transcript on every exit.** `finalize()` appends
   whatever completed items accumulated before exit (the new user message plus any
   finished assistant/tool turns), on all four exit paths (completed / agent-failure
   / cancel / timeout). Semantic items are post-aggregation, so no half-message can
   leak. **Retry contract:** re-run with empty `input.messages` (the session already
   owns the turn) to avoid re-appending the user message.
3. **Architecture — runner-owned.** All session I/O lives in the runner. `LlmAgent`
   stays fully session-agnostic (no changes to `agent.rs`). A reusable
   `SessionRecorder` helper lives in `paigasus-helikon-core`, next to `SessionEvent`
   / `project`, so it is unit-testable without a tokio runtime and reusable by the
   SMA-332 durable runners.

## Architecture

### Components

- **`SessionRecorder`** (new, `paigasus-helikon-core`, `src/session.rs`): accumulates
  `SessionEvent`s for one run. Pre-seeded with the run's `input.messages`; observes
  the `AgentEvent` stream; tracks the active agent name; drains to a
  `Vec<SessionEvent>`.
- **`TokioRunner`** (`paigasus-helikon-runtime-tokio`): performs the load (snapshot →
  merge into input), wraps the controlled stream with a recording layer feeding the
  recorder, and drains + appends in `finalize()`.
- **`LlmAgent`**: unchanged — still seeds from `AgentInput::messages` only. The runner
  hands it a *merged* input that already contains the loaded history.

### Load (run start)

In both `TokioRunner::run` and `run_streamed`, before calling `agent.run`:

```text
snap   = ctx.session().snapshot().await        // ConversationSnapshot { messages }
merged = AgentInput { messages: snap.messages ++ input.messages }
stream = agent.run(ctx, merged).await?
```

`LlmAgent` then builds `[System(instructions)] ++ merged.messages`, so the model sees
`[System(instructions)] ++ snapshot.messages ++ input.messages`.

On a snapshot read error, the run fails before the agent starts (a corrupt/unreadable
session is a hard error at load; this is distinct from §"Error handling" which covers
the best-effort *write* side).

### Record + write

A recording layer (`.inspect()` over the `controlled_stream`, feeding a shared
`Arc<Mutex<SessionRecorder>>`) sits between `controlled()` and `collect()` / the
synthetic stream, so **both** `run` and `run_streamed` capture identically and the
existing finalize-ordering invariants are untouched.

The recorder is **pre-seeded with `input.messages`** — the new user turn is not
re-emitted on the `AgentEvent` stream, so the runner records it directly. The recorder
then observes the stream. `finalize()` drains the recorder and appends the batch.

Mapping (every event stamped `ts` at record time):

| Source | → `SessionEvent` |
|---|---|
| `input.messages` `Item::UserMessage { content }` | `UserMessage { content, ts }` |
| `input.messages` `Item::AssistantMessage { content, agent }` | `AssistantMessage { content, agent: agent ∨ tracked, ts }` |
| `input.messages` `Item::ToolCall { call_id, name, args }` | `ToolCalled { call_id, name, args, ts }` |
| `input.messages` `Item::ToolResult { call_id, content }` | `ToolReturned { call_id, content, ts }` |
| `input.messages` `Item::System { .. }` | *(skipped — no `SessionEvent` variant; debug-logged)* |
| stream `MessageOutput { Item::AssistantMessage { content, agent } }` | `AssistantMessage { content, agent: agent ∨ tracked, ts }` |
| stream `ToolCallItem { Item::ToolCall { .. } }` | `ToolCalled { call_id, name, args, ts }` |
| stream `ToolOutputItem { Item::ToolResult { .. } }` | `ToolReturned { call_id, content, ts }` |
| stream `HandoffItem { from, to }` | `HandoffOccurred { from, to, ts }` *(audit; projects to 0 messages)* |
| stream `RunStarted { agent }` / `AgentUpdated { agent }` | *(no event; updates the recorder's tracked active agent name)* |
| all other `AgentEvent`s (deltas, lifecycle, control, terminal) | *(ignored — not semantic items)* |

**Agent-name resolution.** `SessionEvent::AssistantMessage` requires `agent: String`,
but `Item::AssistantMessage.agent` is `Option<String>` (the wire format can lose
attribution). Resolution: use `item.agent` when `Some`, otherwise the recorder's
tracked active agent (updated from `RunStarted` / `AgentUpdated`). This is the exact
hook the `Item::AssistantMessage.agent` doc comment anticipates ("the session log
keeps `agent: String` because the runner always knows which agent emitted").

**Ordering.** Buffer order is `[input events…] ++ [streamed items in stream order]`,
which matches conversational order.

### `finalize()` signature

`finalize` is private to `runtime-tokio`; the public `Runner` trait signature is
unchanged. It changes from:

```rust
async fn finalize(session: &Arc<dyn Session>)               // before: empty append
async fn finalize(session: &Arc<dyn Session>, recorder: &Arc<Mutex<SessionRecorder>>)  // after
```

and appends `recorder.lock().drain()` instead of `&[]`.

## Exit-path policy

`finalize()` runs on all four paths (the SMA-321 guarantee) and appends whatever the
recorder accumulated:

- **Completed** → full turn (user + assistant/tool items).
- **Agent-failure / cancel / timeout** → user message + any *completed* items so far.
  No half-messages (items are post-aggregation). Retry with empty `input.messages`.

## Error handling

Session **write** failures in `finalize()` are **logged (`tracing::warn!`) and
swallowed** — ephemeral persistence is best-effort and must not change the run's
outcome (cancel still returns `Cancelled`, a completed run still returns its
`RunResult`). This replaces the placeholder's silent `let _ =` discard with an
observable warning, as the SMA-321 placeholder comment anticipated.

Session **read** failure at load (`snapshot()`) is a hard error — the run cannot
faithfully resume from an unreadable session, so it fails before the agent starts.

## What is *not* persisted

The `System(instructions)` prefix is **never** written: instructions come from agent
config and are re-applied on every run, so persisting them would duplicate them on
each resume. `input.messages` `System` items are likewise skipped (no `SessionEvent`
variant exists for them).

Consequence for acceptance criterion #2: `project(persisted_log)` equals the model's
conversation **minus** the re-derived `System(instructions)` prefix.

## Testing

- **Multi-turn round-trip** (acceptance #1): run twice against one `MemorySession`;
  assert turn 2's model request contains turn 1's user + assistant messages.
- **Projection identity** (acceptance #2): `project(session.events())` equals the items
  the model saw, sans the `System(instructions)` prefix.
- **Finalize-on-every-exit contents** (acceptance #3): extend the existing
  `finalize_runs_on_every_run_exit` test to assert the *contents* (not just
  `append_count`) on each of completed / agent-failure / cancel / timeout.
- **`SessionRecorder` unit tests** (core, no runtime): input pre-seed ordering,
  per-variant mapping, agent-name resolution (`item.agent` vs tracked), `System`-skip.
- **Streamed parity**: `run_streamed` persists the same events as `run` for an
  identical scripted agent.

## Scope

**In scope:** load, record, write, exit-path policy, `SessionRecorder`.

**Out of scope:**

- Compaction (`CompactingSession<S>` / `LoopState::Compacting`) — SMA-330.
- Structured error at the Runner boundary (`RunResult` / `RunError`) — SMA-346.

## Release / versioning

Both affected crates are already-released (not stubs):

- `paigasus-helikon-core` — additive `SessionRecorder` (its enums are already
  `#[non_exhaustive]`); patch/minor bump.
- `paigasus-helikon-runtime-tokio` — behavior-only change, no public API change; patch
  bump.

release-plz cascades both automatically in dependency order — **no manual ascend
ritual** (that ritual applies only to stubs ascending from `0.0.0`, and to
already-released consumers no manual core bump is needed).
