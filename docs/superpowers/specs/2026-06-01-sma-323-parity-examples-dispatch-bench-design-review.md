# SMA-323 Design Review — Side-by-side parity examples + dispatch benchmark

**Reviews:** [`2026-06-01-sma-323-parity-examples-dispatch-bench-design.md`](./2026-06-01-sma-323-parity-examples-dispatch-bench-design.md)
**Reviewer perspective:** staff engineering — fitness against the planned design, honesty of the "parity / MVP-exit" claim, and benchmark methodology
**Date:** 2026-06-01
**Verdict:** **Approve with changes.** I verified every public-API call the examples make against current source — they all exist and the examples will compile cleanly (including a public `MemorySession` for the `RunContext`), so the "consumes only the public API, no core changes" premise genuinely holds. The two things to address before the plan are not about compilation: (1) the spec silently diverges from the **planned canonical artifact** — the Notion "Side-by-Side Comparison" is a *handoff* example using several not-yet-built APIs, so the "side-by-side parity / MVP exit" claim is much weaker than the ticket implies and the Notion page needs reconciling (**H1**); and (2) the benchmark measures `block_on`, not dispatch, so it doesn't prove what the ticket asks (**H2**).

## What this was checked against

- **Linear** [SMA-323](https://linear.app/smaschek/issue/SMA-323) (scope + AC, references the Notion page below) and [SMA-324](https://linear.app/smaschek/issue/SMA-324) (handoff/agent-as-tool, Stage 2).
- **Notion** [Side-by-Side Comparison](https://www.notion.so/355830e8fbaa81ce86d7e8caadb96d47) — the planned canonical example this ticket is meant to produce the Rust column of — and [Roadmap → Stage 1](https://www.notion.so/355830e8fbaa81f99b8bf9e4c8ae22f8).
- **Code (ground truth)** — `crates/paigasus-helikon/examples/leukemia_classifier.rs`, `paigasus-helikon-macros`, `agent_builder.rs`, both provider builders, `agent.rs`, `tool.rs`, `session.rs`, root `Cargo.toml`. Every API the spec uses was verified to exist.

Severity legend: **H** = high · **M** = medium · **L** = low · **N** = nit. Each item ends with a concrete **Correction**.

---

## H — High-severity

### H1. The delivered examples diverge from the *planned* "side-by-side" artifact — the parity/MVP-exit claim is overstated and the Notion page is stale

The ticket and the spec both call this the "side-by-side parity examples" and the **MVP / Stage-1 exit criterion** ("the public API is complete and ergonomic enough to build the canonical agent shapes"). But the planned canonical artifact — the Notion **Side-by-Side Comparison** — is explicitly *"a leukemia-lab triage agent that classifies a case … and **hands off** to either an MRD specialist or an AML cytogenetics specialist,"* implemented across all four reference SDKs. Its proposed Rust column uses, verbatim:

- `.handoffs([Handoff::to(mrd_agent), Handoff::to(aml_agent)])` — **handoffs are `NotImplemented`** (verified `loop_state.rs:549`, `agent.rs:976`); owned by SMA-324 (Stage 2, Backlog).
- `let decision: TriageDecision = result.final_output;` — **typed `final_output` via the runner was descoped in SMA-320** (the real API is `collect_typed::<T>()`); see the SMA-320 review (C1).
- `TokioRunner::new().with_tracing(otel_layer()).with_session(SqliteSession::open(…))` — **`TokioRunner` is a stateless unit struct** (SMA-321); there is no `with_tracing`/`with_session` builder.
- `openai::gpt_5()` / `openai::gpt_5_mini()` model constructors — the real API is `OpenAiModel::chat("…").build()?`.

So the canonical side-by-side example **cannot be reproduced today**, and the spec correctly (scope decision 1) reframes the triage examples to a *tool-using single agent* and proves **provider-switching parity** instead of the handoff topology. That pivot is honest and well-justified — but two gaps remain:

1. **The claim is oversold.** "Side-by-side parity" and "MVP exit criterion … build the canonical agent shapes" imply the cross-SDK comparison is reproduced. What's actually delivered is provider-switching + tool-use + structured-output + streaming — *not* the multi-agent/handoff shape the whole Side-by-Side Comparison is built around. The genuine MVP-exit proof for the canonical shape is deferred to SMA-324.
2. **The spec never references the Notion Side-by-Side Comparison page**, and that page still shows a Rust snippet that can't compile (handoffs, typed `final_output`, `TokioRunner` builder, `gpt_5()`). Left unreconciled, it's a stale "planned design" that contradicts reality — the same problem flagged for SMA-321 (fixed via ADR-13) and SMA-322.

**Correction.**
- Reframe the AC/goal text: SMA-323 proves **provider-switching parity** and the single-agent shapes; the **cross-SDK side-by-side triage parity** (handoff + typed runner output) is delivered by SMA-324 + the typed-output follow-up. Don't let "MVP exit criterion" read as "canonical shape proven."
- Reconcile the Notion **Side-by-Side Comparison** Rust column: mark it aspirational, or update it to compileable API and move the handoff/`final_output`/`TokioRunner`-builder bits behind a clearly-labeled "Stage 2 / planned" note. Cite the page in the spec.
- Record explicitly that the Notion snippet's `result.final_output: TriageDecision` is the SMA-320-descoped ergonomic — this ticket's `structured_output.rs` uses `collect_typed`, which is the honest current surface.

### H2. The benchmark measures `block_on`, not tool dispatch — so it doesn't prove the ticket's claim

The ticket asks the bench to "prove **dispatch overhead** is acceptable" and "measure `Tool::invoke` dispatch overhead." The spec's loop is `rt.block_on(tool.invoke(&ctx, args))` per Criterion iteration, and the spec itself admits *"at a trivial tool body the wall-clock is dominated by future-poll/`block_on`, not the vtable call."* That's the problem: the `<50 µs` signal is dominated by **runtime entry (`block_on`)**, so it neither characterizes dispatch nor can detect a dispatch regression hidden under that overhead. The thing the ticket wants measured (registry `Vec` name-lookup + `dyn Tool` vtable + JSON read) is swamped by the executor term.

**Correction.** Use Criterion's async support so the measurement reflects the future's poll cost, not `block_on`: add `criterion = { workspace = true, features = ["async_tokio"] }` and bench with `b.to_async(&rt).iter(|| tool.invoke(&ctx, args.clone()))`. That amortizes runtime entry across iterations and actually isolates dispatch. If you keep `block_on` for simplicity, then rename the claim honestly — it's a "single tool-call latency guard," not a "dispatch overhead" measurement — and say the number is `block_on`-dominated. (Either way, see L1 on the threshold.)

---

## L — Low

### L1. `<50 µs` is an arbitrary, one-off, machine-specific guard — fine, but don't oversell it as an MVP proof

The threshold isn't derived from anything (why 50 µs?), the `bench.yml` job is `workflow_dispatch`-only with no stored Criterion baseline, so there's no regression tracking over time, and the number is a single Linux-x86_64 reading pasted into `BENCHMARKS.md`. The spec is upfront ("loose by design," "only catches gross regressions"). That's acceptable for an examples ticket, but as a *Stage-1 exit* "prove dispatch is acceptable" it's a weak one-shot signal.

**Correction.** Keep it, but (a) state what 50 µs is relative to (e.g. "≫ the ~sub-µs vtable+lookup cost we expect, so any hit means something pathological"), and (b) note that real regression tracking would need the bench wired into a tracked baseline — a follow-up, not this ticket.

### L2. The ticket's own contract says "record the finding" — so file the `RunContext` ergonomics gap, don't just wave it off

The spec's premise is strong: *"If an example cannot be written cleanly against today's surface, that is a finding to record."* The verified reality is that every example must build the 5-arg `RunContext::new(Arc::new(()), Arc::new(MemorySession::new()), HookRegistry::new(), TracerHandle::default(), CancellationToken::new())` — exactly the kind of ceremony scope decision 3 blames for blowing the ±20% LOC parity. The spec then puts "ergonomic shims to shrink example LOC" out of scope (correct — not for *this* ticket) but stops short of **recording the finding** as its own contract requires.

**Correction.** File a follow-up ticket for a `RunContext` convenience (e.g. `RunContext::builder()` or `RunContext::ephemeral(user_ctx)` defaulting `MemorySession`/`HookRegistry`/`TracerHandle`/`CancellationToken`). That both honors the ticket's premise and is the single highest-leverage fix for the LOC-parity gap. Reference it from the spec.

### L3. `±20% LOC` was a ticket scope bullet, downgraded to aspirational

The Linear scope lists "Lines of code within ±20% of the Python original"; the spec demotes it to aspirational (scope decision 3). Reasonable given Rust ceremony — just note it's a disclosed deviation from the ticket bullet (and that L2 is the real lever to close the gap rather than abandon it).

---

## N — Nits

- **N1. Model currency.** `gpt-4o` is a 2024-era model; for mid-2026 examples it reads dated (the Notion comparison itself uses `gpt-5`/`gpt-5-mini`). `claude-sonnet-4-6` is current. Consider the current default OpenAI model so the examples don't look stale. Trivial to change; verify the live model id when writing.
- **N2. "byte-identical except the single model line" is slightly overstated.** `triage_anthropic.rs` also differs in the `use` import (`AnthropicModel` vs `OpenAiModel`) and the `required-features`. The provider-switch proof is real, but it's ~2 lines + feature gate, not one. Minor wording.
- **N3. Two near-duplicate triage files** must be kept in sync by hand. Fine for didactic "one line differs" clarity; just be aware that any change to the triage logic touches both. (A shared helper module + two thin `main`s is the alternative; not required.)
- **N4. `area:core` label vs "no core change."** The ticket carries `area:core` + `area:docs`, but the spec is (correctly) a no-core-change ticket. Consider dropping `area:core` so the label matches the work.

---

## Verified OK (checked against source — the examples will compile)

- **`leukemia_classifier.rs` exists** and already does exactly what `structured_output.rs` should (`AnthropicModel::messages("claude-sonnet-4-6").build()?`, `.output_type::<…>()`, `collect_typed`), so the rename is a clean no-op. It builds `RunContext` with **`MemorySession::new()`**, a public `core` type — so the new examples can construct a `RunContext` without a database or a test-only helper.
- **`#[tool]` and `tools!` exist** in `paigasus-helikon-macros` with the exact syntax the spec uses; `#[tool]` generates an `impl Tool<Ctx>`, `tools![…]` yields `Vec<Arc<dyn Tool<Ctx>>>`, and `LlmAgent::builder().tools(I: IntoIterator<Item = Arc<dyn Tool<Ctx>>>)` accepts it. `Ctx = ()` tools compose with `LlmAgent::builder::<()>()`.
- **Provider builders match:** `OpenAiModel::chat("…")` / `AnthropicModel::messages("…")`, both → `.build() -> Result<…, BuildError>`, so `.build()?` is correct.
- **`AgentEvent::TokenDelta { text: String }` exists** (streaming example), `agent.run(...) -> Result<BoxStream<'static, AgentEvent>, AgentError>`, and `RunResultStreaming::new(stream)` exists.
- **Bench primitives match:** `ToolContext::new(Arc<Ctx>, TracerHandle, CancellationToken)`, `Tool::invoke(&ToolContext, serde_json::Value) -> Result<ToolOutput, ToolError>`, `ToolOutput.content: serde_json::Value`. The "build setup outside the measured closure" guidance is correct Criterion practice.
- **handoff `NotImplemented` citations are accurate** (`loop_state.rs:549`, `agent.rs:976`), so reframing triage away from handoff is correctly motivated.
- **New artifacts are genuinely new:** `criterion` is not yet in `[workspace.dependencies]`, and there is no `benches/`, `BENCHMARKS.md`, or `bench.yml` — so the wiring steps are additive.
- **The no-core-change discipline is sound and achievable** — verified that all four examples + the bench are expressible against today's public surface.

---

## Required before writing the plan

1. **H1** — reframe the "parity / MVP-exit" claim to what's actually delivered (provider-switching + single-agent shapes), and reconcile the Notion **Side-by-Side Comparison** page (its Rust snippet uses handoffs, typed `final_output`, and a `TokioRunner` builder that don't exist). The canonical side-by-side triage parity is an SMA-324 (+ typed-output) deliverable.
2. **H2** — make the bench measure dispatch, not `block_on` (Criterion `to_async`), or rename the claim to a single-call latency guard.

Recommended alongside: **L2** (file the `RunContext` ergonomics finding the ticket's own premise demands). L1/L3/nits are polish.
