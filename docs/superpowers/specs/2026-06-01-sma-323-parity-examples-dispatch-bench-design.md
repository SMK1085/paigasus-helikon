# SMA-323 — Side-by-side parity examples + dispatch benchmark

- **Linear:** [SMA-323](https://linear.app/smaschek/issue/SMA-323/side-by-side-parity-examples-dispatch-benchmark)
- **Branch:** `feature/sma-323-side-by-side-parity-examples-dispatch-benchmark`
- **Milestone:** MVP (Stage 1 exit criterion)
- **Status:** design

## Goal

Reproduce the reference SDK quickstart examples as runnable Rust and prove
that tool-dispatch overhead is acceptable. This is the **MVP exit
criterion**: it demonstrates the public API is complete and ergonomic
enough to build the canonical agent shapes end-to-end against real
provider APIs.

This is a pure **consumer-of-the-public-API** ticket. It changes no
`-core` or provider code — that constraint is itself part of the proof.
If an example cannot be written cleanly against today's surface, that is a
finding to record, not a license to patch core under this ticket.

## Scope decisions (brainstorm outcomes)

1. **Triage examples are tool-using, not handoff-based.** The OpenAI Agents
   SDK "triage agent" quickstart is fundamentally a *handoff* example, but
   handoffs are deliberately **not** implemented yet — the loop returns
   `AgentError::NotImplemented { feature: "handoff" }`
   (`crates/paigasus-helikon-core/src/agent.rs:976`,
   `loop_state.rs:549`). Handoffs **and** agent-as-tool are owned by
   **[SMA-324 — Multi-agent: Handoff + AgentAsTool](https://linear.app/smaschek/issue/SMA-324/multi-agent-handoff-agentastool)**
   (`stage:2`, Backlog). For SMA-323 the triage examples reframe around what
   the SDK supports today: a single agent that calls tools. The Stage-1
   value the examples prove is **provider-switching parity** (the *same*
   agent on OpenAI and Anthropic), not the handoff topology. Handoff-topology
   parity is tracked by SMA-324 and reproduced there once handoffs land.

2. **`structured_output.rs` is the existing `leukemia_classifier.rs`,
   renamed.** `examples/leukemia_classifier.rs` (SMA-320) already is the
   structured-output demo (`output_type::<LeukemiaSubtypeAnalysis>()` +
   `collect_typed`). Rename it to the issue's canonical name rather than ship
   a near-duplicate.

3. **`±20% LOC` parity is aspirational, not a hard gate.** Rust's
   `RunContext::new(...)` ceremony (five `Arc`-wrapped args) adds lines the
   Python original hides. Each triage example carries a header comment citing
   the Python reference and approximate LOC; the ticket does not fail on the
   ratio.

4. **The bench lives in the facade crate**, co-located with the examples.
   `crates/paigasus-helikon/benches/tool_dispatch.rs`. The facade already
   carries a `tokio` dev-dep; the bench calls `core`'s `Tool::invoke`
   directly (the facade re-export adds zero runtime indirection).

5. **The authoritative `<50 µs` number comes from a one-off ubuntu CI run.**
   This dev box is macOS/arm64; acceptance requires Linux x86_64. A
   `workflow_dispatch` bench job on `ubuntu-latest` produces the number,
   which is pasted into `BENCHMARKS.md` with runner specs.

## Deliverables

All under `crates/paigasus-helikon/`. Five code artifacts + one doc + one CI
workflow.

### 1. `examples/structured_output.rs` (rename of `leukemia_classifier.rs`)

Content stays essentially as-is (Anthropic; `output_type::<LeukemiaSubtypeAnalysis>()`
→ `RunResultStreaming::collect_typed`). Only the filename and the
`[[example]]` entry name change. Required-features: `["anthropic"]`.

### 2. `examples/triage_openai.rs` — tool-using leukemia triage agent

- **Ctx:** `()`.
- **Tools** (via `#[tool]` on `async fn`, registered with `tools![…]`):
  - `lookup_marker_profile(args: { markers: Vec<String> }) -> { candidate_subtypes: Vec<String>, note: String }`
    — a canned immunophenotype table (e.g. CD13/CD33/MPO → AML;
    CD10/CD19 → ALL; CD5/CD23 → CLL). No network; pure lookup.
  - `blast_threshold_check(args: { blast_percent: u32 }) -> { meets_acute_threshold: bool }`
    — returns `blast_percent >= 20`.
  - Two tools so the example exercises multi-tool dispatch through the real
    loop, not just a single call.
- **Model:** `OpenAiModel::chat("gpt-4o").build()?` (reads `OPENAI_API_KEY`).
- **Instructions:** a hematopathology triage assistant that must use the
  tools to look up marker associations and check the blast threshold, then
  recommend the most likely subtype and the next diagnostic step.
- **Input:** the flow-cytometry vignette
  (`"Flow: CD13+ CD33+ CD34+ MPO+. Blasts 80%. Auer rods present."`).
- **Drive:** bare `agent.run(ctx, input).await?` (the core loop driver runs
  tools without a runtime crate) → `RunResultStreaming::new(stream).collect()`
  → print the plain-text recommendation. Plain text (not structured) keeps
  triage distinct from `structured_output.rs`.
- Required-features: `["openai", "macros"]`.

### 3. `examples/triage_anthropic.rs` — same agent, swapped provider

Byte-identical to `triage_openai.rs` **except the single model-construction
line** (`AnthropicModel::messages("claude-sonnet-4-6").build()?`), carrying a
`// ⇐ only line that differs vs triage_openai.rs` marker. That one-line diff
is the provider-switching proof. Required-features: `["anthropic", "macros"]`.

### 4. `examples/streaming_console.rs` — token-by-token to stdout

Simple agent (no tools). `agent.run(ctx, input).await?` → iterate the
`BoxStream<AgentEvent>`; on `AgentEvent::TokenDelta { text }` →
`print!("{text}")` + `std::io::stdout().flush()`. OpenAI (`gpt-4o`) — keeps
the four examples balanced 2 OpenAI / 2 Anthropic. The header comment notes
the example is provider-agnostic. Required-features: `["openai"]`.

### 5. `benches/tool_dispatch.rs` — Criterion microbench

- `harness = false`; `[[bench]] name = "tool_dispatch"`.
- Hand-rolled trivial `Tool<()>` (e.g. `AddTool` returning `{ sum: a + b }`),
  held as `Arc<dyn Tool<()>>` inside a `Vec<Arc<dyn Tool<()>>>` so the bench
  measures the realistic registry path.
- **Measured hot path:** name-lookup in the registry `Vec` + `invoke`
  through the `dyn Tool` vtable + reading the returned JSON
  `ToolOutput.content`.
- **Setup built once, outside the measured closure:** the `ToolContext`
  (`ToolContext::new(Arc::new(()), TracerHandle::default(), CancellationToken::new())`)
  and a current-thread tokio runtime; the closure runs
  `rt.block_on(tool.invoke(&ctx, args))`.
- **Target:** `< 50 µs`. This is a "dispatch is not pathologically slow"
  guard — at a trivial tool body the wall-clock is dominated by
  future-poll/`block_on`, not the vtable call, so the target is very loose.
- **Explicitly not measured:** any network/provider latency, model
  invocation, or the full agent loop. This isolates `Tool::invoke` dispatch.

### 6. `BENCHMARKS.md` (repo root)

- What the bench measures and what it deliberately excludes.
- How to run: `cargo bench -p paigasus-helikon --bench tool_dispatch`.
- The authoritative **Linux x86_64** result + runner specs (from the CI run),
  against the `< 50 µs` target.
- A note that local macOS/arm64 figures are indicative only.

### 7. `.github/workflows/bench.yml` (one-off, reusable)

- Trigger: `workflow_dispatch` only (not on PR/push — Criterion in CI is
  noisy and this is not a required gate).
- `runs-on: ubuntu-latest` (Linux x86_64).
- Steps: checkout → rust toolchain → `cargo bench -p paigasus-helikon
  --bench tool_dispatch` → surface the number in the job log.
- All `uses:` pinned to commit SHA with an above-the-fold `# action vX.Y.Z`
  comment, per CLAUDE.md "Always implement GitHub Actions against the latest
  stable major."
- `permissions: contents: read`.

## Cargo wiring (`crates/paigasus-helikon/Cargo.toml`)

- Rename `[[example]] name = "leukemia_classifier"` → `"structured_output"`.
- Add `[[example]]` entries:
  - `triage_openai` — `required-features = ["openai", "macros"]`
  - `triage_anthropic` — `required-features = ["anthropic", "macros"]`
  - `streaming_console` — `required-features = ["openai"]`
- Add `[[bench]] name = "tool_dispatch", harness = false`.
- Add `criterion = { workspace = true }` to `[dev-dependencies]`.
- `serde`, `serde_json`, `schemars` are already present (needed by the
  `#[tool]` arg/output derives).

## Root `Cargo.toml`

- Add `criterion` to `[workspace.dependencies]`, pinned to the current
  stable major.

## Acceptance criteria (restated)

- All four examples compile and run end-to-end against the real APIs
  (env-gated). CI `--all-features` covers the **compile** half (it satisfies
  every `required-features` and builds the examples). The **run** half is the
  manual acceptance step: run each with the relevant key
  (`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`) and confirm sane output.
- Benchmark target (`< 50 µs`) met on Linux x86_64; the number is recorded in
  `BENCHMARKS.md` with runner specs.

## Out of scope

- **Handoff / agent-as-tool topology** → SMA-324 (Stage 2). The triage
  examples are tool-using only.
- **Ergonomic shims to shrink example LOC** (e.g. a default-`RunContext`
  convenience). Not added here; `±20%` is aspirational.
- **Any `-core` / provider source change.** If the examples surface a real
  ergonomics gap, it is filed separately, not patched under this ticket.

## Risks / open questions

- **Real-API runs are manual and key-gated.** CI cannot run them without
  secrets; we rely on compile coverage + a manual pass. Acceptable for an
  examples ticket.
- **`<50 µs` is loose by design.** If a future change regresses dispatch, this
  guard only catches gross regressions; it is not a tight performance SLO.
- **macOS/arm64 ≠ Linux x86_64.** The authoritative number must come from the
  CI job; local figures are labelled indicative.

## Verification (CI gates this must pass)

`cargo fmt --all --check`, `clippy --workspace --all-features --all-targets
-D warnings`, `cargo test --workspace --all-features` (compiles the
examples), `cargo doc` with `-D warnings`, doc-coverage, and `convco`/PR-title
gates. The new bench compiles under `--all-targets`; the example header
docs use `//!` doc comments consistent with the existing examples.
