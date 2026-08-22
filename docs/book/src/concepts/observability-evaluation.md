# Observability & Evaluation

Two separable concerns share this chapter. **Observability** is shipped:
the agent loop emits OpenTelemetry-compatible spans following GenAI semantic
conventions, and you bring your own collector. **Evaluation** is shipped
too, in `paigasus-helikon-evals`: JSONL datasets, an `Evaluator` trait with
four built-ins, a `MockModel` for deterministic replay, and SQLite/Parquet
trace sinks for offline analysis.

## Observability

Helikon does not embed a tracing backend. The agent loop emits spans through the
`tracing` crate; you choose the exporter, collector, and dashboard. This is the
"bring your own observability stack" stance — wire the spans into whatever OTel
pipeline you already run (Langfuse, Jaeger, Honeycomb, an OTLP collector, or a
plain `fmt` subscriber for local debugging).

### `TracerHandle` — per-run trace attributes

`TracerHandle` (re-exported as `paigasus_helikon::core::TracerHandle`) is the
carrier for run-scoped trace attributes that the loop stamps onto the run and
turn spans. It holds three optional Langfuse-flavored fields: a `session_id`, a
`user_id`, and a list of `tags`.

An empty handle comes from `TracerHandle::default()`; a populated one is built
through `TracerHandle::builder()`, which returns a `TracerHandleBuilder`:

```rust
use paigasus_helikon::core::TracerHandle;

let tracer = TracerHandle::builder()
    .with_session_id("demo-session")
    .with_user_id("demo-user")
    .with_tag("example")
    .with_tag("prod")
    .build();

assert_eq!(tracer.session_id(), Some("demo-session"));
assert_eq!(tracer.user_id(), Some("demo-user"));
assert_eq!(tracer.tags(), &["example", "prod"]);
```

The handle is passed to `RunContext` via `.with_tracer(tracer)` — or use
`RunContext::ephemeral(()).with_tracer(tracer)` when you want all other defaults.
The loop reads it back via `RunContext::tracer` and emits the configured
`session.id`, `user.id`, and `tags` onto the trace. `TracerHandleBuilder` is a
consuming builder — its `with_*` methods take and return `self`.

### Exporting to an OTel backend

Spans flow through `tracing`, so any `tracing-subscriber` layer collects them.
The `langfuse_tracing` example
(`crates/paigasus-helikon/examples/langfuse_tracing.rs`, run with the
`runtime-tokio` feature) shows the full path: build an OTLP `SpanExporter`,
install it as a `tracing-opentelemetry` layer, then run the agent through
`TokioRunner` so the run/turn/tool spans land in Langfuse.

The wiring (subscriber setup, abridged from the example):

```rust
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::trace::{BatchSpanProcessor, SdkTracerProvider};
use tracing_subscriber::prelude::*;

let otlp = opentelemetry_otlp::SpanExporter::builder()
    .with_http()
    .with_endpoint(format!("{host}/api/public/otel/v1/traces"))
    .with_headers(std::collections::HashMap::from([(
        "Authorization".to_string(),
        format!("Basic {auth}"),
    )]))
    .build()?;

let provider = SdkTracerProvider::builder()
    .with_span_processor(BatchSpanProcessor::builder(otlp).build())
    .build();
let tracer = provider.tracer("paigasus-helikon");

tracing_subscriber::registry()
    .with(tracing_opentelemetry::layer().with_tracer(tracer))
    .init();
```

With the subscriber installed, the run produces the trace tree
`invoke_agent → agent.turn → chat / execute_tool`, with token counts on the
`chat` observation and the `session.id` / `user.id` / `tags` from the
`TracerHandle` on the trace. The `opentelemetry*`, `tracing-opentelemetry`, and
`tracing-subscriber` crates are the user's choice — they are not Helikon
dependencies, which keeps `paigasus-helikon-core` `tracing`-only and lets you
swap in any exporter.

The example's `runtime-tokio` feature pulls in `TokioRunner`, which installs the
`TracerHandle` on the run context for you. See
[the agent loop](./agent-loop.md) for how the runner drives a run, and
[crates reference](../reference/crates.md) for what each crate ships.

### Filtering by target

Every `tracing` event and span carries a **target**. Helikon's targets come from
two namespaces:

- **`paigasus::<component>::<subsystem>`** — hand-chosen targets, written
  explicitly at the call site and independent of the Rust module the code lives
  in. Today these are the five model providers, plus one call site in
  `paigasus-helikon-runtime-temporal`.
- **`paigasus_helikon_*::…`** — ordinary Rust module paths, the `tracing`
  default. Nearly everything in `paigasus-helikon-core` and the runtime crates
  emits here.

**`EnvFilter` matches a directive against a target by raw string prefix, not by
`::` segment.** That one fact decides every recipe below:

| Directive | Reaches |
| --- | --- |
| `paigasus` | **Both** namespaces — it is a raw prefix, so it also matches `paigasus_helikon_core::session`. |
| `paigasus::` | The hand-chosen namespace only. The trailing `::` is what excludes the module paths. |
| `paigasus::openai` | One component. |
| `paigasus::openai::chat` | One subsystem. |

<!-- tracing-components:start — keep in sync; asserted by
     tests/workspace-lints/tests/tracing_target_docs.rs -->

| Component | Crate | Subsystems today | Status |
| --- | --- | --- | --- |
| `paigasus::openai` | `paigasus-helikon-providers-openai` | `translate`, `chat`, `responses` | stable |
| `paigasus::anthropic` | `paigasus-helikon-providers-anthropic` | `translate`, `stream`, `sse` | stable |
| `paigasus::bedrock` | `paigasus-helikon-providers-bedrock` | `translate`, `stream`, `builder` | stable |
| `paigasus::gemini` | `paigasus-helikon-providers-gemini` | `translate`, `sse` | stable |
| `paigasus::litellm` | `paigasus-helikon-providers-litellm` | `translate`, `stream`, `sse`, `http` | stable |
| `paigasus::temporal` | `paigasus-helikon-runtime-temporal` | `activities` | provisional |

<!-- tracing-components:end -->

The **Subsystems today** column lists what exists at the time of writing, not a
fixed set — see the stability rules below.

#### What is not in this namespace

`paigasus-helikon-core` and the runtime crates do **not** use hand-chosen
targets, with the single exception of `paigasus::temporal::activities` noted
below. Their events and spans land on module paths, so you select them by
crate:

```
paigasus_helikon_core
paigasus_helikon_runtime_axum
paigasus_helikon_runtime_actix
paigasus_helikon_runtime_agentcore
paigasus_helikon_runtime_temporal   # all but one site; see paigasus::temporal below
paigasus_helikon_runtime_tokio
```

This includes **the run/turn/chat trace tree described above** — the
`agent.run`, `agent.turn`, `gen_ai.chat` and `tool.execute` spans come from
`paigasus_helikon_core`, not from `paigasus::*`. (The `invoke_agent` /
`agent.turn` / `chat` / `execute_tool` operation names used above are the
`gen_ai.operation.name` fields set on these same spans.) Most are raised in
`paigasus_helikon_core::agent`; the multi-agent constructs — the sequential,
parallel and loop workflows, plus the graph and swarm agents — raise their own
`agent.run` span in `paigasus_helikon_core::workflow`. Filter on
`paigasus_helikon_core` to catch both: a narrower `paigasus_helikon_core::agent`
silently misses a multi-agent run's top-level span.

`paigasus::temporal` is a single call site in a crate that is otherwise
untargeted, which is why it is marked *provisional* above: it is listed so the
namespace is completely described, but it does not carry the guarantee the
provider components do. Whether the core and runtime crates should adopt
hand-chosen targets is tracked as a follow-up.

#### Stability

The namespace is a two-tier contract.

- **`paigasus::` and `paigasus::<component>` are stable**, for every component
  the table above marks *stable*. Renaming or removing one is a breaking change,
  made through a commit carrying a `BREAKING CHANGE:` footer so it appears in the
  crate's CHANGELOG. A component marked *provisional* carries no such promise and
  may be renamed or removed in any release.
- **No component name will ever be a prefix of another.** This one is
  namespace-wide and binds *provisional* components exactly as much as stable
  ones — it is not part of the guarantee above. A collision would silently widen
  a filter that is already deployed, since matching is prefix-based, and a new
  component's status is no comfort to an operator whose alert quietly started
  matching more than it did yesterday.
- **The `::<subsystem>` leaf is an implementation detail** and may change in any
  release without notice.

So: use **exactly two segments** for anything durable — alerting rules,
dashboards, saved queries. Use three segments for interactive debugging, and
expect them to move. A bare `paigasus` is a raw prefix, not a namespace
selector; reach for `paigasus::` when you mean the curated targets.

This guarantee begins with this document and is not retroactive.

#### Recipes

Warnings everywhere, one provider verbose:

```
RUST_LOG='warn,paigasus::openai=debug'
```

The hand-chosen namespace only, excluding core and runtime module paths — note
the trailing `::`:

```
RUST_LOG='warn,paigasus::=debug'
```

One subsystem and nothing else. This is a three-segment selector, so treat it as
a debugging tool: the `stream` leaf may be renamed in any release, and if this
example ever stops matching, that is why.

```
RUST_LOG='off,paigasus::litellm::stream=trace'
```

The agent trace tree — the `agent.run` / `agent.turn` / `gen_ai.chat` /
`tool.execute` spans:

```
RUST_LOG='warn,paigasus_helikon_core=debug'
```

These set the level for a `tracing-subscriber` `EnvFilter`; see
[`tracing_subscriber::EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
for the full directive grammar.

## Evaluation

`paigasus-helikon-evals` runs a dataset of cases through an agent, scores
each case's outcome with one or more evaluators, and aggregates the results
into a report — in CI (`helikon eval run`) or from an integration test.

### `EvalDataset` — JSONL cases

`EvalDataset::from_jsonl_path`/`from_jsonl_str` load one `EvalCase` per line
(blank lines skipped): an `input` (the user-turn text), an optional
`expected` value (string or JSON, for final-response comparison), an
optional `expected_tools` (tool-call names in order, for trajectory
comparison), and free-form `metadata`. A case without an explicit `id` gets
`case-<line#>`.

### `Evaluator` and the four built-ins

```rust
#[async_trait::async_trait]
pub trait Evaluator: Send + Sync {
    fn name(&self) -> &str;
    async fn evaluate(&self, case: &EvalCase, outcome: &CaseOutcome) -> Result<Score, EvalError>;
}
```

Each `Score` is a value in `[0, 1]` plus a `ScoreOutcome` of `Passed`,
`Failed`, or `Skipped`. **Skipped is a distinct outcome, not a failure** — an
evaluator whose required case field is absent skips rather than fails, and
skips count toward neither pass/fail nor the summary mean, so a
misconfigured dataset (e.g. no `expected_tools` anywhere) shows up as a
visible skip count instead of a silent no-op.

| Evaluator | Scores | Skips when |
| --- | --- | --- |
| `ExactMatch` | Trimmed string equality against `expected` (optionally case-insensitive via `.case_insensitive()`); structural JSON equality when `expected` is non-string JSON. | `expected` is absent. |
| `JsonSchemaConformance` | Parses the final output as JSON and validates it against a constructor-supplied JSON Schema (draft 2020-12). | Never — independent of the case. |
| `LlmJudge` | Wraps an `Arc<dyn Model>` + rubric; asks for `{"score": 0..1, "reasoning": "…"}` and passes at or above a threshold (default `0.7`). Unparseable judge output fails (`0.0`) rather than erroring the run. | Never — independent of the case. |
| `ToolUseTrajectory` | Extracts the observed tool-call name sequence and compares it to `expected_tools`, `.exact()` (position-for-position) or `.in_order()` (subsequence); `transfer_to_*` handoff calls are filtered out by default (`.include_handoffs()` re-enables them). | `expected_tools` is absent. |

### `MockModel` and `ScriptFile` — deterministic replay

`MockModel` is a scripted `Model`: `with_script`/`with_scripts` hand it one
`Vec<ModelEvent>` per `invoke` call, popped in order; running out yields a
`ModelError` rather than looping. `ScriptFile::load` (and
`MockModel::from_script_file`) parse a JSON file of serde mirror types
(`ScriptEvent`/`ScriptFinishReason` — core's own `ModelEvent`/`FinishReason`
deliberately don't derive serde) with a `"default"` script set plus an
optional `"cases"` map keyed by case id, so one file can drive a whole
dataset deterministically.

`MockModel` is **stateful** (it pops from an internal queue), so sharing one
mock-backed agent across cases is order-dependent under concurrency. Use
`EvalRun::agent_factory` — build a fresh agent (and fresh `MockModel`) per
case, selecting scripts by `case.id` — whenever the model is a mock;
`.agent()`/`.shared_agent()` remain for genuinely stateless or live agents.

`MockModel` honors its `CancellationToken` as the `Model::invoke` contract
requires: the stream observes the token at each poll and ends on the first
fired observation, without emitting `Finish`. The token is *observed*, not
awaited — a consumer that stops polling never learns the stream has ended,
which is all a synchronous scripted stream can offer. An `invoke` called with
an already-cancelled token yields an empty stream but still consumes its
script, so "one script per `invoke`" holds regardless of cancellation timing.

Cancellation is a *model*-boundary event, not a run-level one: the loop treats
the truncated turn as a complete turn. If the cut lands mid-JSON, `build_items`
fails to parse the partial `args_delta` and the run fails with `invalid tool
args for call_id=…` — matching what a real provider does when a connection
drops mid-call, now reproducible deterministically. If the cut lands on a
syntactically complete (or empty, hence normalized to `{}`) argument string,
the tool call is well-formed and **the loop executes it**. Neither is specific
to `MockModel`; both are what a real provider produces from the same cut.

### `EvalRun`

```rust,ignore
let report = EvalRun::builder()
    .dataset(EvalDataset::from_jsonl_path(Path::new("triage.jsonl"))?)
    .agent_factory(|case| build_agent_for(&case.id))  // fresh agent per case — see above
    .default_ctx()                      // or .ctx_factory(...) for a non-Default Ctx
    .evaluator(ExactMatch::new())
    .evaluator(ToolUseTrajectory::exact())
    .concurrency(4)                     // default 1 (sequential, deterministic order)
    .run()
    .await?;

assert!(report.passed());
println!("{}", report.render_table());
```

Each case runs on a fresh ephemeral `RunContext` (fresh in-memory session)
through `TokioRunner` by default (override with `.runner(...)` for another
execution backend). Results come back **in dataset order regardless of
`concurrency`** — `EvalRun` re-sorts by original index after the concurrent
buffer drains. `EvalReport` is `Serialize` (for `--json` output) and has a
plain-text `render_table()` for terminals; `EvalReport::passed()` is true iff
no case failed, where a case fails on any evaluator yielding `Failed` **or**
a run-level error (the agent run itself failed before evaluators ran).

### Trace sinks

`TraceSink` records every case's result once the whole run completes, in
dataset order (not progressively as each case finishes — cases run
concurrently and `EvalRun` re-sorts by original index before any recording
starts) — feature-gated so the crate stays lean by default:

- **`SqliteTraceSink`** (feature `trace-sqlite`) writes `eval_runs` /
  `eval_cases` / `eval_events` tables via an embedded sqlx migration. Events
  are persisted in the canonical `SessionEvent` form (via core's
  `SessionRecorder`), not the raw `AgentEvent` UI-stream shape — a stabler,
  audit-grade schema shared with the session backends.
- **`ParquetTraceSink`** (feature `trace-parquet`) writes
  `<dir>/<run_id>-events.parquet` and `<dir>/<run_id>-scores.parquet` with
  flat columnar schemas, for offline analysis with any Parquet-reading tool.

### `helikon eval run`

The CLI's `eval run` subcommand wraps all of the above:
`helikon eval run <dataset.jsonl> --agent NAME` loads an `agents.toml`
sidecar, builds the named agent per case (so mock providers stay
deterministic), runs the configured `[eval].evaluators`, and prints
`render_table()` (or `--json`), exiting non-zero when any case failed. See
[the CLI reference](../reference/cli.md#helikon-eval-run-dataset) for the
full flag grammar and the `[eval]` sidecar section.
