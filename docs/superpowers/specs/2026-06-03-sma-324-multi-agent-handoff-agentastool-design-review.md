# SMA-324 Design Review — Multi-agent: Handoff + `AgentAsTool`

**Reviews:** [`2026-06-03-sma-324-multi-agent-handoff-agentastool-design.md`](./2026-06-03-sma-324-multi-agent-handoff-agentastool-design.md)
**Reviewer perspective:** staff engineering — fitness against the planned design, correctness of the handoff/tool semantics, and downstream blast radius
**Date:** 2026-06-03
**Verdict:** **Approve with changes.** This is the best-grounded spec in the series: I verified every seam it leans on and they all exist as described, and it visibly applies prior-review lessons (breaking change correctly classified as `feat(core)!:`→minor; usage-accumulates-across-the-chain resolves the SMA-402 "who pays" seam; failure propagation rides the now-post-drain `FailureSlot`). The changes are: (1) **re-domain off leukemia** — the spec still uses MRD/AML in its example + tests, contradicting the 2026-06-01 personal-finance pivot (**H1**); and (2) resolve how the flagship `output_type` + `handoffs` triage actually behaves on a constrained provider, which is the SMA-320 H1 conflict resurfacing on the canonical example (**H2**). Plus an `AgentAsTool` recursion bound and a slug-collision gap.

## What this was checked against

- **Linear** [SMA-324](https://linear.app/smaschek/issue/SMA-324) (scope + AC) and the SMA-402 §3.5 "who pays" seam it resolves.
- **Notion** [Multi-Agent Patterns](https://www.notion.so/355830e8fbaa81ff9b43d6e466d5fc14) (planned design) and the [Side-by-Side Comparison](https://www.notion.so/355830e8fbaa81ce86d7e8caadb96d47) (re-domained to finance on 2026-06-01).
- **Code (ground truth)** — `core/src/{agent.rs, agent_builder.rs, loop_state.rs, context.rs, tool.rs, item.rs, hook.rs, runner.rs, session.rs}`. Every claim below was verified against source.

Severity legend: **H** = high · **M** = medium · **L** = low · **N** = nit. Each item ends with a concrete **Correction**.

---

## H — High-severity

### H1. The spec is still in the leukemia domain — re-domain to personal finance to match the 2026-06-01 pivot

On 2026-06-01 the example domain was deliberately moved off hematopathology (too niche for a flagship) to a **personal-finance assistant**, and SMA-323 + the Notion Side-by-Side Comparison were re-domained accordingly. This spec is dated **2026-06-03** but still ships the old domain:

- The facade example is `examples/multi_agent_triage.rs` mirroring "the Notion snippet" with `.handoffs([Handoff::to(mrd), Handoff::to(aml)])` (§7).
- The integration tests (§7) are "triage + MRD + AML," with `transfer_to_mrd_*` routing and "AML must not run" assertions.
- The unit tests use `transfer_to_mrd`.

That contradicts the established direction and, worse, the Notion Side-by-Side it claims to reproduce now shows a **finance** triage (route to *budgeting* vs *investing* specialist). So the SMA-324 example wouldn't match the page it's meant to be the Rust column of.

Note two upstream docs are also stale and should be reconciled in the same pass: the **Linear SMA-324 AC** still says "parent + MRD specialist + AML cytogenetics," and the **Notion Multi-Agent Patterns** handoff snippet uses `mrd_agent`/`aml_agent` and the non-existent `openai::gpt_5_mini()` constructor (real API: `OpenAiModel::chat("gpt-5-mini").build()?`).

**Correction.** Re-domain SMA-324 to finance, consistent with SMA-323/Side-by-Side:
- Facade example → `multi_agent_triage.rs`: triage routes to a **budgeting specialist** vs an **investing specialist** (`transfer_to_budgeting_specialist` / `transfer_to_investing_specialist`).
- Integration tests → triage + budgeting + investing (investing "must not run"); keep the slug-collision + depth-guard cases.
- Fix the Notion Multi-Agent Patterns snippet (finance agents + real builder API) and update the Linear AC text.
(Happy to make these edits the same way I did SMA-323 + the Side-by-Side page — say the word.)

### H2. The flagship triage combines `output_type` + `handoffs` + a tool — exactly the SMA-320 H1 conflict; confirm it works on Anthropic, and define the post-handoff output type

The canonical triage (Notion + §3.3) is an `LlmAgent` with **all three** of: `output_type::<TriageDecision>()`, `handoffs` (→ injected `transfer_to_*` tools), and a regular tool. Two unresolved interactions:

1. **Constrained-provider tool blocking.** Per the SMA-320 review (H1), setting `output_type` makes the Anthropic provider force `tool_choice` to the synthesized structured-output tool, which **blocks all other tools** — including the `transfer_to_*` transfer tools. If SMA-320's H1 wasn't resolved (apply the output constraint only on a finalizing turn, not every turn), the triage **cannot call its transfer tools on Anthropic**, and this whole feature's flagship example silently can't hand off there. The SMA-324 tests are all `MockModel`-based and won't catch this; the facade example is compile-gated, not run.
2. **Post-handoff output type is dynamic but the caller's `collect_typed::<T>()` is static.** §3.3 correctly says that when triage hands off, "the run's terminal output is the **target's**," not a `TriageDecision`. So a caller doing `collect_typed::<TriageDecision>()` on a run that handed off to a free-text specialist gets a **deserialize error** — the run's output type depends on *which* agent terminated. The spec calls the behavior "correct and intended" (it matches OpenAI) but never addresses the typing consequence for the caller.

**Correction.**
- Cross-check SMA-320 H1: confirm the output constraint is applied only on the finalizing turn (so a triage with `output_type` can still call `transfer_to_*`/tools on Anthropic), and add at least one provider-path note. If SMA-320 H1 is unresolved, this is a hard dependency — flag it as blocking the triage example on Anthropic.
- Document the post-handoff output contract: with handoffs, the run's `final_output` is whichever agent terminates; `collect_typed::<T>()` is only sound if every reachable terminal agent produces `T`. Recommend the example use `collect()` (string) or show the parse-or-fallback, and add a doc note on `output_type` + `handoffs`.

---

## M — Medium

### M1. `AgentAsTool` has no recursion bound — asymmetric with the handoff depth guard the spec just added

The spec adds `max_handoff_depth` (default 8) + `HandoffDepthExceeded` precisely because "each nested `run` resets the per-agent turn counter, so `max_turns` alone cannot bound a cross-run cycle" (§3.6). But `AgentAsTool::invoke` (§3.5) builds a **fresh** `RunContext::new` for the sub-run, so `handoff_depth` **resets to 0** and the sub-run uses its *own* `RunConfig`. The exact argument that justified the handoff guard applies to `AgentAsTool` — a cyclic agent-as-tool graph (A wraps B as a tool, B wraps A) or deep nesting recurses on the call stack with **no depth bound**, each level getting a fresh budget. `max_turns` per level bounds calls-per-agent, not nesting depth.

**Correction.** Carry a shared nesting/depth counter into the `AgentAsTool` sub-context too (reuse `handoff_depth`, or a sibling `call_depth`), bounded by the same `max_*_depth`, so the bound spans *both* handoff and agent-as-tool nesting. At minimum, document the unbounded-recursion hole and that `AgentAsTool` sub-runs escape the parent's `RunConfig` (timeout, depth).

### M2. The slug-collision fail-fast only covers handoff-vs-handoff, not handoff-vs-real-tool

§3.1 fails fast when "two targets map to the same `tool_name`." But §3.3 sends `tools = [real tool defs ++ handoff tool defs]`, and the routing branch matches "any `Item::ToolCall` whose `name` matches a `HandoffDef.tool_name`." A real tool (or an `AgentAsTool`) named `transfer_to_x` would (a) collide with a handoff slug in the schema the model sees, and (b) get **mis-routed as a handoff** by the routing branch. The `transfer_to_` prefix makes accidental collisions unlikely, but an `AgentAsTool` named that way, or a user tool, is a real correctness hole.

**Correction.** Extend the fail-fast collision check to also reject any handoff `tool_name` that collides with a real tool/`AgentAsTool` name in the same agent. Cheap, and it closes the mis-route.

### M3. The handed-off transcript carries a `transfer_to_*` tool_use/result for a tool the target doesn't define — verify against real providers

§3.4 threads the parent conversation (which includes the `transfer_to_*` `Item::ToolCall`) plus a synthesized `ToolResult` ack, minus the leading `System`. But the **target's** tool schema does not include `transfer_to_*` (unless it also has handoffs). The spec addresses one wire-format hazard ("OpenAI rejects a tool call with no matching result" → synthesized ack) but not the other: an assistant `tool_use` + `tool_result` pair referencing a tool absent from the current request's `tools`. Providers vary on accepting historical tool calls for undefined tools, and all SMA-324 tests are `MockModel`-based (won't reject wire format); the facade example isn't run in CI.

**Correction.** Verify a handed-off transcript (transfer `tool_use` + synthesized `tool_result`) is accepted by both OpenAI and Anthropic during implementation. If either rejects it, rewrite the transfer pair before threading — e.g. replace the `transfer_to_*` `ToolCall`/`ToolResult` with a plain assistant/user note ("Transferred from <parent>.") — rather than passing a dangling reference to an unknown tool.

---

## L — Low

### L1. `AgentAsTool` re-implements `collect`'s drain/accumulation

§3.5 step 3 drains the sub-stream "exactly as `RunResultStreaming::collect` does" — duplicating logic that already lives in `collect()` (the same DRY concern raised in the SMA-321 review). Prefer `RunResultStreaming::new(sub_stream).collect().await`, mapping `Err(RunError::Agent(e)) → ToolError::Other(e.into())` and reading `final_output`. One definition of "stream → result," and it inherits the post-drain `FailureSlot` read for free.

### L2. Parallel tool + handoff in one turn silently drops the tool calls

§3.3 step 1: "the first matching handoff call wins; other tool calls that turn are ignored." Documented, and benign because handoff is terminal for the parent (the dropped calls vanish with the parent run). Just note it as real behavior — a model that emits a `fetch_account_summary` call *and* a transfer in the same turn loses the fetch.

---

## Verified OK (checked against source — strong foundation, prior lessons applied)

- **Every seam exists and matches.** `LlmAgent.handoffs: Vec<Arc<dyn Agent>>` ("stored but not driven"); `LoopState::ApplyingHandoff { target, transcript }` (no `usage` yet; enum `#[non_exhaustive]`, variant **not** — so adding `usage` is breaking, correctly classified); `AgentEvent::AgentUpdated { agent: String }` (the spec emits `agent`, **matches** — not `new_agent`); `HandoffItem { from, to }`; `HookEvent::OnHandoff { from, to }`; `NextAction` `#[non_exhaustive]` with no `Handoff` yet; `ToolError::{InvalidArgs { schema_errors }, Other}`; `ToolContext` exposes `user_ctx()`/`tracer()`/`cancel()` (so `AgentAsTool` can rebuild a `RunContext`); `TransitionCtx<'a>` ready for the `handoffs` field.
- **`Item::System` is prepended to `conversation`** (verified `agent.rs`), so §3.4's "strip the parent's leading `System`" is both valid and necessary (avoids a double system prompt). This was the load-bearing assumption — confirmed.
- **`FailureSlot` landed and `collect()` reads it POST-DRAIN** (`if let Some(err_msg) = failed { … failure.take() … }` after the drain loop) — i.e. the SMA-346 H1 ordering bug I flagged was fixed in implementation. Consequently the shared-slot handoff failure propagation (§3.4) is sound: a failing target sets the shared slot, the parent forwards `RunFailed`, drains, then reads the slot → `RunError::Agent(target_error)`. No new plumbing needed, as the spec says.
- **Usage accumulates across the chain (D4) is correct and composes.** `ApplyingHandoff{usage: prior+turn}` + rewriting the forwarded `RunCompleted.usage` to `parent_usage + sub` sums each turn exactly once across arbitrarily deep chains (A→B→C), resolving the SMA-402 §3.5 "who pays" seam (my SMA-402 L1) the right way.
- **`NextAction::Handoff` reads the payload from `ApplyingHandoff` state**, mirroring the existing `Terminate`-reads-`Failed` pattern — consistent, and the transcript is built in pure `transition` (replayable).
- **Breaking-change handling is correct.** `feat(core)!:` → minor `0.3.0→0.4.0` via release-plz with the `dependencies_update` cascade; no manual ritual; "verify the actual current core version first." This applies the SMA-402 review lesson (classify a public-enum field addition deliberately) rather than shipping a breaking change as a patch.
- **`handoff.rs` / `agent_as_tool.rs` don't exist yet** (this ticket creates them); `MemorySession::new`, `AgentInput::from_user_text`, `tool_output_to_content_parts` all exist as used.
- **Deferrals are well-judged:** `OnHandoff` hook not fired (consistent with all hooks being stored-but-not-driven), no per-edge `Handoff` overrides, no Sequential/Parallel/Loop/Swarm/Graph agents — and the nested-delegation design is built so those slot in later.

---

## Required before writing the plan

1. **H1** — re-domain the example + tests off leukemia (MRD/AML) to personal finance (budgeting/investing specialists), and reconcile the stale Linear AC + Notion Multi-Agent Patterns snippet (finance + real builder API). Matches the 2026-06-01 pivot.
2. **H2** — confirm `output_type` + `handoffs` + tools coexist on Anthropic (the SMA-320 H1 dependency: constraint only on the finalizing turn), and document the dynamic post-handoff output type vs `collect_typed::<T>()`.

Recommended alongside: **M1** (bound `AgentAsTool` recursion like handoff depth), **M2** (slug-vs-real-tool collision check), **M3** (verify the handed-off transcript's transfer tool_use/result against real providers). L1/L2 are polish.

## Sources

- Linear [SMA-324](https://linear.app/smaschek/issue/SMA-324) · [SMA-402](https://linear.app/smaschek/issue/SMA-402) (the resolved usage seam)
- Notion [Multi-Agent Patterns](https://www.notion.so/355830e8fbaa81ff9b43d6e466d5fc14) · [Side-by-Side Comparison](https://www.notion.so/355830e8fbaa81ce86d7e8caadb96d47)
- Repo: `crates/paigasus-helikon-core/src/{agent.rs, loop_state.rs, context.rs, tool.rs, item.rs, runner.rs}`
- Related reviews on this branch: SMA-320 (H1 tools+output_type), SMA-346 (FailureSlot ordering), SMA-402 (usage / breaking-change classification)
