# SMA-325 Design Review — Workflow Agents: `SequentialAgent`, `ParallelAgent`, `LoopAgent`

**Reviews:** [`2026-06-04-sma-325-workflow-agents-design.md`](./2026-06-04-sma-325-workflow-agents-design.md)
**Reviewer perspective:** staff engineering — fitness against the planned design, correctness of the orchestration semantics, and downstream blast radius
**Date:** 2026-06-04
**Verdict:** **Approve with changes.** Best-grounded spec yet on the dependency front: I confirmed SMA-324 landed with the exact *generalized* `agent_depth` machinery this spec assumes (which also closed my SMA-324 M1), and the load-bearing `Instructions::render(&RunContext<Ctx>)` signature is real, so the state-threading design is feasible. The changes are about the orchestration semantics, not the plumbing: the headline value of `SequentialAgent` (pass step A's output to step B) **isn't actually delivered** for plain `LlmAgent` steps because only `ParallelAgent` writes outputs to state (**H1**); the release is mis-classified as a patch (**H2**); and `ParallelAgent`'s run-level `final_output` is nondeterministic (**M1**).

## What this was checked against

- **Linear** [SMA-325](https://linear.app/smaschek/issue/SMA-325) (scope + AC) and **Notion** [Multi-Agent Patterns](https://www.notion.so/355830e8fbaa81ff9b43d6e466d5fc14) (planned design).
- **Code (ground truth, post-SMA-324, core `0.4.0`)** — `core/src/{agent.rs, context.rs, tool.rs, runner.rs, loop_state.rs, handoff.rs, agent_as_tool.rs}`, root `Cargo.toml`. Every dependency the spec leans on was verified.

Severity legend: **H** = high · **M** = medium · **L** = low · **N** = nit. Each item ends with a concrete **Correction**.

---

## H — High-severity

### H1. `SequentialAgent` never writes step outputs to state — so A→B threading doesn't work for plain `LlmAgent` steps

This is the core of what `SequentialAgent` is *for*, and as specced it's missing. The design makes a deliberate choice (§2, §3.3): threading is **via `SessionState`, not message rewriting** — "each sub-agent receives the parent's **original** `AgentInput`," and downstream agents read upstream results from shared `state` in a dynamic `Instructions` closure. That means a downstream step sees **only** the original input + whatever is in `state`; it does **not** see prior steps' messages or outputs.

But the write half is asymmetric:
- `ParallelAgent` **auto-writes** each branch's final-output text to `state[key]` (§4.2).
- `SequentialAgent` does **not** — §4.1 only says each sub-agent "sees whatever prior sub-agents wrote," with no mechanism for an `LlmAgent` step to write. And `output_key` (the natural auto-write) is explicitly **out of scope** (§10).

Consequence: `Sequential([summarize, critique])` of two plain `LlmAgent`s threads **nothing** — `summarize` writes nothing to `state`, so `critique`'s `Instructions` closure reads nothing. The headline capability is absent for the common case. AC#1 (`Sequential([Parallel([fetchA, fetchB]), summarize])`) passes only because the *Parallel* sub-stage does the writing and `summarize` reads those keys — there is no plain `LlmAgent`→`LlmAgent` threading anywhere in the AC, so the AC can pass while the feature's main value is undelivered.

**Correction.** Make `SequentialAgent` (and `LoopAgent`) **auto-write each step's final-output text to `state[step_name]`**, symmetric with `ParallelAgent` (§4.2 already has the exact "last `MessageOutput` text" extraction). Then `Sequential([A, B])` threads out of the box: `B`'s `Instructions` closure reads `state["A"]`. If you'd rather not auto-write, pull a minimal `output_key` into scope — but shipping `SequentialAgent` where the *only* way to thread is for steps to write `state` by hand contradicts the ticket's "threading shared run state." (Rendering timing is fine: `Instructions::render` runs at each step's own run-start — verified `agent.rs:594` — which is after prior steps completed, so the read sees their writes.)

### H2. Release is mis-classified as a patch — additive new public API is a `feat` → minor bump

§9 says "additive surface ⇒ **patch** bump, `core 0.4.0 → 0.4.1`," but also specifies the PR title `feat(core): SMA-325 …`. Those contradict: release-plz classifies a `feat` commit as a **minor** bump, so it will propose `core 0.4.0 → 0.5.0`, not `0.4.1`. New public API (`SequentialAgent`, `ParallelAgent`, `LoopAgent`, `SessionState`, `EventActions`, `ActionsHandle`, `AgentError::MaxIterationsExceeded`, the new `RunContext`/`ToolContext` methods) is backward-compatible *functionality added* — SemVer minor, Conventional-Commits `feat`. (I confirmed the change is genuinely additive: `RunContext`/`ToolContext` gain private fields with unchanged `new` arity; the enum is `#[non_exhaustive]`.) This is the same bump-classification slip flagged in the SMA-402 review — and SMA-324 got it right (`feat(core)!:` → minor for its breaking change).

**Correction.** State the bump as **minor: `core 0.4.0 → 0.5.0`**, consistent with the `feat(core):` title, and let release-plz cascade the facade pin. Don't describe a `feat` as a patch. (Base `0.4.0` is correct — verified — and SMA-324 has landed, so the dependency is satisfied; just confirm the live version at implementation time as SMA-324 did.)

---

## M — Medium

### M1. `ParallelAgent`'s run-level `final_output` is nondeterministic

The merge convention forwards every child's `MessageOutput` (§4). For `ParallelAgent`, both branches' assistant messages interleave live (`select_all`), so `RunResultStreaming::collect`'s "last `MessageOutput` wins" rule sets `RunResult.final_output` to **whichever branch happened to emit its last message last** — nondeterministic across runs. The meaningful per-branch results live in `state[key]`, but a caller reading `result.final_output` (or `collect_typed`) on a parallel run gets an arbitrary branch's text. The spec normalizes *event order* in tests but never says what `final_output` is for a parallel block.

**Correction.** Define it. Cleanest options: (a) `ParallelAgent` does **not** forward child `MessageOutput`s (final_output is empty; callers read `state`), or (b) it emits a single **synthesized** terminal `MessageOutput` (e.g. a JSON object of `{key: branch_output}`) so `final_output` is deterministic and useful. Document that parallel results are addressed by key, not by `final_output`.

### M2. `ParallelAgent` deviates from the planned `tokio::spawn` design — justified, but reconcile and bound the limitation

Both the **Notion Multi-Agent Patterns** page and the **Linear ticket** say `ParallelAgent` "`tokio::spawn`s sub-agents concurrently." The spec instead uses cooperative `futures::stream::select_all` on a single task because `core` is tokio-runtime-free (§4.2). The reasoning is sound and documented, but two things follow: (a) it's a real planned-design deviation that should be reconciled in Notion/ticket; (b) cooperative single-task concurrency means a **CPU-bound** branch starves its siblings (progress only at `.await` points) — fine for IO-bound `model.invoke`, not "true parallelism."

**Correction.** Reconcile the Notion/ticket wording to "cooperative concurrency (IO-bound), not OS-thread parallelism," and note that a genuinely-parallel `ParallelAgent` (via `tokio::spawn`) could live in `runtime-tokio` as a follow-up if a CPU-bound use case appears. Keep the honest caveat on the type doc.

### M3. The escalate/failure reads need clone-before-move — the §4.3 pseudocode as written won't compile

`Agent::run(self, ctx: RunContext<Ctx>, …)` takes `ctx` **by value** (verified). The §4.3 loop does `let child = ctx.subagent_child(); … drive child stream …; if child.actions().is_escalated()` — but `child` is **moved** into `agent.run(child, input)`, so reading `child.actions()` (and the child's `FailureSlot` for structured-error propagation) afterward is a use-after-move. The mechanism works only by cloning the `ActionsHandle` and the failure handle **before** the move (they're `Arc`-backed, share the slot) — exactly the clone-before-move pattern `TokioRunner` uses for `cancel`/`session`. The spec elides it.

**Correction.** Make the pattern explicit in §4.3/§4: `let actions = child.actions().clone(); let failure = child.failure_handle(); let sub = agent.run(child, input).await?; … ; if actions.is_escalated() { … }; if let Some(e) = failure.take() { parent_failure.set(e); }`. Otherwise the plan re-derives it under compiler pressure.

---

## L — Low

### L1. Parallel branch attribution is ambiguous in the flat event stream

The spec sells `ParallelAgent`'s live interleave as "lower latency / live multi-agent UX" (§2), but with no new `AgentEvent` variants and only `AgentUpdated { agent }` before each branch, a consumer **cannot attribute** an interleaved `TokenDelta`/`MessageOutput` to a specific branch once two branches are running concurrently — there's no per-branch correlation id in the event. OTel spans (SMA-322) nest correctly, but the `AgentEvent` stream is flat. So the "live multi-agent UX" benefit is partly undercut.

**Correction.** Acknowledge the limitation (per-branch attribution requires the OTel spans, not the event stream), or note a future correlation-id/`branch` field on the relevant events as the way to make live parallel UIs unambiguous. Not blocking.

### L2. Merge-convention vs collect-all presentation conflict

§4's merge convention lists "on a child `RunFailed`: set `FailureSlot` and **stop (fail-fast)**" for all three agents, but §4.2 then overrides `ParallelAgent` to **collect-all** (siblings finish; one aggregate `RunFailed`). Minor, but state it once cleanly, and specify that child `RunFailed` events are **swallowed** under collect-all so only the single aggregate surfaces (otherwise multiple `RunFailed`s reach `collect`, whose first-wins early-return would mask later branches).

### L3. `escalate` is iteration-level, not turn-level (already documented)

Per §4.3/§10, `LoopAgent` observes `escalate` only **after** a sub-agent's run fully drains — so an escalating tool doesn't stop the current sub-agent early; it ends the loop after the current iteration. That's the intended ADK-ish semantics and is documented; just make sure the example/docs set that expectation (escalate ⇒ "no more iterations," not "stop now").

---

## Verified OK (checked against source — strong foundation; resolves a prior finding)

- **SMA-324 landed with generalized nesting names** — `RunContext.agent_depth`, `RunConfig.max_agent_depth` (default 8), `AgentError::MaxAgentDepthExceeded { depth, max }`; **no** handoff-specific `handoff_depth`/`HandoffDepthExceeded`. So SMA-325's `max_agent_depth`/`MaxAgentDepthExceeded` references are correct, and `AgentAsTool` now stamps `with_agent_depth(depth+1)` — **which closes the SMA-324 M1 unbounded-`AgentAsTool`-recursion finding** (the depth bound is unified across handoff + agent-as-tool, and now workflow). Good.
- **`Instructions::render(&self, ctx: &RunContext<Ctx>) -> String`** exists with a blanket `Fn(&RunContext<Ctx>) -> String` impl, rendered at `agent.rs:594` with the full context — so the "downstream agent reads upstream `state` in a dynamic `Instructions` closure" mechanism is real (once `RunContext::state()` is added). This was the load-bearing assumption.
- **The handoff merge convention exists** (`NextAction::Handoff` arm, `agent.rs`): swallows child `RunStarted`, rewrites `RunCompleted { usage }` to the chain sum, forwards other events — exactly the convention §4 mirrors, and the usage-summation approach composes across stages.
- **`FailureSlot` is the right precedent** for `SessionState`/`ActionsHandle`: `Arc<Mutex<…>>`, `Clone` (shares slot), `set`/`take`, poison-recovery `unwrap_or_else(|e| e.into_inner())` — and `collect()` reads it post-drain, so fail-fast structured-error propagation works.
- **`Agent::run` takes `ctx` by value** (confirms M3's clone-before-move), `futures-util` is a `core` dep with `stream::select_all`, `RunConfig`/`AgentError` are `#[non_exhaustive]` (so the additions are additive), and `state.rs`/`workflow.rs`/`MockAgent` don't exist yet (this ticket creates them).
- **No new `AgentEvent` variants** ⇒ zero serde-fixture churn — correct (with the L1 attribution caveat).
- **Domain-neutral** — the workflow examples are generic (`fetchA`/`summarize`/`drafter`/`critic`), so no leukemia/finance re-domain is needed here. Good.

---

## Required before writing the plan

1. **H1** — deliver `SequentialAgent`/`LoopAgent` state threading: auto-write each step's final-output to `state[step_name]` (symmetric with `ParallelAgent`), or pull in a minimal `output_key`. Otherwise `Sequential([A, B])` of plain agents threads nothing.
2. **H2** — classify the release as a **minor** bump (`0.4.0 → 0.5.0`) to match the `feat(core):` title; don't call additive new API a patch.

Recommended alongside: **M1** (define `ParallelAgent.final_output` deterministically), **M3** (make clone-before-move explicit), **M2** (reconcile the cooperative-vs-`tokio::spawn` deviation in Notion/ticket). L-items are polish.

## Sources

- Linear [SMA-325](https://linear.app/smaschek/issue/SMA-325) · Notion [Multi-Agent Patterns](https://www.notion.so/355830e8fbaa81ff9b43d6e466d5fc14)
- Repo: `crates/paigasus-helikon-core/src/{agent.rs, context.rs, tool.rs, runner.rs, loop_state.rs, handoff.rs, agent_as_tool.rs}`, root `Cargo.toml`
- Related reviews on this branch: SMA-324 (handoff/`AgentAsTool`, the `agent_depth` generalization that resolved M1), SMA-402 (bump classification), SMA-322 (OTel spans / branch attribution)
