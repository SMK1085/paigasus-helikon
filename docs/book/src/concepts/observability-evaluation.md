# Observability & Evaluation

Two separable concerns share this chapter. **Observability** is shipped:
the agent loop emits OpenTelemetry-compatible spans following GenAI semantic
conventions, and you bring your own collector. **Evaluation** is not yet
implemented — `paigasus-helikon-evals` is a `0.0.0` name-claim stub.

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

The handle is passed to `RunContext::new` as its fourth argument (alongside the
user context, session, hook registry, and cancellation token). The loop reads it
back via `RunContext::tracer` and emits the configured `session.id`, `user.id`,
and `tags` onto the trace. `TracerHandleBuilder` is a consuming builder — its
`with_*` methods take and return `self`.

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

Evaluation is **not yet implemented.** `paigasus-helikon-evals` exists in the
workspace as a `0.0.0` stub with `publish = false`: it claims the crate name and
carries a single module docstring, nothing more. There is no public API to call.

The planned harness — replay against recorded traces, LLM-as-judge scoring, and
trajectory assertions — lands in a future ticket. Until then, evaluate runs with
your own tooling: the OpenTelemetry spans described above already capture
inputs, outputs, token usage, and the full tool/handoff trajectory, so an
external evaluator (Langfuse datasets, a custom judge, or recorded-trace replay
in your test suite) can consume them today without waiting on this crate.

This page will document the real API the moment the harness ships. Treat any
`paigasus-helikon-evals` symbol you see referenced elsewhere as forward-looking.
