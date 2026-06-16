# paigasus-helikon-providers-openai

The OpenAI model adapter for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. `OpenAiModel` implements [`paigasus-helikon-core`](https://crates.io/crates/paigasus-helikon-core)'s `Model` trait over OpenAI's Chat Completions and Responses APIs.

## Install

```bash
cargo add paigasus-helikon-providers-openai
```

Most users enable the `openai` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::openai`.

## Example

```rust
use paigasus_helikon_providers_openai::OpenAiModel;

// Reads the API key from the OPENAI_API_KEY environment variable.
let model = OpenAiModel::chat("gpt-5-mini").build()?;
```

Pass `model` to `LlmAgent::builder::<()>().model(model)`. The provider lives entirely in this one construction line — everything downstream (the `#[tool]` functions, the builder, the run loop) is provider-agnostic. See the [quickstart](https://smk1085.github.io/paigasus-helikon/getting-started/quickstart.html) for a full agent.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-providers-openai)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [model providers](https://smk1085.github.io/paigasus-helikon/concepts/model-providers.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
