# paigasus-helikon-core

The trait surface and core types of the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents.

This is the **dependency root** of the workspace. It owns the seven object-safe traits every other crate is written against — `Model`, `Tool`, `Session`, `Guardrail`, `Hook`, `Agent`, and `Runner` — plus the agent loop, the event stream, and the carrier types (`RunContext`, `AgentInput`, `RunResult`, …). It depends on no other workspace crate and picks no provider, runtime, or hosting story for you.

## Install

```bash
cargo add paigasus-helikon-core
```

Most applications don't depend on this crate directly — they use the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade, which re-exports this entire surface as `paigasus_helikon::core`. Depend on `-core` directly when you are implementing the traits — a new provider, runtime, session backend, or tool — without pulling in the facade.

## Example

Assemble an agent from the core types. The model adapter comes from a provider crate (e.g. `OpenAiModel` from `paigasus-helikon-providers-openai`):

```rust
use paigasus_helikon_core::LlmAgent;

// `model` is any `Model` impl.
let agent = LlmAgent::builder::<()>()
    .name("assistant")
    .model(model)
    .instructions("You are a helpful assistant.")
    .build();
```

See the [quickstart](https://smk1085.github.io/paigasus-helikon/getting-started/quickstart.html) for a complete, runnable agent that drives the tool-calling loop end-to-end.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-core)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — start with [core primitives](https://smk1085.github.io/paigasus-helikon/concepts/core-primitives.html) and [the agent loop](https://smk1085.github.io/paigasus-helikon/concepts/agent-loop.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
