# SMA-392 Design Review — Wire Session persistence into the run lifecycle

**Reviews:** [`2026-06-15-session-persistence-run-lifecycle-design.md`](./2026-06-15-session-persistence-run-lifecycle-design.md)
**Reviewer perspective:** staff engineering — correctness of the load/record/write cycle, resume invariants, and downstream blast radius
**Date:** 2026-06-15
**Verdict:** **Approve with changes.** The architecture is clean and accurately grounded — runner-owned I/O, `LlmAgent` untouched, a `SessionRecorder` in core, the `.inspect()` layer between `controlled()` and `collect()`. I verified the load-bearing facts: the loop really does emit `ToolCallItem`/`ToolOutputItem`, the `SessionEvent` shapes match exactly, input isn't double-emitted, and `finalize` runs on every exit. The change to make before the plan is a **resume-invariant bug**: a cancel/timeout during tool execution persists tool calls with no matching results, so the next run resumes from a provider-invalid conversation (**H1**) — and the "send only the new turn / retry with empty input" contract (**H2**) compounds it in exactly that scenario.

## What this was checked against

- **Linear** [SMA-392](https://linear.app/smaschek/issue/SMA-392) (builds on SMA-318 backends; implements the SMA-321 `finalize` seam; complements SMA-330 compaction, SMA-346 structured error).
- **Notion** [Sessions](https://www.notion.so/355830e8fbaa81d79e15d62ac40954e8) + ADR *"Session is an append-only event log, not a message list"* — the spec aligns with this (project()→snapshot→seed; append `SessionEvent`s).
- **Code (ground truth; core `0.5.2`, runtime-tokio `0.1.7`)** — `core/src/{session.rs, agent.rs, loop_state.rs, item.rs}`, `runtime-tokio/src/lib.rs`, `sessions-sqlite`.

Severity legend: **H** = high · **M** = medium · **L** = low. Each item ends with a concrete **Correction**.

---

## H — High-severity

### H1. Cancel/timeout during tool execution persists dangling tool calls → resume builds a provider-invalid conversation

This is the central correctness gap, and it's confirmed in code. The loop yields `ToolCallItem` for the turn's calls **before** executing them, then runs `run_tools_concurrent(...).await`, then yields `ToolOutputItem` for **all** outcomes in one batch *after* that await completes. So the recorder (`.inspect()` on the controlled stream) records the `ToolCalled` events, and then — if the run is cancelled or times out *during* `run_tools_concurrent` — the whole tool-execution await is dropped and **no `ToolOutputItem`s are ever emitted**. `finalize()` (which runs on every exit, including cancel/timeout) then appends a turn containing `UserMessage` + N `ToolCalled` with **zero** `ToolReturned`.

On the next `run`, `snapshot()` → `project()` yields a conversation ending in unmatched `ToolCall`(s) with no `ToolResult`. The first model request is then malformed — OpenAI and Anthropic both reject an assistant tool-call that has no corresponding tool result (the exact wire-format constraint SMA-324 had to handle for handoff transcripts). So a cancel mid-tool **poisons the session**: every subsequent resume fails at the first model call.

The spec's "no half-message can leak (items are post-aggregation)" is true for individual *messages* but does not cover dangling tool-call/result **pairs** across a turn — which is precisely what a mid-tool cancel produces (and it's the whole turn's calls, not just one, because the outcomes are emitted as a single post-await batch).

**Correction.** Make `finalize()`/`SessionRecorder` guarantee the persisted log always projects to a provider-valid conversation. Simplest: when draining, **drop any trailing `ToolCalled` that has no matching `ToolReturned`** (and the assistant message that introduced them, if that leaves it call-less), OR synthesize a `ToolReturned { content: "tool call did not complete (run cancelled/timed out)" }` for each unmatched call. Add a test: a run cancelled mid-tool, then a second `run` against the same session, asserting the second run's first model request is well-formed (no dangling tool call). This is the one that bites in the first real cancel-and-resume.

### H2. The load/retry contract is an unguarded footgun — and compounds H1

Load is `conversation = snapshot.messages ++ input.messages`, with the contract (Decision 1/2) that callers send **only the new turn** each run, and on retry send **empty** `input.messages`. Both are easy to violate and unenforced:

- **Same API, opposite behavior.** `agent.run(ctx, input)` with a session present concatenates `input` onto durable history. A caller migrating from sessionless usage (who passes the full running conversation in `input`) gets it **duplicated** against the snapshot — and since `finalize` then re-appends, the session grows the duplication every run. There's no idempotency/dedup.
- **Retry is inverted.** The natural retry (re-run with the *same* input) **re-appends the user message** (Decision 2 requires empty input to avoid it). So the obvious retry path double-records the turn.
- **Compounds H1.** Cancel a tool-using run → session = `[user, dangling tool_calls]`. Retry naturally (same input) → load = `[user, dangling tool_calls] ++ [user]` (duplicated user) **and** a conversation with dangling tool calls → malformed request. The natural recovery path is doubly broken.

**Correction.** Make the contract hard to get wrong, not just documented: (a) prominently document "with a `Session`, pass only the new turn; the session owns history" on `Runner::run`; (b) given H1's fix, the retry-with-same-input path should at least produce a valid (if duplicated) conversation; (c) consider a cheap guard — e.g. detect when `input.messages` re-sends messages already at the tail of the snapshot and skip/warn, or offer a `resume()`-style entry point distinct from `run(new_turn)` so the two intents aren't the same call. At minimum, pick a retry model that doesn't require the caller to remember to blank `input`.

---

## M — Medium

### M1. Agent-name attribution depends on `AgentUpdated` firing at every transition — confirm

The recorder resolves `SessionEvent::AssistantMessage.agent` (a required `String`) from `item.agent` (an `Option`) ∨ the recorder's tracked active agent, which it updates from `RunStarted`/`AgentUpdated`. So correct attribution after a **handoff** or into a **workflow sub-agent** depends on `AgentUpdated` actually being emitted at those transitions; if it isn't, the target's assistant messages are mis-attributed to the prior agent in the persisted log. The SMA-324 review verified the handoff driver does emit `HandoffItem` + `AgentUpdated`, and SMA-325 workflow agents emit `AgentUpdated` before each child — but a second code scan for this review was ambiguous on whether `AgentUpdated` reaches the stream, so the two passes disagreed.

**Correction.** Before relying on it, confirm `AgentUpdated` is emitted for **all** agent transitions the recorder must track (top-level via `RunStarted`, handoff, and each workflow sub-agent), and add a multi-agent recording test asserting the target's messages are logged with the **target's** name. If any transition path doesn't emit `AgentUpdated`, the attribution silently regresses to the parent.

### M2. Read hard-fails while write best-effort — asymmetric failure policy

`snapshot()` read failure is a **hard error** (the run fails before the agent starts), while `finalize()` write failure is **swallowed** (`tracing::warn!`). For an "ephemeral, best-effort persistence" philosophy that's asymmetric: a transient `SqliteSession` read hiccup (DB busy/locked) now fails an otherwise-fine run, whereas the same backend's write hiccup is tolerated. The spec's justification ("can't faithfully resume from an unreadable session") is defensible, but consider whether a read failure should also degrade gracefully (start from empty history + `warn`) or at least retry, so a transient backend blip doesn't take down the run.

**Correction.** State the rationale explicitly and decide deliberately; if hard-fail stays, scope it to *corruption* (unparseable events) vs *transient* errors (retryable), rather than failing the run on any `SessionError`.

### M3. `run_streamed` persistence requires the consumer to drain the stream

`finalize()` (and the recorder's full view of events) only happens if the `run_streamed` stream is driven to its terminal. If a consumer abandons the stream early (drops it mid-stream), the `async_stream` generator is dropped before reaching the `finalize` call, and the recorder has seen only the events pulled so far → **nothing (or a partial turn) is persisted**, with no warning. The SMA-321 "finalize on every exit" guarantee assumes the stream reaches a terminal/cancel, not consumer abandonment.

**Correction.** Document that `run_streamed` must be drained for persistence (and the run isn't "done" until it is), or detect drop and best-effort persist what's accumulated. At minimum call it out so users of the streaming API aren't surprised by missing sessions.

---

## L — Low

### L1. Unbounded snapshot growth until compaction (SMA-330)

Each run appends and the next run loads the **entire** history into the model request. Without compaction (correctly scoped out → SMA-330), long multi-turn sessions grow the context window unboundedly (token cost + latency climb every turn). Expected and documented as out of scope — just note the interaction so it's a conscious "compaction is required before long-lived sessions are practical," not a surprise.

---

## Verified OK (checked against source + planned design)

- **All session infrastructure exists exactly as the spec assumes** (SMA-318): `Session` (`append`/`events`/`snapshot`), `ConversationSnapshot { messages: Vec<Item> }`, `project()` (the canonical `SessionEvent`→`Item` map, with `HandoffOccurred` projecting to 0 messages and `Compacted`→`System`), `MemorySession`, `SqliteSession` (whose `snapshot()` can error — so the read-failure path is real).
- **`SessionEvent` shapes match the spec precisely** — `UserMessage`/`AssistantMessage`/`ToolCalled`/`ToolReturned`/`HandoffOccurred`(+`Compacted`), all with `ts: Timestamp`, enum-level `#[non_exhaustive]`, and `AssistantMessage.agent: String` (required) — with the `Item::AssistantMessage.agent: Option<String>` doc comment that explicitly anticipates the runner-resolves-it design the spec uses.
- **The loop emits the events the recorder maps** — `MessageOutput`, `ToolCallItem { item }`, `ToolOutputItem { item }` are all emitted carrying `Item::{AssistantMessage,ToolCall,ToolResult}` (the spec's table notation is loose but substantively correct). So tool turns *are* recordable (subject to H1's ordering caveat).
- **No double-counting** — `LlmAgent::run` seeds `[System(instructions)] ++ input.messages` only, never reads `ctx.session()`, and does **not** re-emit input messages as `MessageOutput`. So pre-seeding the recorder with `input.messages` + observing the stream captures each item once. Runner-owned architecture keeps `agent.rs` untouched — clean.
- **`finalize` runs on every exit** (confirmed): `run` calls it after `collect()`; `run_streamed` calls it *before* each terminal/synthetic-terminal yield (so an early-stopping consumer at the terminal still triggers it — see M3 for the *pre*-terminal drop case). The `.inspect()` recording layer between `controlled()` and `collect()`/the synthetic stream is the right seam and preserves the finalize-ordering invariants.
- **Projection-identity (AC#2) reasoning is sound** — re-applying `System(instructions)` fresh each run and never persisting it means `project(log)` == the model's conversation minus the System prefix, which is the correct, non-duplicating design.
- **Release handling is accurate and honest** — additive `SessionRecorder` in core (enums already `#[non_exhaustive]`) + behavior-only `runtime-tokio`; release-plz cascades both, no manual ascend (already-released crates, no stub). Notably the spec says core is a **"patch/minor"** bump rather than firmly mis-labeling it patch — a small but welcome contrast with the bump-classification slips in SMA-325/326/412.
- **Scope discipline** — compaction (SMA-330) and structured error (SMA-346) correctly deferred; the design leaves the `SessionRecorder` reusable by the SMA-332 durable runners.

---

## Required before writing the plan

1. **H1** — guarantee the persisted log always projects to a provider-valid conversation (drop or synthesize a result for any unmatched `ToolCalled` left by a mid-tool cancel/timeout); test cancel-then-resume.
2. **H2** — make the "new turn only / empty input on retry" contract hard to misuse (prominent docs; ideally a distinct resume entry point or a dup guard), so the natural retry path isn't broken — especially in combination with H1.

Recommended alongside: **M1** (confirm `AgentUpdated` fires for every transition the recorder tracks; test multi-agent attribution), **M2** (decide the read-hard-fail vs write-swallow asymmetry deliberately), **M3** (document `run_streamed` must be drained to persist).

## Sources

- Linear [SMA-392](https://linear.app/smaschek/issue/SMA-392) · [SMA-318](https://linear.app/smaschek/issue/SMA-318) (backends) · [SMA-321](https://linear.app/smaschek/issue/SMA-321) (finalize seam) · [SMA-330](https://linear.app/smaschek/issue/SMA-330) (compaction) · [SMA-346](https://linear.app/smaschek/issue/SMA-346) (structured error)
- Notion [Sessions](https://www.notion.so/355830e8fbaa81d79e15d62ac40954e8) · ADR *Session is an append-only event log, not a message list*
- Repo: `crates/paigasus-helikon-core/src/{session.rs, agent.rs, loop_state.rs, item.rs}`, `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, `crates/paigasus-helikon-sessions-sqlite/`
- Related reviews: SMA-321 (finalize-on-every-exit), SMA-324 (handoff transcript dangling-tool-call wire-format; `AgentUpdated`), SMA-325 (`SessionState`/workflow `AgentUpdated`), SMA-346 (collect/error boundary)
