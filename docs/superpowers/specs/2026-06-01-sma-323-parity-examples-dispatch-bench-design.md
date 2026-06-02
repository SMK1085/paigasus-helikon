# SMA-323 — Side-by-side parity examples + dispatch benchmark

- **Linear:** [SMA-323](https://linear.app/smaschek/issue/SMA-323/side-by-side-parity-examples-dispatch-benchmark)
- **Branch:** `feature/sma-323-side-by-side-parity-examples-dispatch-benchmark`
- **Milestone:** MVP (Stage 1 exit criterion)
- **Status:** design

> **Domain note (2026-06-01).** The example domain was changed from hematopathology
> (AML/MDS subtype classification) to a **personal-finance assistant**. The leukemia
> framing required domain expertise to even read, which works against the goal of a
> *relatable* flagship example set. Personal finance exercises the same SDK surface
> (tools, multi-tool dispatch, structured output, provider switching, streaming) while
> being legible to anyone. The Notion [Side-by-Side Comparison](https://www.notion.so/355830e8fbaa81ce86d7e8caadb96d47)
> is re-domained to match.

## Goal

Reproduce the reference SDK quickstart examples as runnable Rust and prove
that tool-dispatch overhead is acceptable. This is the **MVP exit
criterion**: it demonstrates the public API is complete and ergonomic
enough to build the canonical single-agent shapes end-to-end against real
provider APIs.

This is a pure **consumer-of-the-public-API** ticket. It changes no
`-core` or provider code — that constraint is itself part of the proof.
If an example cannot be written cleanly against today's surface, that is a
finding to record (and file as a follow-up), not a license to patch core
under this ticket.

**Honest scope of the "parity" claim.** SMA-323 proves **provider-switching
parity** (the *same* agent on OpenAI and Anthropic) plus the single-agent
shapes (tool use, structured output, streaming). It does **not** reproduce the
cross-SDK *triage-and-route* topology that the Notion Side-by-Side Comparison is
built around — that requires handoffs (`AgentError::NotImplemented` today) and is
owned by **[SMA-324](https://linear.app/smaschek/issue/SMA-324/multi-agent-handoff-agentastool)**
(Stage 2). The full side-by-side parity lands when SMA-324 + the typed-runner-output
follow-up land; see Scope decision 1.

## Scope decisions (brainstorm outcomes)

1. **The flagship multi-agent example is *triage-and-route*, but that is SMA-324.**
   The reference quickstarts (OpenAI Agents SDK, Claude Agent SDK, ADK, Strands) all
   center on a *triage* agent that classifies a request and **hands off** to a
   specialist. Handoffs are deliberately **not** implemented yet — the loop returns
   `AgentError::NotImplemented { feature: "handoff" }`
   (`crates/paigasus-helikon-core/src/agent.rs:976`, `loop_state.rs:549`). Handoffs
   **and** agent-as-tool are owned by **SMA-324** (`stage:2`). For SMA-323 the examples
   reframe around what the SDK supports today: a single personal-finance assistant that
   calls tools. The Stage-1 value the examples prove is **provider-switching parity**
   (the *same* agent on OpenAI and Anthropic), not the handoff topology. Handoff-topology
   parity (the finance triage→budgeting/investing specialist shape in the Notion
   comparison) is reproduced under SMA-324 once handoffs land.

2. **`structured_output.rs` replaces `leukemia_classifier.rs`, re-domained.**
   `examples/leukemia_classifier.rs` (SMA-320) is the existing structured-output demo
   (`output_type::<…>()` + `collect_typed`). Rather than ship a near-duplicate, it is
   **rewritten in the finance domain and renamed** `structured_output.rs`: same API path
   (`output_type::<TransactionCategory>()` → `collect_typed`), new content
   (transaction categorization). This also retires the niche `LeukemiaSubtypeAnalysis`
   type from the public example surface.

3. **`±20% LOC` parity is aspirational, not a hard gate.** Rust's
   `RunContext::new(...)` ceremony (five args incl. an `Arc<dyn Session>`) adds lines the
   Python originals hide. Each example carries a header comment citing the reference and
   approximate LOC; the ticket does not fail on the ratio. The ceremony itself is a
   recorded ergonomics finding — see "Findings to file".

4. **The bench lives in the facade crate**, co-located with the examples:
   `crates/paigasus-helikon/benches/tool_dispatch.rs`. The facade already carries a
   `tokio` dev-dep; the bench calls `core`'s `Tool::invoke` directly.

5. **The authoritative `<50 µs` number comes from a one-off ubuntu CI run.**
   This dev box is macOS/arm64; acceptance requires Linux x86_64. A `workflow_dispatch`
   bench job on `ubuntu-latest` produces the number, pasted into `BENCHMARKS.md` with
   runner specs. (See "Findings to file" re: what the bench actually measures.)

## Deliverables

All under `crates/paigasus-helikon/`. Four examples + a bench + one doc + one CI
workflow. Model ids below are illustrative (`gpt-5` / `gpt-5-mini` / `claude-sonnet-4-6`,
consistent with the Notion docs) — confirm against live provider docs when writing.

### 1. `examples/structured_output.rs` — categorize a transaction (Anthropic)

Re-domain of `leukemia_classifier.rs`. Anthropic; `output_type::<TransactionCategory>()`
→ `RunResultStreaming::collect_typed`.

(The Notion comparison's Rust column shows `let decision: TriageDecision = result.final_output;`
— a typed runner `final_output` that is **not the current surface**: typed runner output was
descoped in SMA-320 in favor of `collect_typed::<T>()` / `RunResult::parse_final::<T>()`. This
example uses `collect_typed`, the honest current API. The Notion page was reconciled
2026-06-01: it now splits a "Target shape (Stage 2)" block (handoffs/typed-runner, annotated
`← SMA-324`) from a "Works today (Stage 1)" block that uses `collect_typed`.)

```rust
#[derive(Debug, Deserialize, JsonSchema)]
struct TransactionCategory {
    /// Spending category, e.g. "Groceries", "Dining", "Transport", "Entertainment".
    category: String,
    /// 0.0–1.0 confidence in the category.
    confidence: f32,
    /// True if this looks like a recurring charge (subscription, utility, rent).
    recurring: bool,
    /// One-sentence justification.
    reasoning: String,
}
```

- **Model:** `AnthropicModel::messages("claude-sonnet-4-6").build()?`.
- **Instructions:** "You are a personal-finance assistant. Categorize the transaction
  into a single spending category and say whether it looks recurring."
- **Input:** `"NETFLIX.COM 866-579-7172 CA — $15.49"` (a recurring entertainment charge,
  so the example exercises `recurring: true`).
- **Drive:** `agent.run(ctx, input).await?` → `RunResultStreaming::new(stream).collect_typed::<TransactionCategory>().await?`
  → print the struct. Required-features: `["anthropic"]`.

### 2. `examples/budget_assistant_openai.rs` — tool-using budget Q&A (OpenAI)

(Renamed from the ticket's `triage_openai.rs`: these examples do *not* route/triage —
they answer a budgeting question using tools — so a `budget_assistant_*` name is more
honest than `triage_*`. The triage/handoff shape is SMA-324.)

- **Ctx:** `()`.
- **Tools** (via `#[tool]` on `async fn`, registered with `tools![…]`):
  - `lookup_spending(args: { category: String, month: String }) -> { total: f64, count: u32 }`
    — a canned in-memory ledger (e.g. `Dining → { total: 312.40, count: 18 }`,
    `Groceries → { total: 540.10, count: 9 }`). No network; pure lookup.
  - `budget_status(args: { category: String }) -> { budget: f64, spent: f64, remaining: f64 }`
    — canned monthly budgets (e.g. `Dining → { budget: 250.00, spent: 312.40, remaining: -62.40 }`).
  - Two tools so the example exercises multi-tool dispatch through the real loop, not a
    single call.
- **Model:** `OpenAiModel::chat("gpt-5-mini").build()?` (reads `OPENAI_API_KEY`).
- **Instructions:** a budgeting assistant that must use the tools to look up the user's
  spending and budget for the relevant category, then say whether they're on track and
  suggest one concrete action.
- **Input:** `"How am I doing on my dining budget this month?"`
- **Drive:** bare `agent.run(ctx, input).await?` (the core loop driver runs tools without
  a runtime crate) → `RunResultStreaming::new(stream).collect()` → print the plain-text
  recommendation. Plain text (not structured) keeps this distinct from
  `structured_output.rs`. Required-features: `["openai", "macros"]`.

### 3. `examples/budget_assistant_anthropic.rs` — same agent, swapped provider

Byte-identical to `budget_assistant_openai.rs` **except the `use` import and the single
model-construction line** (`AnthropicModel::messages("claude-sonnet-4-6").build()?`),
carrying a `// ⇐ only logic-relevant line that differs vs the OpenAI variant` marker.
That ~one-line diff is the provider-switching proof. Required-features:
`["anthropic", "macros"]`.

### 4. `examples/streaming_console.rs` — token-by-token to stdout

Simple agent (no tools). `agent.run(ctx, input).await?` → iterate the
`BoxStream<AgentEvent>`; on `AgentEvent::TokenDelta { text }` → `print!("{text}")` +
`std::io::stdout().flush()`. OpenAI (`gpt-5`) — keeps the four examples balanced
2 OpenAI / 2 Anthropic. Input: `"Give me three quick tips to trim my monthly
subscriptions."` The header comment notes the example is provider-agnostic.
Required-features: `["openai"]`.

### 5. `benches/tool_dispatch.rs` — dependency-free microbench

- `harness = false`; `[[bench]] name = "tool_dispatch", test = false`. The bench is a plain
  `fn main()` binary — **no Criterion, no new dependencies** (uses only `tokio`,
  `async-trait`, `serde_json`, already dev-deps).
- **Why not Criterion (decided during implementation):** Criterion transitively pulls
  `clap_lex 1.1.0`, whose manifest declares `edition = "2024"`. Cargo 1.75 cannot *parse*
  that during resolution, so adding Criterion would break the workspace's Rust 1.75 MSRV
  for the whole `cargo test --workspace` (a resolution-time failure that `[[bench]]
  test = false` cannot avoid). A hand-rolled timing loop sidesteps it entirely. (`test =
  false` is kept on its own merit: a benchmark is not a test, so it stays out of `cargo
  test` / the 1.75 job.)
- Trivial `SumTool` implementing `Tool<()>` (returns `{ total: a + b }`), held as
  `Arc<dyn Tool<()>>` in a `Vec<Arc<dyn Tool<()>>>` so the bench measures the realistic
  registry path.
- **Measured hot path:** name-lookup in the registry `Vec` + `invoke` through the
  `dyn Tool` vtable + reading the returned JSON `ToolOutput.content`.
- **Setup built once, before timing:** the `ToolContext`, the registry, and a
  current-thread tokio runtime.
- **Measurement:** a **single** `rt.block_on(async { … })` wraps a warmup loop *and* the
  measured loop, so tokio runtime entry is amortized across all iterations rather than
  charged per call (the same property `Criterion::to_async` would give, without the
  dependency). The measured loop is timed with `std::time::Instant` over a fixed iteration
  count; per-call = elapsed / iters. `std::hint::black_box` on the output prevents the
  optimizer eliding the work. The bench `println!`s the per-call time and `assert!`s it is
  `< 50 µs`.
- **Target:** `< 50 µs`. A deliberately loose "dispatch is not pathologically slow" guard:
  the registry lookup + vtable call + JSON read should cost on the order of sub-µs, so 50 µs
  is ~50× headroom — only a pathological regression trips it. It is **not** a tight SLO and
  has no stored baseline (see Risks); real regression tracking is a follow-up.
- **Explicitly not measured:** network/provider latency, model invocation, the full
  agent loop.

### 6. `BENCHMARKS.md` (repo root)

- What the bench measures and what it deliberately excludes.
- How to run: `cargo bench -p paigasus-helikon --bench tool_dispatch`.
- The authoritative **Linux x86_64** result + runner specs (from the CI run), against the
  `< 50 µs` target.
- A note that local macOS/arm64 figures are indicative only.

### 7. `.github/workflows/bench.yml` (one-off, reusable)

- Trigger: `workflow_dispatch` only (benchmarks in CI are noisy; not a required gate).
- `runs-on: ubuntu-latest` (Linux x86_64).
- Steps: checkout → rust toolchain → `cargo bench -p paigasus-helikon --bench tool_dispatch`
  → surface the number in the job log.
- All `uses:` pinned to commit SHA with an above-the-fold `# action vX.Y.Z` comment, per
  CLAUDE.md "Always implement GitHub Actions against the latest stable major."
- `permissions: contents: read`.

## Cargo wiring (`crates/paigasus-helikon/Cargo.toml`)

- Rename `[[example]] name = "leukemia_classifier"` → `"structured_output"`.
- Add `[[example]]` entries:
  - `budget_assistant_openai` — `required-features = ["openai", "macros"]`
  - `budget_assistant_anthropic` — `required-features = ["anthropic", "macros"]`
  - `streaming_console` — `required-features = ["openai"]`
- Add `[[bench]] name = "tool_dispatch", harness = false, test = false`.
- **No new dependencies** — the dependency-free bench uses `tokio`, `async-trait`, and
  `serde_json`, all already dev-deps (see Deliverable 5 for why Criterion was rejected).
- `serde`, `serde_json`, `schemars` are already present (needed by the `#[tool]`
  arg/output derives).

## Root `Cargo.toml`

- No change. The dependency-free bench adds nothing to `[workspace.dependencies]` (Criterion
  was rejected — see Deliverable 5).

## Acceptance criteria (restated)

- All four examples compile and run end-to-end against the real APIs (env-gated). CI
  `--all-features` covers the **compile** half (it satisfies every `required-features` and
  builds the examples). The **run** half is the manual acceptance step: run each with the
  relevant key (`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`) and confirm sane output.
- Benchmark target (`< 50 µs`) met on Linux x86_64; recorded in `BENCHMARKS.md` with
  runner specs.
- **Parity claim, honestly scoped:** provider-switching parity is demonstrated by the two
  `budget_assistant_*` examples; cross-SDK *triage-and-route* parity is explicitly an
  SMA-324 deliverable, reflected in the Notion Side-by-Side Comparison.

## Out of scope

- **Handoff / agent-as-tool topology** → SMA-324 (Stage 2). The examples are tool-using
  single agents only.
- **Ergonomic shims to shrink example LOC** (e.g. a default-`RunContext` convenience).
  Not added here; recorded as a finding to file (below).
- **Any `-core` / provider source change.** If the examples surface a real ergonomics
  gap, it is filed separately, not patched under this ticket.

## Findings to file (per the ticket's own "record the finding" contract)

- **`RunContext` construction ceremony.** Every example builds
  `RunContext::new(Arc::new(()), Arc::new(MemorySession::new()), HookRegistry::new(),
  TracerHandle::default(), CancellationToken::new())`. A `RunContext::builder()` /
  `RunContext::ephemeral(user_ctx)` convenience (defaulting `MemorySession` / empty hooks
  / default tracer / fresh cancel token) would shrink every example and is the real lever
  for the ±20% LOC gap. Filed as **[SMA-403](https://linear.app/smaschek/issue/SMA-403)**
  (`area:core`, `stage:2`); the SMA-323 examples ship with the verbose `RunContext::new` form
  and migrate once SMA-403 lands.
- **Pre-existing Rust 1.75 MSRV break via `home 0.5.12` (edition2024).** Discovered while
  validating the bench: `sqlx-postgres → etcetera → home 0.5.12` is already in `main`'s
  `Cargo.lock` and declares `edition = "2024"`, which Cargo 1.75 cannot parse — so
  `cargo +1.75 test --workspace --all-features` already fails on `main`, independent of
  SMA-323. Out of scope here (this ticket only avoids *adding* a second such break by
  rejecting Criterion). Candidate follow-up: pin `home`/`etcetera` down, or track until
  the `sqlx` chain drops the edition2024 dep.

## Risks / open questions

- **Real-API runs are manual and key-gated.** CI cannot run them without secrets; we rely
  on compile coverage + a manual pass. Acceptable for an examples ticket.
- **`<50 µs` is loose by design** and the bench job is one-off (no stored baseline), so it
  only catches gross regressions; it is not a tight SLO.
- **macOS/arm64 ≠ Linux x86_64.** The authoritative number must come from the CI job.

## Verification (CI gates this must pass)

`cargo fmt --all --check`, `clippy --workspace --all-features --all-targets -D warnings`,
`cargo test --workspace --all-features` (compiles the examples), `cargo doc` with
`-D warnings`, doc-coverage, and `convco`/PR-title gates. The new bench compiles under
`--all-targets`; the example header docs use `//!` doc comments consistent with the
existing examples.
