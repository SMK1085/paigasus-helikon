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
   works ("installed by Cargo"). Publishing means its internal lib target is on
   crates.io too; it carries a crate-level "internal — no stability guarantees"
   banner and stays as small as the two bins allow, preserving the "never
   published as a library" policy in the semantic sense.
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

Note on AC3 wording: in the implemented core, `max_turns` is a *per-agent-run*
budget — a handoff nests a fresh child run (fresh turn counter, incremented
`agent_depth`). The swarm's convergence budget is therefore its own
`max_handoffs` bound (plus the underlying `max_agent_depth` hard bound), and
the AC is satisfied as "converges on a winner within the configured swarm
budget"; §3.1 has the details.

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
ritual entirely; §9 spells out the release choreography and its couplings.

## 3. Core: `SwarmAgent<Ctx>` and `GraphAgent<Ctx>`

### 3.1 SwarmAgent — semantics

Per Notion Multi-Agent Patterns: a pool of `LlmAgent` members with **handoff
tools auto-injected full-mesh**; execution is a sequential handoff chain driven
by the existing `LlmAgent` loop (`transfer_to_<name>` tool injection, agent
switching); the swarm **ends when the active member produces a final output**
instead of handing off. "First to produce a final output wins."

**Bounding convergence.** In the implemented handoff machinery
(`agent.rs` / `context.rs`), a handoff *nests*: the parent derives
`ctx.handoff_child()` (incrementing `agent_depth`, checked against
`RunConfig::max_agent_depth`, default 8) and drains the child's stream; each
member's run has its **own** `max_turns` budget (and a runner-supplied
`RunConfig` overrides any per-member `config`, so a builder-level
`.max_turns()` on the swarm would be inert). Consequences:

- A ping-ponging swarm exhausts `max_agent_depth` and fails with
  `AgentError::MaxAgentDepthExceeded` — that is the underlying hard bound.
- The swarm exposes its own **`.max_handoffs(n)`** budget, enforced by the
  swarm itself: its returned stream wraps the entry member's stream and counts
  `AgentEvent::HandoffItem` events; on exceeding the budget it drops the inner
  stream (cancelling the nested run) and emits `RunFailed` with a new
  `AgentError::MaxHandoffsExceeded { limit: u32 }` variant (additive on the
  `#[non_exhaustive]` enum; used only inside core, so no cross-crate
  registry-verify coupling). Default: unset — `max_agent_depth` governs.

### 3.2 SwarmAgent — API (illustrative)

```rust
let swarm = SwarmAgent::builder()
    .name("support_swarm")
    .description("Personal-finance support pool")
    .member(triage)        // LlmAgent<Ctx, M1, T1> — heterogeneous M/T per call
    .member(budgeting)     // LlmAgent<Ctx, M2, T2>
    .member(investing)
    .entry("triage")       // optional; defaults to the first member
    .max_handoffs(6)       // optional swarm hop budget (see §3.1)
    .build()?;             // SwarmBuildError on empty/duplicate/unknown-entry
```

`impl Agent<Ctx> for SwarmAgent<Ctx>`. Build-time validation: at least one
member, unique member names, entry name exists. Errors: `SwarmBuildError {
Empty, DuplicateMember(String), UnknownEntry(String) }` (`thiserror`,
`#[non_exhaustive]`).

**Identity & spans.** `run()` follows the `workflow.rs` composite convention:
the swarm emits its **own** `RunStarted` and workflow span (so trajectories and
OTel attribution name `support_swarm`), swallows the members' nested
`RunStarted`s the same way `SequentialAgent` does, and forwards all other
events from the entry member's (nested) stream through the hop-counting
wrapper described in §3.1.

**Wiring without `Arc` cycles or dangling weaks.** Full-mesh `Handoff`s between
`Arc`'d members would create strong reference cycles (A→B→A) and leak. Design:

- At `.member(agent)` time the builder records the member's `name` and
  `description` as strings (available before type erasure) plus a deferred
  `FnOnce(Vec<Handoff<Ctx>>) -> Arc<dyn Agent<Ctx>>` that injects handoffs
  into the concrete `LlmAgent` (public `handoffs` field — appended to any
  pre-existing handoffs) and then `Arc`s it. This is how heterogeneous
  `LlmAgent<Ctx, M, T>` member types coexist.
- `build()` creates one private **member slot** per member — a tiny
  `Agent<Ctx>` adapter carrying the recorded name/description and a
  `OnceLock<Weak<dyn Agent<Ctx>>>` — wires every member's handoffs with
  `Handoff::shared(slot_j)` for all `j ≠ i`, then fills each slot's weak
  reference with the finished member `Arc`. The slot's `run()` upgrades the
  weak and delegates with `ctx` unchanged — the handoff machinery has already
  derived the child context before invoking the target, so the slot adds no
  extra `agent_depth` level and no event-attribution noise.
- **The returned stream owns the members.** `SwarmAgent::run()` clones the
  strong `Arc`s of *all* members into the returned `'static` stream, because a
  caller may legally drop the `SwarmAgent` before draining
  (`Runner::run_streamed` returns a self-contained `RunResultStreaming`). The
  weak slots exist only to break the member↔member cycle; liveness is
  guaranteed by the stream (while running) and the swarm struct (while held),
  and both owners dropping simply means nothing can poll the stream anymore.

Members are `LlmAgent`s only — they are the only agents that can call transfer
tools. (A `dyn Agent` "terminal member" that can receive but never initiate a
handoff is a noted future extension, not in scope.)

### 3.3 GraphAgent — semantics

A declared DAG: nodes are agents, directed edges are dependencies. A node runs
when **all** its predecessors completed. Scheduling is a Kahn wavefront with
**dynamic readiness**: ready nodes' streams are polled concurrently and
cooperatively (a `FuturesUnordered`/`select_all`-style set that nodes are
*pushed into as their predecessors complete* — this is more bookkeeping than
`ParallelAgent`'s static fan-out, but the same cooperative, non-tokio
concurrency model). Each completed node's final text is recorded under a
session-state key (mirroring `ParallelAgent`'s branch-key convention), and is
appended to each successor's input as a labeled context message.

Termination: all nodes complete → the graph emits one synthesized final
assistant message aggregating the **sink** nodes' outputs (single sink: its text
verbatim; multiple sinks: keyed merge, same convention as `ParallelAgent`).
Always synthesizing the final message keeps `RunResultStreaming::collect()`
deterministic — with concurrent branches, "last assistant message in the
stream" would otherwise depend on completion timing.

Failure: collect-all like `ParallelAgent` — a failed node marks its
**transitive descendants** skipped, independent branches still complete, then
one aggregate `RunFailed` names the failed and skipped nodes. Skipped nodes are
surfaced in that aggregate error detail (and their state keys are simply
absent); **no new `AgentEvent` variant** is added for skips.

Like the swarm, `GraphAgent::run()` emits its own `RunStarted`/workflow span
and swallows child `RunStarted`s, per the `workflow.rs` convention.

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
- `agent.rs` — the one-line `AgentError::MaxHandoffsExceeded` variant addition.
- `lib.rs` re-exports. (`workflow.rs` is already large; new files, same
  conventions: doc comments on every pub item, span emission consistent with
  Sequential/Parallel/Loop.)

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
    pub usage: TokenUsage,         // core's run-level usage type
}
pub enum ScoreOutcome { Passed, Failed, Skipped }
pub struct Score {
    pub value: f64,                // ∈ [0, 1]
    pub outcome: ScoreOutcome,
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
`expected_tools`) returns `Score::skipped(reason)`. `ScoreOutcome::Skipped`
counts toward neither pass/fail nor summary means (an explicit enum, so
`EvalReport::passed()` cannot accidentally treat a skip as a failure);
`EvalSummary` reports skip counts per evaluator so silent no-ops are visible.

Built-in evaluators (`evaluators/` module):

| Evaluator | Kind | Scoring |
|---|---|---|
| `ExactMatch` | final-response | Trimmed string equality; if `expected` is JSON (non-string), structural `serde_json::Value` equality after parsing the output. Options: `case_insensitive()`. 0/1. |
| `JsonSchemaConformance` | final-response | Parses `final_output` as JSON, validates against a constructor-supplied JSON Schema (`jsonschema` crate, draft 2020-12). 0/1; `detail` lists violations. |
| `LlmJudge` | final-response | Wraps an `Arc<dyn Model>` + rubric prompt; sends input/expected/actual, asks for `{"score": 0..1, "reasoning": "…"}`; lenient JSON extraction; passed = score ≥ threshold (default 0.7). No tools — a bare model call, not a full agent. |
| `ToolUseTrajectory` | trajectory | Extracts the tool-call name sequence from `events`, **filtering out `transfer_to_*` handoff tool calls by default** (the handoff loop emits them as real `ToolCallItem`s before recognizing the transfer; counting them would pollute swarm/handoff evals — a `include_handoffs()` option re-enables them). Modes `exact()` (default) and `in_order()` (subsequence). Score = matched fraction; passed at 1.0; `detail` shows expected vs actual. |

### 4.2 EvalRun

```rust
let report = EvalRun::builder()
    .dataset(dataset)
    .agent_factory(|case| build_agent_for(case))  // fresh agent per case — see below
    // or .agent(agent) for a shared stateless agent
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

**Determinism rule:** `MockModel` is stateful (it pops one script per
`invoke`), so a *shared* mock-backed agent across N cases is order-dependent
and racy under concurrency. Mock-backed runs must use `.agent_factory` — a
fresh agent + `MockModel` per case, scripts selected per case id (§4.3/§5.2).
`.agent(...)` remains for genuinely stateless/live agents. This is what makes
"deterministic scores in CI" (AC1) actually hold.

`EvalReport { dataset, results: Vec<CaseResult>, summary: EvalSummary }` where
`CaseResult` carries the outcome plus per-evaluator `Score`s and `EvalSummary`
aggregates per-evaluator mean score, pass rate, and skip count.
`EvalReport::passed()` is true iff no evaluator on any case yielded
`ScoreOutcome::Failed`. `EvalReport` is `Serialize` (JSON output for CI) and
has a plain-text `render_table()` for terminals.

### 4.3 MockModel + recorded scripts

Promote the test-double that already exists in
`crates/paigasus-helikon-core/tests/common/mod.rs` into a public, documented
type:

```rust
pub struct MockModel { /* Mutex<VecDeque<Vec<ModelEvent>>> */ }
impl MockModel {
    pub fn with_script(script: Vec<ModelEvent>) -> Arc<Self>;
    pub fn with_scripts(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self>; // one per invoke()
    pub fn from_script_file(path: &Path) -> Result<Arc<Self>, EvalError>; // JSON, "default" entry
}
impl Model for MockModel { /* pops one script per invoke; exhausted → ModelError */ }
```

`ModelEvent` is deliberately **not** `Serialize`/`Deserialize` in core (and must
not become so in this PR — see §2's registry-verify rule). The JSON file format
therefore uses serde **mirror types in evals**: `ScriptEvent` (the five
`ModelEvent` variants: `TokenDelta`, `ReasoningDelta`, `ToolCallDelta`,
`Usage`, `Finish`) **and `ScriptFinishReason`** (mirroring the nested
`FinishReason`: `Stop`, `Length`, `ToolCalls`, `ContentFilter`,
`Other(String)`), each with `From<…>` into the core type. Only the deserialize
direction is needed (scripts are hand-written or generated); recording live
runs into script files is out of scope.

Script files support per-case script sets for deterministic multi-case eval
(consumed via `EvalRun`'s `agent_factory`):

```json
{
  "default": [ [ …ScriptEvents per invoke… ], … ],
  "cases":   { "case-1": [ [ … ], … ] }
}
```

The existing per-test-crate `MockModel` copies are left untouched in this PR
(core's tests cannot depend on evals — that would be a dependency cycle); a
follow-up may consolidate the other crates' test doubles.

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
  seq, kind, ts_nanos, payload JSON). Events are persisted in the
  **`SessionEvent`** form (derived from the trajectory via core's public
  `SessionRecorder`) because it is the canonical, stable, audit-grade shape
  shared with the session backends — `AgentEvent` does derive serde, but it is
  the UI-stream shape (deltas and all) and a poor analytics schema.
- **`ParquetTraceSink`** (feature `trace-parquet`): `arrow` + `parquet`
  (Apache-2.0), writing `<dir>/<run_id>-events.parquet` and
  `<dir>/<run_id>-scores.parquet` with flat columnar schemas (run_id, case_id,
  seq, kind, ts_nanos, payload_json / evaluator, value, outcome, detail).

Crate features: `default = []`; `trace-sqlite` (sqlx, already a workspace dep);
`trace-parquet` (new heavy deps, opt-in). Everything else (dataset, evaluators,
MockModel) is unconditional. The facade's existing `evals` feature keeps
forwarding to the crate's defaults. **CI-surface caveat (conscious call):** the
canonical gates run `--all-features` (test matrix, docs, deny), so arrow/parquet
compile in every required CI run and join the audited dependency tree
regardless of the opt-in gating for *users*. Accepted: the ticket mandates
Parquet, and carving features out of the exact-gate `--all-features` runs has
bitten before. Consequence: the chosen arrow/parquet release must hold MSRV ≤
1.94 and stay `deny`-clean.

## 5. `paigasus-helikon-cli`

### 5.1 Shape

The crate gains an internal lib target (shared by both bins; crate-level docs
say "internal — no stability guarantees"; `missing_docs` already allowed) and
both bins become thin `fn main()` shims. `Ctx = ()` throughout. Clap (derive)
with subcommands:

```
helikon repl      [--agents agents.toml] [--agent NAME]
helikon eval run  <dataset.jsonl> --agent NAME [--agents agents.toml]
                  [--json] [--fail-under 1.0] [--trace sqlite:<path>]
helikon mcp serve --agent NAME [--agents agents.toml] [--http ADDR]   # default stdio
```

`--agents` defaults to `./agents.toml` so the AC's verbatim command
(`helikon eval run datasets/triage.jsonl --agent triage`) works from a project
root containing the sidecar.

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

- **Runtime model selection — `CliModel`.** `LlmAgent<Ctx, M, T>` is generic
  over a *concrete* `M: Model` and core has no `impl Model for Arc<dyn Model>`,
  so a TOML-driven provider choice needs a homogenizing model type. The CLI
  defines one internally:
  ```rust
  enum CliModel { OpenAi(OpenAiModel), Anthropic(AnthropicModel), Mock(Arc<MockModel>) }
  impl Model for CliModel { /* delegates invoke/capabilities/provider/model */ }
  ```
  Every sidecar agent is an `LlmAgent<(), CliModel>` — one concrete type, so
  the registry, handoffs, eval, and `mcp serve` all type-check. `openai`/
  `anthropic` resolve env-var keys; `mock` loads a script file via evals —
  which is what makes `eval run` (and the repl) fully deterministic in CI. For
  `eval run` with a mock provider, the CLI uses `EvalRun::agent_factory` and
  the script file's per-case `cases` map (§4.3/§4.2 determinism rule).
- **Rhai tools**: a `RhaiTool` (implements `Tool<()>`) with the JSON-Schema
  params from TOML. Rhai is compiled with its **`sync` feature** (engine/AST
  must be `Send + Sync` to live in a `Tool`), and each invocation runs the
  script's `fn run(args)` under **`tokio::task::spawn_blocking`** (the engine
  is synchronous; blocking a runtime worker is not acceptable) on an engine
  with safety limits (`max_operations`, no file/network access — Rhai's
  default sandbox). JSON args map to a Rhai map and the returned value maps
  back to JSON; script errors surface as tool errors, not crashes.
- **Handoffs** in TOML build `Handoff::to` chains. (Mutual TOML handoffs create
  `Arc` cycles; acceptable in a short-lived CLI process — documented.)

### 5.3 REPL + hot reload

An `AgentRegistry` owns the parsed definitions and built agents. A `notify`
watcher wrapped in **`notify-debouncer-mini`** (raw notify events need
debouncing; watching the sidecar and any referenced `.rhai`/`.md` files)
triggers `registry.reload()`: re-parse, rebuild, atomically swap. The in-flight
turn is unaffected; the **next** turn uses the new definition — that is the
"hot-reloads without restarting" AC. Parse errors on reload keep the old
definitions and print the error (a broken save must not kill the session).
REPL I/O is plain stdin lines + streamed stdout (no rustyline; deliberate
dependency diet), with `/agents`, `/switch NAME`, `/reload`, `/quit` commands.
Conversation state: in-memory session per REPL process.

### 5.4 `eval run` and `mcp serve`

- `eval run`: loads the sidecar + dataset, builds the named agent (per-case
  factory for mock providers), constructs evaluators from `[eval]` (per-case
  applicability handled by the library skip rule in §4.1), runs `EvalRun`,
  prints `render_table()` (or `--json`), exits non-zero if `report.passed()`
  is false (or mean score < `--fail-under`). Trajectory + final-response
  scores in one report — the first AC.
- `mcp serve`: builds a fresh owned `LlmAgent<(), CliModel>` from the parsed
  definition and hands it to the existing `McpAgentServer::with_default_ctx`
  **by value** (it takes `impl Agent<Ctx>`; there is no `impl Agent for
  Arc<dyn Agent>`, so serving from the registry's shared `Arc` would need a
  wrapper — building fresh is simpler). `serve_stdio()` by default,
  `serve_streamable_http(addr)` with `--http`.

## 6. Approaches considered

### A. Swarm as sugar over member-embedded Handoffs, weak member slots (chosen)

Reuses the entire existing handoff loop (tool injection, agent switching, span
emission, depth accounting) — the swarm is wiring plus a thin hop-counting
stream wrapper, not a second driver. Weak slots prevent `Arc` leaks; the
returned stream holds the strong `Arc`s (§3.2). Smallest new surface, exact
Notion semantics.

### B. Swarm-level orchestration loop intercepting handoff events (rejected)

A swarm-owned loop that runs members and watches the event stream for handoffs
would duplicate the `LlmAgent` driver (turn accounting, session threading, span
nesting) and drift from it. More core surface, no added capability. (The chosen
design's stream wrapper only *counts* handoffs and enforces the swarm budget —
it does not re-drive execution.)

### C. Direct full-mesh `Arc` handoffs (rejected)

Simplest wiring but leaks every swarm via strong reference cycles — unacceptable
for long-running servers that build swarms per request.

### D. GraphAgent compiled to nested Sequential/Parallel (rejected)

Diamonds decompose, but general DAGs (skip-level edges, uneven branch depths)
don't map onto nested Seq/Par without either serializing independent work or
duplicating nodes. A dynamic Kahn wavefront is the exact semantics; it is more
scheduling bookkeeping than `ParallelAgent`, and §3.3 scopes it honestly.

### E. Serde on core `ModelEvent` vs mirror enum in evals (mirror chosen)

Deriving serde in core is one line but constitutes new core API consumed by the
ascending evals crate → forces the manual core bump + facade bump ritual and
couples the release. The mirror (`ScriptEvent` + `ScriptFinishReason`) is
small, one-directional, and keeps this PR's release story clean. Core serde
derives can be revisited later independently.

## 7. Testing & quality

- **Core (Swarm)**: unit tests for builder validation; integration tests with
  scripted `MockModel`s (local test-double, as today): 3-member swarm where
  triage hands off and a specialist answers → winner's text is the final
  output, swarm-attributed `RunStarted` first, `HandoffItem` events present;
  a ping-ponging swarm with `.max_handoffs(n)` fails with
  `MaxHandoffsExceeded` at exactly n hops; without a swarm budget, the same
  ping-pong hits core's `MaxAgentDepthExceeded` — the convergence bound tested
  from both sides; dropping the `SwarmAgent` before draining the stream still
  completes (stream owns the members).
- **Core (Graph)**: builder validation incl. cycle detection; diamond topology
  (A→B, A→C, B→D, C→D) asserting dependency gating, concurrent middle layer,
  deterministic synthesized final output; failure propagation (failed node →
  transitive descendants skipped and named in the aggregate error, independent
  branch completes).
- **Evals**: unit tests per evaluator (incl. `LlmJudge` against a scripted
  `MockModel` returning judge JSON, and `ToolUseTrajectory`'s `transfer_to_*`
  filtering); `EvalRun` end-to-end over a 3-case inline dataset with per-case
  `agent_factory` mocks — asserting stable scores across repeated runs and
  `concurrency(4)`; JSONL parse errors; `ScriptEvent`/`ScriptFinishReason` JSON
  round-trip; skip semantics (absent `expected_tools` → Skipped, not Failed);
  `SqliteTraceSink` writes and re-reads rows (tempfile db); `ParquetTraceSink`
  writes files that `parquet` reads back (behind feature).
- **CLI**: sidecar parser unit tests (good/bad TOML, inline vs file Rhai);
  `RhaiTool` invoke + error surfacing + operation-limit; `AgentRegistry` reload
  logic tested directly (rewrite temp file → reload → new instructions in
  effect) plus one debouncer-driven smoke test with a generous timeout (watcher
  events are CI-flake-prone; the logic test is the load-bearing one);
  integration test via `env!("CARGO_BIN_EXE_helikon")` running
  `eval run <fixtures>/triage.jsonl --agent triage` with cwd at the fixture dir
  (exercising the `./agents.toml` default — the AC's verbatim shape) on a
  mock-provider agent, asserting trajectory + final-response scores in stdout
  and exit codes (pass and fail cases) — this is the first AC, in CI.
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
| `rhai` (`sync` feature) | cli | MIT/Apache-2.0; engine limits on by default; invoked via `spawn_blocking` |
| `notify` + `notify-debouncer-mini` | cli | **CC0-1.0** — needs a `deny.toml` license-allowlist addition; CC0's patent non-grant makes this a policy call, flagged for Sven at GATE 1 |
| `jsonschema` | evals | MIT; check transitive tree against `deny.toml` |
| `arrow`, `parquet` | evals (`trace-parquet`) | Apache-2.0; compiled in every `--all-features` CI run (§4.4 conscious call); **verify MSRV ≤ 1.94** (arrow-rs moves MSRV aggressively; pick the newest release that fits) |

Already available: `sqlx` (0.9, sqlite), `serde`/`serde_json`, `schemars`,
`rmcp` via `-mcp`, `jiff`, `tokio`, `thiserror`, `async-trait`, `futures`,
`tempfile`. New deps must not disturb the aws-lc-rs/rustls provider discipline;
`cargo audit`/`deny` run in CI either way.

Internal: evals → core + runtime-tokio (+ sqlx / arrow+parquet behind
features); cli → core, evals, runtime-tokio, providers-openai,
providers-anthropic, mcp, clap, toml, rhai, notify(+debouncer), tokio,
serde_json, anyhow, tracing-subscriber.

## 9. Release engineering

- **`paigasus-helikon-evals`**: standard 4-step stub-ascend to `0.1.0` (bump
  version, drop `publish = false`, drop the `release-plz.toml` block, one
  `chore(release): SMA-333 lift stage-1 gates for paigasus-helikon-evals`
  commit on the branch). Update its `[workspace.dependencies]` pin to `0.1.0`.
- **`paigasus-helikon-cli`**: same 4-step ascend to `0.1.0`, publishing as a
  binary crate (keeps `autobins = false` and both explicit `[[bin]]`s). Its
  internal lib target becomes crates.io-visible; the crate-level "internal, no
  stability guarantees" banner is the mitigation (§5.1) — a conscious reversal
  of the standing `publish = false`, decided at intake.
- **No manual core/facade bump — with its couplings stated.** evals and cli
  use only already-published core API (§2), so their `--verify` passes against
  the registry. Swarm/Graph land as `feat(core)` content; release-plz
  auto-bumps core (0.x patch per its policy) in the bot release PR and — 
  because release-plz performs that bump itself — `dependencies_update`
  cascades a facade patch bump whose republish also refreshes the facade's
  `evals = ^0.1.0` requirement. **The facade un-stranding therefore rides on
  the coincident core bump.** (In a future PR that ascends a stub with *no*
  core change, the facade would strand exactly per CLAUDE.md's second-order
  caveat and need a manual facade bump; not this PR's problem, but stated so
  nobody copies this section blindly.) Contingency if the cascade misses: a
  one-line `chore(release)` facade patch PR, the known play from PR #50.
- **Two interdependent ascends in one pass — first time.** cli depends on
  evals `0.1.0`; both publish on the ascend PR's merge. release-plz publishes
  in dependency order (evals → cli), but this is the first two-crate
  simultaneous ascend in this repo. Contingency if cli's publish/verify runs
  before or without evals: evals is already on crates.io at that point, so
  re-running the release job (or a manual `cargo publish -p paigasus-helikon-cli`)
  completes the pair — no yanking involved. Watch the release run after merge
  (standing rule), plus the bot release PR's CI (fresh-advisory rule).
- **Docs**: crate READMEs for evals + cli (real content replacing SMA-304
  stubs), facade README + root README (roster/feature-map: evals + cli now
  real), mdBook pages (`docs/book/`): evals page, CLI page, multi-agent
  concepts page gains Swarm/Graph. `mdbook build docs/book` stays clean.
- **PR title**: `feat(evals): SMA-333 add evals crate, cli subcommands, and
  swarm/graph agents` (type+scope prefix, lowercase subject after `SMA-333`).

## 10. Scope boundaries (YAGNI)

Out of scope, deliberately:

- Recording live runs into MockModel script files (replay only, no capture).
- Serde derives on core `ModelEvent`/`FinishReason` (mirror types instead — §6E).
- Consolidating the existing per-test-crate `MockModel` copies onto evals.
- Swarm "terminal members" (`dyn Agent` pool members that can't hand off).
- New `AgentEvent` variants (graph skips ride the aggregate error; §3.3).
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
  a library" is preserved semantically (internal lib, no API promise, unstable
  banner).
- **REPL sidecar depth** → TOML agent definitions + Rhai-scripted tools; not a
  full Rhai DSL.

## 12. Adversarial review changelog

Stage 2 spec-challenger (Opus) verdict on the initial draft: **NEEDS REWORK**
(2 blockers, 6 majors, 7 minors, 3 questions). Disposition:

**Folded in (justified):**

- BLOCKER — swarm convergence is bounded by `max_agent_depth`, not a shared
  `max_turns` (handoffs nest with fresh turn budgets; runner `RunConfig`
  overrides member config, verified in `agent.rs`). → §1 AC note, §3.1
  rewritten around `.max_handoffs` + `MaxHandoffsExceeded`; `.max_turns`
  dropped from the swarm builder; §7 tests rewritten.
- BLOCKER — weak-slot members must outlive the returned `'static` stream. →
  §3.2: the stream owns strong `Arc`s of all members; slots only break cycles.
- MAJOR — runtime provider selection needs a named homogenizing model type. →
  §5.2 `CliModel` enum; registry/eval/serve all use `LlmAgent<(), CliModel>`.
- MAJOR — shared stateful `MockModel` across cases is non-deterministic. →
  §4.2 `agent_factory` + determinism rule; §4.3 per-case script-file format.
- MAJOR — `AgentEvent` *does* derive serde (rationale was false) and
  `CaseOutcome.usage` type is `TokenUsage`. → §4.4 rationale corrected
  (SessionEvent = canonical audit shape, chosen not forced); §4.1 field fixed.
- MAJOR — swarm must emit its own `RunStarted`/span per workflow convention. →
  §3.2 "Identity & spans"; graph gets the same treatment (§3.3).
- MAJOR — `--all-features` CI runs compile arrow/parquet regardless of the
  user-facing opt-in. → §4.4/§8: stated as a conscious, accepted cost.
- MAJOR — two interdependent ascends + cli publish-policy reversal. → §9:
  ordering contingency documented; published-internal-lib acknowledged.
- MINOR ×7 — `ScriptFinishReason` mirror (§4.3); `transfer_to_*` filtering in
  `ToolUseTrajectory` (§4.1); `--agents` default so the verbatim AC command
  works (§5.1, §7); Rhai `sync` + `spawn_blocking` (§5.2);
  `notify-debouncer-mini` + CC0 policy flag (§5.3, §8); `Score` skip modeled
  as an explicit `ScoreOutcome` enum (§4.1); `mcp serve` takes a fresh owned
  agent by value (§5.4).
- QUESTIONS — slot name/description capture + `OnceLock` fill + no extra depth
  (§3.2); graph scheduling scoped honestly, skips surfaced without new
  `AgentEvent` variants (§3.3, §10); facade-cascade coupling stated (§9).

**Rejected (with reasons):**

- "Consider excluding `trace-parquet` from the `--all-features` CI surface" —
  rejected: deviating from the exact canonical gate has caused real breakage
  before (the `--all-features` dual-CryptoProvider incident); we accept the
  arrow build cost instead.
- "Consider splitting into evals-first / cli-second PRs" — rejected: Sven
  chose one PR at intake; the ordering contingency in §9 covers the risk
  without splitting.
