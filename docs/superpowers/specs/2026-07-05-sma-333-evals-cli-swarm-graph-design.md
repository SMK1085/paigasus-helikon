# SMA-333 — paigasus-helikon-evals + paigasus-helikon-cli (+ Swarm/Graph)

**Ticket:** [SMA-333](https://linear.app/smaschek/issue/SMA-333/paigasus-helikon-evals-paigasus-helikon-cli-swarmgraph)
**Branch:** `feature/sma-333-paigasus-helikon-evals-paigasus-helikon-cli-swarmgraph`
**References:** Notion — Observability & Evaluation, Multi-Agent Patterns, Crate Reference.

## 1. Problem & goal

This is the final Stage 3 grab bag: three independent-but-bundled deliverables that
close out the Notion crate roster.

1. **`paigasus-helikon-evals`** — the evaluation framework: datasets, an
   `Evaluator` trait with four built-in evaluators, a `MockModel` for
   deterministic replay, and a trace recorder (SQLite/Parquet) for offline
   analysis. The crate ascends from its `0.0.0` stub to a published `0.1.0`.
2. **`paigasus-helikon-cli`** — the `helikon` binary (plus the `paigasus-helikon`
   shim alias) gains its first real subcommands: `repl` (hot-reloading TOML/Rhai
   agent definitions), `eval run`, and `mcp serve`. The crate ascends to a
   published **binary** crate at `0.1.0` so `cargo install paigasus-helikon-cli`
   works ("installed by Cargo"). It remains "never published as a library" in the
   semantic sense: its lib target is internal plumbing with no stability promise.
3. **Core multi-agent shapes** — `SwarmAgent<Ctx>` and `GraphAgent<Ctx>` join
   `SequentialAgent`/`ParallelAgent`/`LoopAgent` in `paigasus-helikon-core`,
   completing the Multi-Agent Patterns table.

Decisions made with Sven at intake: **one PR** for the whole ticket; **CLI
publishes** as a binary crate; REPL sidecar depth is **TOML agent definitions +
Rhai-scripted tools** (not TOML-only, not a full Rhai DSL).

### Acceptance criteria (verbatim from ticket)

* `helikon eval run datasets/triage.jsonl --agent triage` produces trajectory +
  final-response scores in CI.
* `helikon repl` hot-reloads on agent file change without restarting.
* Swarm example with 3 agents converges on a winner within `max_turns`.

## 2. Architecture overview

```
paigasus-helikon-core          paigasus-helikon-evals         paigasus-helikon-cli
┌─────────────────────┐        ┌─────────────────────┐        ┌──────────────────────┐
│ swarm.rs  SwarmAgent│        │ MockModel (Model)   │        │ helikon repl         │
│ graph.rs  GraphAgent│◄───────│ EvalDataset/EvalRun │◄───────│ helikon eval run     │
│ (existing: Agent,   │  uses  │ Evaluator + 4 impls │  uses  │ helikon mcp serve    │
│  Handoff, workflow) │        │ TraceSink: sqlite/  │        │ agents.toml + .rhai  │
└─────────────────────┘        │            parquet  │        │ (Ctx = ())           │
                               └─────────────────────┘        └──────────────────────┘
```

Dependency rule that shapes everything below: **the evals and cli tarballs must
compile against the *registry* versions of their dependencies** (`cargo publish
--verify` strips `path`), because both ascend in this PR. Therefore neither crate
may use core API added in this same PR — Swarm/Graph are consumed only by core's
own tests and the facade's examples. This avoids the manual core+facade bump
ritual entirely: release-plz auto-bumps core (for the Swarm/Graph feat) and
cascades the facade in the bot release PR, which works precisely because
release-plz performs those bumps itself.

## 3. Core: `SwarmAgent<Ctx>` and `GraphAgent<Ctx>`

### 3.1 SwarmAgent — semantics

Per Notion Multi-Agent Patterns: a pool of `LlmAgent` members with **handoff
tools auto-injected full-mesh**; execution is a sequential handoff chain (the
existing `LlmAgent` loop already drives `transfer_to_<name>` tools and agent
switching); the swarm **ends when the active member produces a final output**
instead of handing off. "First to produce a final output wins." The chain shares
one driver loop, so `RunConfig::max_turns` bounds the whole swarm run — that is
the AC's "converges on a winner within `max_turns`".

### 3.2 SwarmAgent — API (illustrative)

```rust
let swarm = SwarmAgent::builder()
    .name("support_swarm")
    .description("Personal-finance support pool")
    .member(triage)        // LlmAgent<Ctx, M1, T1> — heterogeneous M/T per call
    .member(budgeting)     // LlmAgent<Ctx, M2, T2>
    .member(investing)
    .entry("triage")       // optional; defaults to the first member
    .max_turns(12)         // optional; sets each member's RunConfig
    .build()?;             // SwarmBuildError on empty/duplicate/unknown-entry
```

`impl Agent<Ctx> for SwarmAgent<Ctx>`: `run()` delegates to the entry member's
stream. Build-time validation: at least one member, unique member names, entry
name exists. Errors: `SwarmBuildError { Empty, DuplicateMember(String),
UnknownEntry(String) }` (`thiserror`, `#[non_exhaustive]`).

**Wiring without `Arc` cycles.** Full-mesh `Handoff`s between `Arc`'d members
would create strong reference cycles (A→B→A) and leak. Instead the builder
creates one private **member slot** per member — a tiny `Agent<Ctx>` adapter
holding the member's name/description (copied) and a `Weak<dyn Agent<Ctx>>` —
wires every member's `handoffs` with `Handoff::shared(slot_j)` for all `j ≠ i`
(appending to any pre-existing handoffs), then fills each slot's weak reference
with the finished member `Arc`. The swarm holds the strong `Arc`s; slots upgrade
on use and fail with a run-start `AgentError` if the swarm was dropped
mid-flight (cannot happen while `SwarmAgent` itself is alive). Heterogeneous
member types are handled by capturing each `.member(agent)` as a deferred
`FnOnce(Vec<Handoff<Ctx>>) -> Arc<dyn Agent<Ctx>>` so handoff injection happens
on the concrete `LlmAgent` (public `handoffs` field) before type erasure.

Members are `LlmAgent`s only — they are the only agents that can call transfer
tools. (A `dyn Agent` "terminal member" that can receive but never initiate a
handoff is a noted future extension, not in scope.)

### 3.3 GraphAgent — semantics

A declared DAG: nodes are agents, directed edges are dependencies. A node runs
when **all** its predecessors completed (Kahn wavefront); independent ready
nodes run concurrently via cooperative `futures::select_all`, exactly like
`ParallelAgent` (core has no tokio runtime). Each completed node's final text is
recorded under a session-state key (mirroring `ParallelAgent`'s branch keys),
and is appended to each successor's input as a labeled context message.

Termination: all nodes complete → the graph emits one synthesized final
assistant message aggregating the **sink** nodes' outputs (single sink: its text
verbatim; multiple sinks: keyed merge, same convention as `ParallelAgent`).
Always synthesizing the final message keeps `RunResultStreaming::collect()`
deterministic — with concurrent branches, "last assistant message in the
stream" would otherwise depend on completion timing.

Failure: collect-all like `ParallelAgent` — a failed node marks its descendants
skipped, independent branches still complete, then one aggregate `RunFailed`
names the failed and skipped nodes.

### 3.4 GraphAgent — API (illustrative)

```rust
let graph = GraphAgent::builder()
    .name("monthly_report")
    .description("Fan-out research, fan-in summary")
    .node("spending", spending_agent)   // impl Agent<Ctx> + 'static (or Arc<dyn ...>)
    .node("income", income_agent)
    .node("summary", summary_agent)
    .edge("spending", "summary")        // spending → summary
    .edge("income", "summary")
    .build()?;                          // GraphBuildError on cycle/unknown/duplicate/empty
```

`GraphBuildError { Empty, DuplicateNode(String), UnknownNode(String),
Cycle(Vec<String>) }`. Cycle detection at `build()` via Kahn's algorithm; the
error carries the offending nodes.

### 3.5 Module layout (core)

- `crates/paigasus-helikon-core/src/swarm.rs` — `SwarmAgent`, builder, slot
  adapter, `SwarmBuildError`.
- `crates/paigasus-helikon-core/src/graph.rs` — `GraphAgent`, builder,
  `GraphBuildError`.
- `lib.rs` re-exports both. (`workflow.rs` is already large; new files, same
  conventions: doc comments on every pub item, GenAI span emission consistent
  with Sequential/Parallel/Loop.)

## 4. `paigasus-helikon-evals`

### 4.1 Crate surface

```rust
// dataset.rs
pub struct EvalCase {
    pub id: String,
    pub input: String,                        // user-turn text
    pub expected: Option<serde_json::Value>,  // expected final output (string or JSON)
    pub expected_tools: Option<Vec<String>>,  // expected tool-call names, in order
    pub metadata: serde_json::Map<String, serde_json::Value>,
}
pub struct EvalDataset { pub name: String, pub cases: Vec<EvalCase> }
impl EvalDataset {
    pub fn from_jsonl_path(path: &Path) -> Result<Self, EvalError>; // one EvalCase per line
    pub fn from_jsonl_str(name: &str, s: &str) -> Result<Self, EvalError>;
}

// evaluator.rs
pub struct CaseOutcome {
    pub final_output: String,
    pub events: Vec<AgentEvent>,   // full trajectory from RunResult
    pub usage: Usage,
}
pub struct Score {
    pub value: f64,              // ∈ [0, 1]
    pub passed: bool,
    pub skipped: bool,           // evaluator not applicable to this case
    pub detail: Option<String>,
}
#[async_trait]
pub trait Evaluator: Send + Sync {
    fn name(&self) -> &str;
    async fn evaluate(&self, case: &EvalCase, outcome: &CaseOutcome) -> Result<Score, EvalError>;
}
```

**Applicability rule** (library-level, so the CLI inherits it): an evaluator
whose required case field is absent (`ExactMatch`/`JsonSchemaConformance`/
`LlmJudge` without `expected` where they need it, `ToolUseTrajectory` without
`expected_tools`) returns `Score::skipped(reason)`. Skipped scores don't count
toward pass/fail or summary means; `EvalSummary` reports skip counts per
evaluator so silent no-ops are visible in the output.

Built-in evaluators (`evaluators/` module):

| Evaluator | Kind | Scoring |
|---|---|---|
| `ExactMatch` | final-response | Trimmed string equality; if `expected` is JSON (non-string), structural `serde_json::Value` equality after parsing the output. Options: `case_insensitive()`. 0/1. |
| `JsonSchemaConformance` | final-response | Parses `final_output` as JSON, validates against a constructor-supplied JSON Schema (`jsonschema` crate, draft 2020-12). 0/1; `detail` lists violations. |
| `LlmJudge` | final-response | Wraps an `Arc<dyn Model>` + rubric prompt; sends input/expected/actual, asks for `{"score": 0..1, "reasoning": "…"}`; lenient JSON extraction; `passed = score ≥ threshold` (default 0.7). No tools — a bare model call, not a full agent. |
| `ToolUseTrajectory` | trajectory | Extracts the tool-call name sequence from `events`; modes `exact()` (default) and `in_order()` (subsequence). Score = matched fraction; `passed = score == 1.0`; `detail` shows expected vs actual. Compares against `case.expected_tools`. |

### 4.2 EvalRun

```rust
let report = EvalRun::builder()
    .dataset(dataset)
    .agent(agent)                       // impl Agent<Ctx> or Arc<dyn Agent<Ctx>>
    .ctx_factory(|| ())                 // fresh Ctx per case — case isolation
    .evaluator(ExactMatch::new())
    .evaluator(ToolUseTrajectory::exact())
    .concurrency(4)                     // default 1 (sequential, deterministic order)
    .trace(SqliteTraceSink::open(path).await?)   // optional
    .run().await?;                      // -> EvalReport
assert!(report.passed());
```

Each case runs on a fresh ephemeral `RunContext` (fresh `MemorySession`) via
`TokioRunner` (evals depends on `paigasus-helikon-runtime-tokio`; a
`.runner(Arc<dyn Runner<Ctx>>)` override exists for other runtimes).
`EvalReport { dataset, results: Vec<CaseResult>, summary: EvalSummary }` where
`CaseResult` carries the outcome plus per-evaluator `Score`s and `EvalSummary`
aggregates per-evaluator mean score and pass rate. `EvalReport::passed()` is
true iff every case passed every evaluator. `EvalReport` is `Serialize` (JSON
output for CI) and has a plain-text `render_table()` for terminals.

### 4.3 MockModel + recorded scripts

Promote the test-double that already exists in
`crates/paigasus-helikon-core/tests/common/mod.rs` into a public, documented
type:

```rust
pub struct MockModel { /* Mutex<VecDeque<Vec<ModelEvent>>> */ }
impl MockModel {
    pub fn with_script(script: Vec<ModelEvent>) -> Arc<Self>;
    pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self>; // one per invoke()
    pub fn from_script_file(path: &Path) -> Result<Arc<Self>, EvalError>; // JSON
}
impl Model for MockModel { /* pops one script per invoke; exhausted → ModelError */ }
```

`ModelEvent` is deliberately **not** `Serialize`/`Deserialize` in core (and must
not become so in this PR — see §2's registry-verify rule). The JSON file format
therefore uses a serde **mirror enum in evals**, `ScriptEvent` (same five
variants: `TokenDelta`, `ReasoningDelta`, `ToolCallDelta`, `Usage`, `Finish`),
with `From<ScriptEvent> for ModelEvent`. Only the deserialize direction is
needed (scripts are hand-written or generated); recording live runs into script
files is out of scope. The existing per-test-crate `MockModel` copies are left
untouched in this PR (core's tests cannot depend on evals — that would be a
dependency cycle); a follow-up may consolidate the other crates' test doubles.

### 4.4 Trace recorder

```rust
#[async_trait]
pub trait TraceSink: Send + Sync {
    async fn record_case(&self, run: &RunMeta, case: &CaseResult) -> Result<(), TraceError>;
    async fn finish(&self) -> Result<(), TraceError>;   // flush/close
}
```

- **`SqliteTraceSink`** (feature `trace-sqlite`): sqlx SQLite pool, embedded
  migration, following `sessions-sqlite`'s proven shape. Tables: `eval_runs`
  (run id, dataset, started/finished ts), `eval_cases` (run id, case id,
  final_output, per-evaluator scores as JSON), `eval_events` (run id, case id,
  seq, kind, ts_nanos, payload JSON). Events are persisted as the stable,
  already-`Serialize` `SessionEvent` form (derived from the trajectory via
  core's `SessionRecorder`), not as `AgentEvent` (which has no serde).
- **`ParquetTraceSink`** (feature `trace-parquet`): `arrow` + `parquet`
  (Apache-2.0), writing `<dir>/<run_id>-events.parquet` and
  `<dir>/<run_id>-scores.parquet` with flat columnar schemas (run_id, case_id,
  seq, kind, ts_nanos, payload_json / evaluator, value, passed, detail).

Crate features: `default = []`; `trace-sqlite` (sqlx, already a workspace dep);
`trace-parquet` (new heavy deps, strictly opt-in). Everything else (dataset,
evaluators, MockModel) is unconditional. The facade's existing `evals` feature
keeps forwarding to the crate's defaults — lean by design.

## 5. `paigasus-helikon-cli`

### 5.1 Shape

The crate gains an internal lib target (shared by both bins; documented as
"internal — no stability guarantees", `missing_docs` already allowed) and both
bins become thin `fn main()` shims. `Ctx = ()` throughout. Clap (derive) with
subcommands:

```
helikon repl      --agents agents.toml [--agent NAME]
helikon eval run  <dataset.jsonl> --agent NAME --agents agents.toml
                  [--json] [--fail-under 1.0] [--trace sqlite:<path>]
helikon mcp serve --agents agents.toml --agent NAME [--http ADDR]   # default stdio
```

### 5.2 The TOML/Rhai sidecar

One `agents.toml` declares agents, tools, and eval settings:

```toml
[agents.triage]
description  = "Routes personal-finance questions"
instructions = "Classify the user's question and route it."   # or { file = "triage.md" }
model        = { provider = "openai", id = "gpt-5-mini" }
# providers: "openai" | "anthropic" | "mock" (mock: script = "fixtures/triage_script.json")
max_turns    = 8
tools        = ["lookup_spending"]
handoffs     = ["budgeting"]          # names of other agents in this file

[tools.lookup_spending]
description = "Look up spending for a month"
params      = { type = "object", properties = { month = { type = "string" } }, required = ["month"] }
script      = "tools/lookup_spending.rhai"    # or inline = '''fn run(args) { ... }'''

[eval]
evaluators = ["exact_match", "tool_trajectory"]
# [eval.json_schema]  schema = "schemas/answer.json"
# [eval.llm_judge]    model = { provider = "openai", id = "gpt-5-mini" }, rubric = "…", threshold = 0.7
```

- **Model resolution**: `openai`/`anthropic` via the provider crates and their
  env-var keys; `mock` via evals' `MockModel::from_script_file` — which is what
  makes `eval run` (and the repl) fully deterministic in CI.
- **Rhai tools**: a `RhaiTool` (implements `Tool<()>`) with the JSON-Schema
  params from TOML; on invoke, the JSON args map to a Rhai map, the script's
  `fn run(args)` executes on an engine with safety limits (`max_operations`,
  no file/network access — Rhai's default sandbox), and the returned value maps
  back to JSON. Script errors surface as tool errors, not crashes.
- **Handoffs** in TOML build `Handoff::to` chains. (Mutual TOML handoffs create
  `Arc` cycles; acceptable in a short-lived CLI process — documented.)

### 5.3 REPL + hot reload

An `AgentRegistry` owns the parsed definitions and built agents. A `notify`
watcher (recursive on the sidecar file and any referenced `.rhai`/`.md` files)
debounces change events and triggers `registry.reload()`: re-parse, rebuild,
atomically swap. The in-flight turn is unaffected; the **next** turn uses the
new definition — that is the "hot-reloads without restarting" AC. Parse errors
on reload keep the old definitions and print the error (a broken save must not
kill the session). REPL I/O is plain stdin lines + streamed stdout (no
rustyline; deliberate dependency diet), with `/agents`, `/switch NAME`,
`/reload`, `/quit` commands. Conversation state: in-memory session per REPL
process.

### 5.4 `eval run` and `mcp serve`

- `eval run`: loads the sidecar + dataset, builds the named agent, constructs
  evaluators from `[eval]` (per-case applicability handled by the library skip
  rule in §4.1), runs
  `EvalRun`, prints `render_table()` (or `--json`), exits non-zero if
  `report.passed()` is false (or mean score < `--fail-under`). Trajectory +
  final-response scores in one report — the first AC.
- `mcp serve`: wraps the named agent in the existing `McpAgentServer`
  (`with_default_ctx`), `serve_stdio()` by default or
  `serve_streamable_http(addr)` with `--http`.

## 6. Approaches considered

### A. Swarm as sugar over member-embedded Handoffs, weak member slots (chosen)

Reuses the entire existing handoff loop (tool injection, agent switching, span
emission, `max_turns` accounting) — the swarm is wiring, not a second driver.
Weak slots prevent `Arc` leaks. Smallest new surface, exact Notion semantics.

### B. Swarm-level orchestration loop intercepting handoff events (rejected)

A swarm-owned loop that runs members and watches the event stream for handoffs
would duplicate the `LlmAgent` driver (turn accounting, session threading, span
nesting) and drift from it. More core surface, no added capability.

### C. Direct full-mesh `Arc` handoffs (rejected)

Simplest wiring but leaks every swarm via strong reference cycles — unacceptable
for long-running servers that build swarms per request.

### D. GraphAgent compiled to nested Sequential/Parallel (rejected)

Diamonds decompose, but general DAGs (skip-level edges, uneven branch depths)
don't map onto nested Seq/Par without either serializing independent work or
duplicating nodes. A Kahn wavefront is ~100 lines and exact.

### E. Serde on core `ModelEvent` vs mirror enum in evals (mirror chosen)

Deriving serde in core is one line but constitutes new core API consumed by the
ascending evals crate → forces the manual core bump + facade bump ritual and
couples the release. The five-variant mirror (`ScriptEvent`) is trivial,
one-directional, and keeps this PR's release story clean. Core serde derives can
be revisited later independently.

## 7. Testing & quality

- **Core (Swarm)**: unit tests for builder validation; integration tests with
  scripted `MockModel`s (local test-double, as today): 3-member swarm where
  triage hands off and a specialist answers → winner's text is the final
  output, `HandoffOccurred` events present; a non-converging swarm (members
  ping-pong) hits `MaxTurnsExceeded` at `max_turns` — the AC's bound, tested
  from both sides.
- **Core (Graph)**: builder validation incl. cycle detection; diamond topology
  (A→B, A→C, B→D, C→D) asserting dependency gating, concurrent middle layer,
  deterministic synthesized final output; failure propagation (failed node →
  descendants skipped, independent branch completes, aggregate error).
- **Evals**: unit tests per evaluator (incl. `LlmJudge` against a scripted
  `MockModel` returning judge JSON); `EvalRun` end-to-end over a 3-case inline
  dataset with `MockModel` agent; JSONL parse errors; `ScriptEvent` → JSON
  round-trip; `SqliteTraceSink` writes and re-reads rows (tempfile db);
  `ParquetTraceSink` writes files that `parquet` reads back (behind feature).
- **CLI**: sidecar parser unit tests (good/bad TOML, inline vs file Rhai);
  `RhaiTool` invoke + error surfacing + operation-limit; `AgentRegistry` reload
  logic tested directly (rewrite temp file → reload → new instructions in
  effect) plus one `notify`-driven smoke test with a generous timeout (watcher
  events are CI-flake-prone; the logic test is the load-bearing one);
  integration test via `env!("CARGO_BIN_EXE_helikon")` running
  `eval run tests/fixtures/triage.jsonl --agent triage --agents
  tests/fixtures/agents.toml` on a mock-provider agent and asserting scores in
  stdout + exit codes (pass and fail cases) — this is the first AC, in CI.
- **Facade examples**: `swarm_finance.rs` (3-agent swarm, the AC example) and
  `graph_report.rs`, both `required-features = ["openai"]`, registered as
  `[[example]]` with doc comments.
- All existing gates apply: fmt, clippy `-D warnings`, docs `-D warnings`
  (every new pub item documented — evals inherits `missing_docs`), doc-coverage
  ≥ 80 %, `cargo test --workspace --all-features` (the exact gate), msrv 1.94.

## 8. Dependencies

New `[workspace.dependencies]` (exact versions resolved at implementation time —
latest stable, MSRV ≤ 1.94 verified):

| Dep | Used by | Notes |
|---|---|---|
| `clap` (derive) | cli | MIT/Apache-2.0 |
| `toml` | cli | MIT/Apache-2.0 |
| `rhai` | cli | MIT/Apache-2.0; engine limits on by default |
| `notify` | cli | **CC0-1.0** — needs a `deny.toml` license-allowlist addition (conscious call) |
| `jsonschema` | evals | MIT; check transitive tree against `deny.toml` |
| `arrow`, `parquet` | evals (`trace-parquet`) | Apache-2.0; heavy — strictly behind the opt-in feature; **verify MSRV ≤ 1.94** (arrow-rs moves MSRV aggressively; pick the newest release that fits) |

Already available: `sqlx` (0.9, sqlite), `serde`/`serde_json`, `schemars`,
`rmcp` via `-mcp`, `jiff`, `tokio`, `thiserror`, `async-trait`, `futures`,
`tempfile`. New deps must not disturb the aws-lc-rs/rustls provider discipline;
`cargo audit`/`deny` run in CI either way.

Internal: evals → core + runtime-tokio (+ sqlx / arrow+parquet behind
features); cli → core, evals, runtime-tokio, providers-openai,
providers-anthropic, mcp, macros(? only if `#[tool]` is used — likely not),
clap, toml, rhai, notify, tokio, serde_json, anyhow, tracing-subscriber.

## 9. Release engineering

- **`paigasus-helikon-evals`**: standard 4-step stub-ascend to `0.1.0` (bump
  version, drop `publish = false`, drop the `release-plz.toml` block, one
  `chore(release): SMA-333 lift stage-1 gates for paigasus-helikon-evals`
  commit on the branch). Update its `[workspace.dependencies]` pin to `0.1.0`.
- **`paigasus-helikon-cli`**: same 4-step ascend to `0.1.0` (publishes as a
  binary crate; keeps `autobins = false` and both explicit `[[bin]]`s; gains
  `description` etc. via workspace inheritance as needed).
- **No manual core/facade bump**: evals and cli use only already-published core
  API (§2), so their `--verify` passes against the registry. Swarm/Graph land
  as `feat(core)` content; release-plz auto-bumps core (0.x patch per its
  policy) and cascades the facade in the bot release PR — the cascade works
  because release-plz performs the core bump itself.
- **Publish order**: release-plz publishes in dependency order (evals before
  cli) on the ascend PR's merge; core/facade follow via the bot release PR.
  Watch that PR's CI after merge (fresh-advisory rule).
- **Docs**: crate READMEs for evals + cli (real content replacing SMA-304
  stubs), facade README + root README (roster/feature-map: evals + cli now
  real), mdBook pages (`docs/book/`): evals page, CLI page, multi-agent
  concepts page gains Swarm/Graph. `mdbook build docs/book` stays clean.
- **PR title**: `feat(evals): SMA-333 add evals crate, cli subcommands, and
  swarm/graph agents` (type+scope prefix, lowercase subject after `SMA-333`).

## 10. Scope boundaries (YAGNI)

Out of scope, deliberately:

- Recording live runs into MockModel script files (replay only, no capture).
- Serde derives on core `ModelEvent` (mirror enum instead — §6E).
- Consolidating the existing per-test-crate `MockModel` copies onto evals.
- Swarm "terminal members" (`dyn Agent` pool members that can't hand off).
- Conditional/weighted graph edges, per-node retry policies, partial re-runs.
- TOML-declared swarms/graphs in the CLI sidecar (Rust API only this ticket).
- REPL session persistence (`--session` sqlite), rustyline line editing.
- Per-case JSON-schema overrides in `JsonSchemaConformance`.
- An `ollama`/`bedrock`/`gemini` provider in the CLI sidecar (openai,
  anthropic, mock only).
- OTel exporter-based trace capture (the recorder consumes the event stream
  directly; the OTel pipeline stays as-is).

## 11. Resolved questions

- **One PR vs split** → one PR (Sven, 2026-07-05). Implementation plan keeps
  the three workstreams as cleanly separable task groups.
- **CLI publishing** → ascend + publish as a binary crate; "never published as
  a library" is preserved semantically (internal lib, no API promise).
- **REPL sidecar depth** → TOML agent definitions + Rhai-scripted tools; not a
  full Rhai DSL.

## 12. Adversarial review changelog

*(Populated after the Stage 2 spec-challenger pass.)*
