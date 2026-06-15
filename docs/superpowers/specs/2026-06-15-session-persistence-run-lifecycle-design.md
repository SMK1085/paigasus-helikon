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
   leak. **Retry contract:** retry via `resume()` (Decision 5) — it continues from the
   session with no new turn, so the user message is not re-appended.
3. **Architecture — runner-owned.** All session I/O lives in the runner. `LlmAgent`
   stays fully session-agnostic (no changes to `agent.rs`). A reusable
   `SessionRecorder` helper lives in `paigasus-helikon-core`, next to `SessionEvent`
   / `project`, so it is unit-testable without a tokio runtime and reusable by the
   SMA-332 durable runners.
4. **Tool-call/result pairing on drain.** When `SessionRecorder` drains,
   any `ToolCalled` with no matching `ToolReturned` (left by a cancel/timeout *during*
   tool execution — see "Dangling tool calls" below) gets a **synthesized**
   `ToolReturned { content: "tool call did not complete (run cancelled/timed out)" }`.
   The persisted log therefore always `project()`s to a provider-valid conversation,
   and a resumed model sees that the call was attempted but interrupted.
5. **Explicit `resume()` entry point.** Add `Runner::resume` (and
   `resume_streamed`) as thin **default** trait methods equal to `run` with an empty
   `AgentInput`. This gives "continue from the session, no new turn" an explicit call
   so callers don't have to remember to blank `input` on retry, without burdening the
   stub runners (default body). `run(ctx, new_turn)` remains "append this new turn."
6. **Read hard-fails, write is best-effort.** A `snapshot()` read failure
   at load fails the run before the agent starts (loud failure beats silently behaving
   as a fresh conversation). A `finalize()` write failure is logged and swallowed. The
   asymmetry is deliberate and documented; refining read failures into
   corruption-vs-transient is left for when `SessionError` classifies error kinds.

## Architecture

### Components

- **`SessionRecorder`** (new, `paigasus-helikon-core`, `src/session.rs`): accumulates
  `SessionEvent`s for one run. Pre-seeded with the run's `input.messages`; observes
  the `AgentEvent` stream; tracks the active agent name; on `drain()` it pairs tool
  calls with results (synthesizing a placeholder for any unmatched `ToolCalled` —
  Decision 4) and returns a `Vec<SessionEvent>`.
- **`Runner` trait** (`paigasus-helikon-core`): gains default `resume` /
  `resume_streamed` methods (Decision 5), each delegating to `run` / `run_streamed`
  with `AgentInput::new()`. Additive, non-breaking.
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

`resume()` is exactly this path with `input.messages` empty: `merged == snapshot.messages`,
so the model continues from persisted history with no new turn.

On a snapshot read error, the run fails before the agent starts (Decision 6: a
corrupt/unreadable session is a hard error at load; distinct from §"Error handling",
which covers the best-effort *write* side).

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
tracked active agent. The recorder is seeded at construction with the root agent's
`name()` (the runner already has the `agent` handle), and the tracked value is updated
from `RunStarted` / `AgentUpdated` — so `tracked` is always defined, even for a
pre-seed `input.messages` assistant item before any stream event. In practice
`item.agent` is authoritative: `build_items` (`agent.rs:467-470`) always sets it for
LlmAgent messages, and sub-agent messages are forwarded with their own attribution
(`agent.rs:1137-1148`). This is the exact hook the `Item::AssistantMessage.agent` doc
comment anticipates ("the session log keeps `agent: String` because the runner always
knows which agent emitted").

**Ordering.** Buffer order is `[input events…] ++ [streamed items in stream order]`,
which matches conversational order.

### Dangling tool calls

The loop `yield`s a turn's `ToolCallItem`s (`agent.rs:840-890`) **before** awaiting
`run_tools_concurrent` (`agent.rs:1024`); the matching `ToolOutputItem`s are emitted
only by the *next* transition. So a cancel/timeout while the controlled stream is
suspended inside that await drops the generator with `ToolCalled`s recorded but **no**
`ToolReturned`s — and because outcomes are emitted as one post-await batch, it's the
whole turn's calls, not just one. (Decision 2's "no half-message" property holds for
individual *messages* but not for tool call/result *pairs* across a turn.)

A naively-persisted dangling `ToolCalled` makes the next run's first model request
malformed — OpenAI and Anthropic both reject an assistant tool call with no
corresponding tool result (the same wire-format constraint SMA-324 handled for handoff
transcripts).

**Fix (Decision 4):** `SessionRecorder::drain()` pairs `ToolCalled`/`ToolReturned` by
`call_id`; for every `ToolCalled` lacking a result it appends a synthesized
`ToolReturned { call_id, content: [Text "tool call did not complete (run cancelled/
timed out)"], ts }`. The drained log then always `project()`s to a provider-valid
conversation.

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
- **Agent-failure / cancel / timeout** → user message + any *completed* items so far
  (plus synthesized results for any interrupted tool calls — Decision 4). No
  half-messages (items are post-aggregation). Retry via `resume()` (Decision 5).

## Error handling

Session **write** failures in `finalize()` are **logged (`tracing::warn!`) and
swallowed** — ephemeral persistence is best-effort and must not change the run's
outcome (cancel still returns `Cancelled`, a completed run still returns its
`RunResult`). This replaces the placeholder's silent `let _ =` discard with an
observable warning, as the SMA-321 placeholder comment anticipated.

Session **read** failure at load (`snapshot()`) is a hard error — the run cannot
faithfully resume from an unreadable session, so it fails before the agent starts.
The read/write asymmetry is deliberate (Decision 6).

**Streamed drain requirement.** `run_streamed` persists via the
`async_stream` generator, which calls `finalize()` just before the terminal yield. If
a consumer drops the stream *before* the terminal, the generator is dropped at its
suspension point and `finalize()` never runs — so a partial turn (or nothing) is
persisted, silently. This is documented on `run_streamed`: the run is not "done" (and
the session is not written) until the stream is driven to its terminal. The
non-streamed `run()` is unaffected — `collect()` always drains.

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
  per-variant mapping, agent-name resolution (`item.agent` vs tracked), `System`-skip,
  and **drain pairing** — an unmatched `ToolCalled` yields a synthesized `ToolReturned`.
- **Cancel-mid-tool then resume:** cancel a tool-using
  run *during* tool execution, then `run`/`resume` against the same session; assert the
  second run's first model request is well-formed (no dangling tool call) and the
  synthesized placeholder result is present.
- **`resume()` round-trip:** after a completed turn, `resume()` with no
  input continues from persisted history; assert the model sees the prior turn and no
  duplication occurs.
- **Multi-agent attribution:** a handoff run records the target agent's
  assistant messages under the *target's* name.
- **Streamed parity**: `run_streamed` persists the same events as `run` for an
  identical scripted agent.

## Scope

**In scope:** load, record, write, exit-path policy, `SessionRecorder` (with drain
pairing), `Runner::resume` / `resume_streamed`.

**Out of scope:**

- Compaction (`CompactingSession<S>` / `LoopState::Compacting`) — SMA-330. **Note:**
  without compaction, each run loads the *entire* history into the model
  request, so long multi-turn sessions grow context (token cost + latency) every turn.
  Expected; compaction is the prerequisite for practical long-lived sessions.
- Structured error at the Runner boundary (`RunResult` / `RunError`) — SMA-346.
- Corruption-vs-transient classification of read failures (Decision 6) — deferred until
  `SessionError` distinguishes error kinds.

## Release / versioning

Both affected crates are already-released (not stubs):

- `paigasus-helikon-core` — additive `SessionRecorder` + additive default
  `Runner::resume` / `resume_streamed` methods (its enums are already
  `#[non_exhaustive]`; default trait methods are non-breaking); patch/minor bump.
- `paigasus-helikon-runtime-tokio` — behavior-only change, no public API change (it
  inherits `resume` from the trait default); patch bump.

release-plz cascades both automatically in dependency order — **no manual ascend
ritual** (that ritual applies only to stubs ascending from `0.0.0`, and to
already-released consumers no manual core bump is needed).
