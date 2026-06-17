# paigasus-helikon-runtime-tokio

The default ephemeral Tokio runner for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. `TokioRunner` implements [`paigasus-helikon-core`](https://crates.io/crates/paigasus-helikon-core)'s `Runner` trait, adding run-level control — cancellation, timeout, session loading/persistence, and event aggregation — at the boundary around an agent's event stream.

## Install

```bash
cargo add paigasus-helikon-runtime-tokio
```

Most users enable the `runtime-tokio` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::runtime_tokio`.

## Example

```rust
use paigasus_helikon_core::{AgentInput, RunConfig, Runner};
use paigasus_helikon_runtime_tokio::TokioRunner;

// `agent` is any `Agent` impl; `ctx` is a `RunContext`.
let result = TokioRunner
    .run(
        &agent,
        ctx,
        AgentInput::from_user_text("Hello!"),
        RunConfig::default(),
    )
    .await?;

println!("{}", result.final_output);
```

`TokioRunner` loads persisted history from the run's `Session` at start and writes the run's events back at exit, so `input` is the *new turn* rather than the whole conversation. Cancellation and timeout are best-effort — they lose to a terminal event that already occurred (see the crate docs).

## Retrying transient errors

Wrap any `Model` in `RetryingModel` to retry transient provider failures
(`Unavailable`, `RateLimited`, `Transport`) with exponential backoff + jitter.
Retry is **opt-in** — configured by wrapping the model, not via `RunConfig`
(the runner can't reach the agent's model, and core can't sleep) — and is
disabled unless you wrap. It covers *connection establishment*: once a response
has started streaming, a mid-stream drop is surfaced rather than retried.

```rust
use std::time::Duration;
use paigasus_helikon_runtime_tokio::{RetryPolicy, RetryingModel};

// `model` is any `Model` (e.g. an OpenAI or Anthropic provider).
let policy = RetryPolicy::new()
    .max_attempts(4)
    .base_delay(Duration::from_millis(250));
let resilient = RetryingModel::new(model, policy);
// Build your agent with `resilient` as its model.
```

`RateLimited { retry_after_ms }` waits at least the provider's hint; backoff
sleeps abort promptly on cancellation.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-runtime-tokio)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [the agent loop](https://smk1085.github.io/paigasus-helikon/concepts/agent-loop.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
