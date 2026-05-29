# SMA-321 Design Review — TokioRunner: cancellation, timeouts, parallel tool calls

**Reviews:** [`2026-05-29-tokiorunner-design.md`](./2026-05-29-tokiorunner-design.md)
**Reviewer perspective:** staff engineering — fitness against the planned design and downstream blast radius
**Date:** 2026-05-29
**Verdict:** **Approve with changes.** Unlike the SMA-320 spec, this one is *faithful to the as-built code* — I verified every load-bearing claim and they hold (object-safe `Runner`, agent-owned driver, `join_all` fan-out, tokio-free core). The headline risks are not "spec vs code" but (a) the **planned design in Notion/Linear is now stale** and contradicts reality, (b) the **driver extraction is premature and mis-justified**, and (c) two seams (`finalize`, `retry_policy`) ship asymmetric or inert. Resolve **D1 scope, H1, and H2** before the plan; reconcile the Notion/Linear items so "the plan" stops being self-contradictory.

## What this was checked against

- **Linear** [SMA-321](https://linear.app/smaschek/issue/SMA-321) (scope + AC) and [SMA-346](https://linear.app/smaschek/issue/SMA-346) (the structured-error follow-up it blocks).
- **Notion** [ADR-6 "Library + pluggable Runner trait"](https://www.notion.so/355830e8fbaa81a483dafbf8a9cfcc15), [Core Primitives](https://www.notion.so/355830e8fbaa81c6accccb3e6997da25), [Agent Loop & State Machine](https://www.notion.so/355830e8fbaa81d4af16dfa4ed57424a), [tokio as the default async runtime](https://www.notion.so/355830e8fbaa811fa6a8d6f9ca5e6c8e).
- **Code (ground truth)** — `crates/paigasus-helikon-core/src/{runner.rs, agent.rs, context.rs, model.rs}`, `crates/paigasus-helikon-runtime-tokio/{src/lib.rs, Cargo.toml}`, root `Cargo.toml`, `release-plz.toml`. Every claim below was verified against current source.

Severity legend: **C** = contradicts the planned design / blocking · **H** = high, will bite · **M** = medium · **N** = minor / nit. Each item ends with a concrete **Correction**.

---

## C — Planned-design discrepancies (Notion / Linear are stale)

### C1. The "planned design" the spec references is internally contradictory with the code — reconcile it

This is the most important finding for "review against the planned design," because the planned design **no longer matches what was built**, and the spec quietly takes the code's side without saying the plan is wrong.

- **Runner trait shape.** Notion ADR-6 *and* Core Primitives both specify a **generic** runner: `async fn run<A: Agent<Ctx>>(&self, agent: &A, …)`. The actual `core/src/runner.rs` is **object-safe**: `async fn run(&self, agent: &(dyn Agent<Ctx> + '_), …)` — with rustdoc that explicitly says *"object-safe: both methods accept `&dyn Agent<Ctx>` rather than a generic `<A: Agent<Ctx>>` parameter."* The spec's §2 ("object-safe by design, takes `&dyn Agent<Ctx>`") is **correct about the code** and **contradicts Notion**.
- **Who owns the driver.** SMA-321's own scope bullet says *"`TokioRunner` … **Owns the `LoopState` driver**; emits `AgentEvent`s,"* and the Notion Agent Loop page says *"**The runner drives `LoopState`** forward, emitting `AgentEvent`s at every transition."* The code (and this spec) do the **opposite**: `LlmAgent::run` owns the `async_stream` driver; the runner only consumes `agent.run()`'s stream. The spec's Consequence #1 is the correct reasoning *given the object-safe trait* — but it means the ticket's scope bullet and two Notion pages are now false.

The spec is on the right side of this (object-safety is the better call — it's what lets handoffs store `Arc<dyn Agent>` and lets callers hold `Box<dyn Runner>`). The problem is that the user asked to review *against the planned design*, and the planned design is stale and self-contradictory, so "conformance" is undefined.

**Correction.** Before implementation, land doc updates so the plan and the build agree:
1. Update **ADR-6** and **Core Primitives** to the object-safe `Runner` signature (or add a superseding ADR — "Runner is object-safe; the Agent owns the loop driver"). This is a genuine architecture decision that deserves its own ADR, not a silent change in a feature spec.
2. Fix the **SMA-321 scope bullet** — "`TokioRunner` owns the `LoopState` driver" is no longer true; it consumes the agent's event stream and adds boundary control. Update the Notion Agent Loop "the runner drives `LoopState`" sentence likewise.
3. Add a one-line note to the spec acknowledging it *supersedes* those pages, so a future reader doesn't treat the contradiction as a spec defect.

### C2. `RunConfig::cancellation` field dropped — sound, and correctly disclosed

SMA-321 scope lists `RunConfig { …, cancellation }`. The spec drops it in favor of the single canonical `RunContext::cancel()` token (§4.1), and discloses this as an intentional deviation. **This is the right call** (one source of truth for a live signal) and I'm noting it only so the Linear scope bullet gets trimmed to match. No change needed beyond updating the ticket text. (See the tension this creates with config-via-context in H3.)

---

## H — High-severity (will bite)

### H1. The driver extraction (D1 / §4.3) is premature and mis-justified — unbundle it

D1 locks "extract a shared driver into `core::driver` … so `LlmAgent::run` and **future durable runners** reuse it." Verified against the code, this justification does not hold:

- **No consumer needs it in SMA-321.** `LlmAgent::run` is the only caller, and `TokioRunner` reaches the driver *through* `agent.run()` (the spec says so in §1 and §5). Whether the `async_stream` body lives in `agent.rs` or a new `driver.rs` makes **zero functional difference** to `TokioRunner`. The cancel/timeout/concurrency features do not depend on the extraction.
- **Durable runners won't reuse `drive()`.** Per ADR-6, the Temporal runner *"wraps each `Tool::invoke` in a Temporal Activity; the loop becomes a Workflow."* That requires driving `transition()` **step-by-step with persistence between steps** — it cannot consume a monolithic `drive()` `BoxStream` that runs the whole loop internally with its own `join_all` fan-out. The real durability seam is **`transition`** (the pure state machine), which already exists and is untouched. So "future durable runners reuse `core::driver`" is the wrong seam; they reuse `transition`.
- **It bundles a ~280-line mechanical refactor with new behavior.** Moving `async_stream` + `build_items` + `run_tools_concurrent` + `tool_output_to_content_parts` + `ToolCallAccum` out of `agent.rs`, *plus* changing `LlmAgent::run` into a shim, *plus* adding bounded concurrency and config threading, all in one ticket, makes review and regression-bisection harder. The spec's own "regression: existing core loop tests stay green" line is an admission of the risk.

Note the functional changes that *do* require touching the driver — bounded concurrency (§4.4) and reading new `RunConfig` fields (§4.1) — can be done **in place** in `agent.rs` without relocating anything.

**Correction.** Pick one:
1. **Preferred — drop the extraction from SMA-321.** Make the functional changes in place. Defer `core::driver` to the ticket that introduces a *second real consumer* (and reframe it around `transition`, which is what a durable runner actually reuses).
2. **If you want the extraction for organization**, land it as a **separate, behavior-preserving refactor PR first** (pure move, no logic change, tests unchanged), then build SMA-321's features on top. Either way, correct D1's durability justification — it's `transition`, not `drive`.

### H2. `run` has no `finalize` seam; `run_streamed` does — the asymmetry will silently drop session persistence

§5.2 calls `finalize(&session).await` (no-op now) after the streamed run, framed as the seam where session persistence + compaction land (follow-up #1). But §5.1's `run` just accumulates events and returns `Ok(RunResult)` / `Err` — **no `finalize` seam at all**, on either the happy path or the cancel/timeout paths. When `finalize` becomes real:

- `run` (non-streamed) will **never persist the session** — a silent data-loss-shaped bug for every caller who uses the aggregate API.
- Cancel/timeout in `run` returns `Err` immediately and drops the stream, also skipping persistence, while `run_streamed` *does* finalize on cancel/timeout. Two code paths, two behaviors, baked in now and discovered later.

**Correction.** Put the `finalize` seam in **both** methods now, even as a no-op, and run it on all exits (normal, cancel, timeout). Add a test asserting `finalize` is invoked on each path (a counting `NoopSession`), so the follow-up that fills it in inherits the guarantee instead of having to re-plumb `run`.

### H3. `RunConfig` field enforcement is fragmented across two layers — and a bare `agent.run()` silently ignores `timeout`

Because core is hard-constrained to be tokio-runtime-free (verified: `core/Cargo.toml` has no `tokio` runtime dep; the constraint forbids `tokio::time`), the new `RunConfig` fields land in **different enforcement layers**:

- `max_turns` — enforced in the core driver (today).
- `parallel_tool_call_limit` — enforced in the core driver (§4.4, `futures_util::buffered`).
- `timeout` — **only** enforceable in `TokioRunner` (§5), since core cannot sleep.
- `retry_policy` — enforced **nowhere** yet (D5).

Consequence: a user who sets `RunConfig::timeout` and calls `agent.run()` **directly** (the documented MVP path from the SMA-320 work: `agent.run().collect()`) gets **no timeout** — it's silently ignored — while `parallel_tool_call_limit` *is* honored on that same path. Same struct, different fields silently active depending on how you invoke. That's a comprehension footgun on a public type.

This also re-introduces the exact "two sources of truth" pattern the spec rejected for `cancellation`: config now flows from **`ctx.run_config()`** (runner-injected) *or* **`self.config`** (agent field), reconciled by precedence (§4.2). The reasoning that killed `RunConfig::cancellation` ("two sources … force reconciliation") applies here too; the difference (config is inert data, a cancel token is a live signal) is defensible but should be stated, not glossed.

**Correction.** Document, on `RunConfig` itself, which fields are honored by the core driver vs. only by a runtime backend (`timeout`, and later `retry_policy`, are runner-scoped). Consider making the split explicit in the type — e.g. driver-scoped fields vs. runner-scoped fields, or a doc table — so "I set `timeout` and nothing happened" is impossible to hit blind. Explicitly acknowledge the config dual-source and why it's acceptable where the cancellation dual-source wasn't.

### H4. Inert public `retry_policy` on a 0.1.0 surface (D5) — don't ship config that does nothing

`runtime-tokio` ascends to `0.1.0` (published) in this ticket, and `RunConfig` is a public, `#[non_exhaustive]` type. D5 lands `retry_policy: RetryPolicy` (and `RetryPolicy::initial_backoff`) **with the mechanism deferred** — i.e. a caller sets `with_retry_policy(…)`, the build accepts it, and **nothing retries**. That's the same "dishonest API" failure mode flagged in the SMA-320 review, now on the runtime surface. `#[non_exhaustive]` means adding the field *later* (when the mechanism lands) is fully backward-compatible, so there is no compatibility reason to ship it inert.

**Correction.** Either (1) implement the minimal `RetryingModel<M>` decorator now — it lives in `runtime-tokio`, keeps backoff timers out of core, and is small — so the field is honest; or (2) **omit `retry_policy` from `RunConfig` until that mechanism lands**, and add it in the same release. Do not ship an inert public knob. (D5 marks itself "confirm at review" — this is the confirmation: don't.)

---

## M — Medium

### M1. `tokio::select!` is unbiased — a run that finishes as cancel/timeout fires can be misreported

In both §5.1 (`run`) and §5.2 (`run_streamed`) the control loop is `select! { stream.next() … , cancel.cancelled() … , sleep …}`. `tokio::select!` polls branches in **random** order by default. If the stream has `RunCompleted` ready in the same poll that `cancel` (or the deadline) fires, the macro may pick the cancel/timeout branch and report `Cancelled`/`Timeout` for a run that actually completed. That's both a semantic wart (a finished run reported as cancelled) and a source of **flaky tests** for AC#1.

**Correction.** Use `biased;` with the stream branch first (drain ready events before honoring cancel/timeout), or on cancel/timeout do a final non-blocking poll of the stream for a terminal event before deciding. Add a test for "cancel fires in the same poll as `RunCompleted`."

### M2. `TokioRunner::run` duplicates `RunResultStreaming::collect`'s accumulation — share it

§5.1 says it accumulates events / `final_output` / `usage` *"exactly as `RunResultStreaming::collect` does."* I confirmed `collect()` already contains that logic (the `AssistantMessage` text-concatenation into `final_output`, `RunCompleted`→usage, `RunFailed`→`RunError::Other`). Re-implementing it inside `run` invites drift — e.g. the structured-output text-extraction rule changing in one place but not the other.

**Correction.** Have `run` build the same `select!`-wrapped stream that `run_streamed` produces and then call the existing `.collect()` on it (or factor the accumulation into one shared helper). One definition of "how a stream becomes a `RunResult`."

### M3. `RunContext` gains execution policy — a layering smudge

`RunContext` today carries ambient run state: `user_ctx`, `session`, `hooks`, `tracer`, `cancel` (verified). §4.2 adds `run_config: Option<RunConfig>` — i.e. execution policy (timeout, concurrency, retry) — into the object that's also narrowed into `ToolContext` via `to_tool_context()` and passed to guardrails. Execution policy is the Runner's concern; putting it in the ambient context is the only available channel (the `Agent::run` signature is `(ctx, input)`), but it muddies `RunContext`'s role.

**Correction.** Acceptable for MVP given the trait constraint, but (a) ensure `run_config` is **not** surfaced into `ToolContext` (tools have no business reading the run's timeout/retry policy), and (b) add a doc comment on the field explaining it's the runner-injection channel, not general context state.

### M4. Typed `RunResult<T>` still isn't delivered end-to-end (cross-ref SMA-320)

`TokioRunner::run` returns `RunResult` (= `RunResult<String>`); `collect()` returns `RunResult<String>`. So the Runner boundary still hands back `String`, and structured-output callers still go through `parse_final::<T>()`. That's consistent with both SMA-320 and SMA-346 deferring the typed surface — but it means the "honest typed output" promised by the Notion examples remains undelivered *through the runner too*. Worth one explicit sentence so it isn't assumed delivered here. (See the SMA-320 review, C1.)

---

## N — Minor / nits

### N1. Spec references a `noop_run_context` helper that doesn't exist

§4.2 claims the change "keeps the existing test helper (`noop_run_context`) … compiling untouched." A workspace search finds **no `noop_run_context`** anywhere, and `RunContext::new` takes five non-trivial args (`user_ctx, session: Arc<dyn Session>, hooks: HookRegistry, tracer: TracerHandle, cancel`). §6 separately (and correctly) lists `noop_run_context` among the mocks `runtime-tokio` must *create*. The §4.2 parenthetical is the slip — there's no existing helper to preserve. The substantive point (`RunContext::new` signature is unchanged, so call sites compile) is valid.

**Correction.** Drop the "existing `noop_run_context`" reference in §4.2; keep the §6 plan to build one in `runtime-tokio/tests/common`.

### N2. `drive()` is a 10-parameter free function

§4.3's `drive(model, tools, tool_defs, model_settings, output_type, agent_name, instructions_text, config, ctx, input)` is a 10-arg signature — a maintainability smell and an easy place to transpose arguments.

**Correction.** If the extraction survives H1, pass a `struct DriveParams<Ctx> { … }` (or keep the body in `agent.rs` and capture `self`'s fields directly, which is cleaner still).

### N3. Crate-ascend recipe — actually *more* complete than CLAUDE.md; confirm one point

§7 adds a 4th step CLAUDE.md's recipe omits — bump the `[workspace.dependencies]` entry for the crate to `0.1.0`. If that entry pins `version = "0.0.0"` today, this step is **required** (cargo would otherwise reject the path crate at `0.1.0` against a `^0.0.0` requirement), and CLAUDE.md's generic 4-step is incomplete. Good catch by the author. One thing to preserve: CLAUDE.md is emphatic that release-infra commits use `chore(...)`/`docs(...)`, never `feat`/`fix` — the spec's header says `chore(release): …`, so it's covered; just keep it as an explicit step so it isn't lost in execution.

### N4. Cancellation "within one poll" assumes cooperative tools

D4/§5 are sound (verified: `run_tools_concurrent` uses `futures_util::future::join_all`, not `tokio::spawn`, so dropping the stream drops the in-flight tool futures; the cancel token also flows to `model.invoke` and tool child tokens). The "aborts within one polling boundary" guarantee holds **for cooperative futures** — a tool doing blocking, non-`await` CPU work won't yield until it hits an await point. AC#1's barrier-mock test exercises the cooperative case, which is the right thing to claim.

**Correction.** State the cooperative-cancellation caveat in the spec so it isn't read as a hard preemption guarantee.

---

## Verified OK (checked, no action needed)

- **`Runner` is object-safe** (`agent: &(dyn Agent<Ctx> + '_)`), so the spec's "driver lives in the agent, runner consumes the stream" architecture is correct against the code (the disagreement is with stale Notion — see C1).
- **`RunError` already has `Cancelled`, `Agent(AgentError)`, `Other(anyhow::Error)`** and is `#[non_exhaustive]`; only `Timeout` is genuinely new (§4.5). The TokioRunner's `RunError::Cancelled` / `RunError::Agent` returns are already supported.
- **Drop-based cancellation is sound**: `join_all` futures are polled within the stream, not spawned; `Model::invoke` takes a `CancellationToken`; no `tokio::spawn`/`tokio::time`/`Semaphore` in core. The boundary is clean.
- **`RunConfig`, `RunResult`, `RunResultStreaming`, `AgentInput`, `LlmAgent.config`** all exist as the spec assumes; `RunConfig` and `RunResult<T = String>` are `#[non_exhaustive]`/generic, so the additive changes are safe.
- **`futures_util::buffered`** is available (core depends on `futures-util`), and `buffered` (not `buffer_unordered`) is the correct choice for order-preserving bounded concurrency.
- **`runtime-tokio` is a clean `0.0.0` stub** with `publish = false` (Cargo.toml) and a `release = false` block (`release-plz.toml`), matching the ascend preconditions.

---

## Required before writing the plan

1. **C1** — reconcile ADR-6 / Core Primitives / Agent Loop / SMA-321 scope with the as-built object-safe `Runner` + agent-owned driver (ideally a superseding ADR). The "planned design" must stop contradicting the code.
2. **H1** — drop or unbundle the `core::driver` extraction; fix its durability justification (`transition`, not `drive`).
3. **H2** — add the `finalize` seam to `run`, not just `run_streamed`; run it on all exits.
4. **H4** — don't ship an inert `retry_policy`; implement minimally or omit until the mechanism lands.

Recommended alongside: **H3** (document/structure runner-scoped vs driver-scoped config), **M1** (`biased` select), **M2** (share `collect` accumulation). The rest is plan-level detail.
