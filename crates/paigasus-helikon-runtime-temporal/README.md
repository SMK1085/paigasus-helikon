# paigasus-helikon-runtime-temporal

Durable [Temporal](https://temporal.io)-backed runtime for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. `TemporalRunner` implements [`paigasus-helikon-core`](https://crates.io/crates/paigasus-helikon-core)'s `Runner` trait, making agent runs resilient to worker crashes: the workflow executes as a deterministic Temporal workflow, activities are replayed from history on resumption, and a crash mid-run resumes from the last completed activity.

## Install

```bash
cargo add paigasus-helikon-runtime-temporal
```

Most users enable the `runtime-temporal` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::runtime_temporal`:

```bash
cargo add paigasus-helikon --features runtime-temporal
```

## Quick start

Start a local Temporal dev server:

```bash
temporal server start-dev
```

**Worker side** — register your agent(s) and serve activities (`my_model` is any `Model` impl, e.g. an OpenAI/Anthropic provider crate's model; compile-checked versions of both snippets live in the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal)):

```rust,ignore
use std::sync::Arc;

use paigasus_helikon_core::LlmAgent;
use paigasus_helikon_runtime_temporal::worker::TemporalAgentWorker;
use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to the Temporal server (`temporal server start-dev` locally).
    let target = url::Url::parse("http://localhost:7233")?;
    let connection = Connection::connect(ConnectionOptions::new(target).build()).await?;
    let client = Client::new(connection, ClientOptions::new("default").build())?;

    let agent = Arc::new(
        LlmAgent::builder::<()>()
            .name("assistant")
            .model(my_model)
            .build(),
    );

    // Registration fails fast on hooks/guardrails/handoffs (v0 constraint).
    TemporalAgentWorker::builder::<()>()
        .task_queue("helikon-agents")
        .client(client)
        .with_ctx(|| ())
        .register(agent)?
        .build()?
        .run() // serves the task queue until shutdown
        .await?;
    Ok(())
}
```

**Client side** — run your agent through the durable runtime (the run executes on the worker; the runner needs the agent definition only for its name and session semantics):

```rust,ignore
use paigasus_helikon_core::{AgentInput, LlmAgent, RunConfig, RunContext, Runner};
use paigasus_helikon_runtime_temporal::runner::{TemporalRunner, TemporalRunnerConfig};
use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target = url::Url::parse("http://localhost:7233")?;
    let connection = Connection::connect(ConnectionOptions::new(target).build()).await?;
    let client = Client::new(connection, ClientOptions::new("default").build())?;

    let agent = LlmAgent::builder::<()>()
        .name("assistant")
        .model(my_model)
        .build();

    let runner = TemporalRunner::new(client, TemporalRunnerConfig::new("helikon-agents"));
    let result = runner
        .run(
            &agent,
            RunContext::ephemeral(()),
            AgentInput::from_user_text("What can you help with?"),
            RunConfig::default(),
        )
        .await?;

    println!("Agent output: {}", result.final_output);
    Ok(())
}
```

## Live validation

With a local Temporal dev server running:

```bash
TEMPORAL_TEST_SERVER=localhost:7233 cargo test -p paigasus-helikon-runtime-temporal \
  --test temporal_live -- --test-threads=1
```

Tests validate crash-resume (stopping mid-tool-call and resuming), cancellation with partial transcripts, session persistence, and model error handling.

Set `HELIKON_REQUIRE_TEMPORAL=1` to turn the suite's loud skip into a hard
failure. Without a server the tests *pass* (a skipped test is a passing test, and
`cargo test` captures its output), so this is what stops an unattended run from
reporting green while asserting nothing:

```bash
HELIKON_REQUIRE_TEMPORAL=1 TEMPORAL_TEST_SERVER=127.0.0.1:7233 \
  cargo test -p paigasus-helikon-runtime-temporal --test temporal_live -- --test-threads=1
```

CI runs exactly this in the `temporal-it` job
(`.github/workflows/integration.yml`), currently as a signal-only, non-required
check.

## Core concepts

### v0 Constraint Set

The Temporal runtime v0 explicitly does **not** support:

- Hooks and guardrails (arbitrary async code cannot be made deterministic inside a workflow)
- Handoffs (agent-to-agent transfers)
- Nested agents (agent-as-tool is opaque to Temporal)

Hooks, handoffs, and guardrails are rejected at registration time with a descriptive `RegistrationError`. Nested agent-as-tool runs are permitted — registration succeeds — but the nested run executes non-durably inside its tool activity (a crash-retry re-executes the whole nested run). See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "v0 Constraint Set") for details.

### Worker-Side Posture

Your agent's `RunContext` (tenant data, auth claims, permission rules) does **not** cross to the worker — the worker fabricates its own context from its configuration. This is intentional: **the worker's permission and redaction settings are authoritative for tool execution**, and Temporal's durable history is a security boundary. Configure that posture via `WorkerPosture` on the worker builder (`.posture(...)` — `WorkerPosture::default()` reproduces the fixed v0 defaults exactly), and optionally hand the worker request-scoped caller data — tenant id, user id, auth subject — via a client-attached, serializable `Ctx` seed:

```rust,ignore
use paigasus_helikon_core::PermissionMode;
use paigasus_helikon_runtime_temporal::worker::{TemporalAgentWorker, WorkerPosture};
use paigasus_helikon_runtime_temporal::runner::TemporalRunnerConfig;
use serde_json::json;

// Worker side: tighten the posture and reconstitute `Ctx` from the client's seed.
// Prefer `try_with_seeded_ctx` over `with_seeded_ctx` whenever the seed drives
// authorization, so a malformed/hostile seed fails the run instead of silently
// defaulting to the wrong identity.
let worker = TemporalAgentWorker::builder::<MyCtx>()
    .task_queue("helikon-agents")
    .client(client)
    .posture(WorkerPosture::default().with_permission_mode(PermissionMode::Plan))
    .try_with_seeded_ctx(|seed| MyCtx::from_seed(seed))
    .register(agent)?
    .build()?;

// Client side: attach a small, secret-free seed — it's recorded in Temporal history.
let config = TemporalRunnerConfig::new("helikon-agents")
    .with_ctx_seed(json!({ "tenant": "acme" }));
```

Heartbeats (`.heartbeat_interval(duration)`) are opt-in on the worker builder — they speed up crash reclamation on `call_model`/`invoke_tool`, at the cost of also tripping on a live worker whose executor is starved by a blocking tool call. See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Worker-Side Posture and Security Boundary", § "Heartbeats").

### Retry Semantics

- **Model errors**: Non-retryable per [ADR-10](https://smk1085.github.io/paigasus-helikon/contributing/adrs.html). Wrap your model in [`RetryingModel`](https://docs.rs/paigasus-helikon-runtime-tokio/latest/paigasus_helikon_runtime_tokio/struct.RetryingModel.html) if you want model retries.
- **Tool errors**: Returned as outcomes, fed back to the model. Never fail the activity.
- **Crash-retry**: If the activity worker crashes, Temporal retries it. Your tools must be idempotent unless you set `max_attempts: 1` to opt out of crash recovery.

See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Retry Semantics and Tool Idempotency").

### Payload Budget

Each activity payload includes the full conversation-so-far. Typical budget: ~**1.5 MB JSON** per activity, supporting **15–20 turns** with tool outputs **≤50 KB each**. Strategies to fit: bound tool output, limit `max_turns`, monitor in Temporal Web UI. See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Payload Budget and Conversation Size").

### Upgrade Discipline

Replaying workflows against a different version of `paigasus-helikon-core` or this crate can cause non-determinism errors. Activity **input encoding** is not among those hazards — Temporal's replay check compares an activity's id and type only, never its payloads.

Activity inputs are a single self-describing envelope payload, and as of 0.3.0 (SMA-484) that is the **only** shape a worker decodes. **Upgrading from 0.2.0 or 0.2.1 requires a stop at 0.2.2**, which decodes both shapes: land the fleet on 0.2.2, drain in-flight runs, then take 0.3.0. 0.1.x cannot use that bridge — 0.2.2 decodes the 0.2.0/0.2.1 arities specifically, so a 0.1.x fleet must drain or terminate its in-flight runs outright. 0.2.2 ↔ 0.3.0 is compatible both ways for activity inputs and needs no drain for this change. If a 0.3.0 worker does meet a pre-envelope task it logs an error and fails the attempt retryably — re-join a 0.2.2 worker to the task queue and let the runs drain. **That recovery window is not open indefinitely**: a finite `maximum_attempts` on `model_retry_policy` / `tool_retry_policy`, or a set `WorkflowInput::timeout_ms`, can exhaust the retries first, so act promptly or raise those caps for the duration. Because a queued envelope payload is frozen in history, **drain in-flight runs before rolling back below 0.2.2**. Blue-green task queues remain available. See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Upgrade Discipline and Determinism").

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-runtime-temporal)
- [Crate docs: detailed reference](https://docs.rs/paigasus-helikon-runtime-temporal/latest/paigasus_helikon_runtime_temporal/)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [Runtimes](https://smk1085.github.io/paigasus-helikon/concepts/runtimes.html)
- [Temporal documentation](https://docs.temporal.io)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
