# paigasus-helikon-providers-anthropic

The Anthropic model adapter for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. `AnthropicModel` implements [`paigasus-helikon-core`](https://crates.io/crates/paigasus-helikon-core)'s `Model` trait over Anthropic's Messages API, with support for prompt caching (`CacheStrategy`) and extended thinking (`ExtendedThinking`).

## Install

```bash
cargo add paigasus-helikon-providers-anthropic
```

Most users enable the `anthropic` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::anthropic`.

## Example

```rust
use paigasus_helikon_providers_anthropic::AnthropicModel;

// Reads the API key from the ANTHROPIC_API_KEY environment variable.
let model = AnthropicModel::messages("claude-sonnet-4-6").build()?;
```

Pass `model` to `LlmAgent::builder::<()>().model(model)`. The provider lives entirely in this one construction line — everything downstream is provider-agnostic, so the same agent runs against any provider. See the [quickstart](https://smk1085.github.io/paigasus-helikon/getting-started/quickstart.html) for a full agent.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-providers-anthropic)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [model providers](https://smk1085.github.io/paigasus-helikon/concepts/model-providers.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
