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

**Worker side** — register your agent(s) and serve activities:

```rust
use std::sync::Arc;
use paigasus_helikon_core::LlmAgent;
use paigasus_helikon_runtime_temporal::TemporalAgentWorker;
use temporalio_client::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::default().await?;

    // Build your agent(s)
    let agent: Arc<LlmAgent<Ctx, M, T>> = Arc::new(/* ... */);

    // Build and run the worker
    let worker = TemporalAgentWorker::builder::<Ctx>()
        .task_queue("default")
        .client(client)
        .with_ctx(Arc::new(|| your_context_factory()))
        .register(agent)?
        .build()?;

    // Serve the task queue until shutdown
    worker.run().await?;

    Ok(())
}
```

**Client side** — run your agent through the durable runtime:

```rust
use paigasus_helikon_core::{AgentInput, RunConfig, Runner};
use paigasus_helikon_runtime_temporal::{TemporalRunner, TemporalRunnerConfig};
use temporalio_client::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::default().await?;
    let runner = TemporalRunner::new(client, TemporalRunnerConfig::new("default"));

    let result = runner
        .run(
            &agent,
            ctx,
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

## Core concepts

### v0 Constraint Set

The Temporal runtime v0 explicitly does **not** support:
- Hooks and guardrails (arbitrary async code cannot be made deterministic inside a workflow)
- Handoffs (agent-to-agent transfers)
- Nested agents (agent-as-tool is opaque to Temporal)

All four are rejected at registration time with a descriptive error. See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "v0 Constraint Set") for details.

### Worker-Side Posture

Your agent's `RunContext` (tenant data, auth claims, permission rules) does **not** cross to the worker — the worker fabricates its own context from its configuration. This is intentional: **the worker's permission and redaction settings are authoritative for tool execution**, and Temporal's durable history is a security boundary. See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Worker-Side Posture and Security Boundary").

### Retry Semantics

- **Model errors**: Non-retryable per [ADR-10](https://smk1085.github.io/paigasus-helikon/contributing/adrs.html). Wrap your model in [`RetryingModel`](https://docs.rs/paigasus-helikon-runtime-tokio/latest/paigasus_helikon_runtime_tokio/struct.RetryingModel.html) if you want model retries.
- **Tool errors**: Returned as outcomes, fed back to the model. Never fail the activity.
- **Crash-retry**: If the activity worker crashes, Temporal retries it. Your tools must be idempotent unless you set `max_attempts: 1` to opt out of crash recovery.

See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Retry Semantics and Tool Idempotency").

### Payload Budget

Each activity payload includes the full conversation-so-far. Typical budget: ~**1.5 MB JSON** per activity, supporting **15–20 turns** with tool outputs **≤50 KB each**. Strategies to fit: bound tool output, limit `max_turns`, monitor in Temporal Web UI. See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Payload Budget and Conversation Size").

### Upgrade Discipline

Replaying workflows against a different version of `paigasus-helikon-core` or this crate can cause non-determinism errors. Drain in-flight runs before redeploying, or use blue-green task queues. See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Upgrade Discipline and Determinism").

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-runtime-temporal)
- [Crate docs: detailed reference](https://docs.rs/paigasus-helikon-runtime-temporal/latest/paigasus_helikon_runtime_temporal/)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [Runtimes](https://smk1085.github.io/paigasus-helikon/concepts/runtimes.html)
- [Temporal documentation](https://docs.temporal.io)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
