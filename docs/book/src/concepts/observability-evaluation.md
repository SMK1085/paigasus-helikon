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
no evaluator on any case yielded `Failed`.

### Trace sinks

`TraceSink` records each case's result as the run progresses — feature-gated
so the crate stays lean by default:

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
