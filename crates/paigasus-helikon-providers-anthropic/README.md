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

## Native tool types

Some Anthropic tools, such as the memory tool, use a typed entry in the request: `{"type": "memory_20250818", "name": "memory"}`. A tool can declare such a type with `Tool::provider_types`. The adapter then sends the typed entry for that tool. It sends no description and no input schema.

```rust,ignore
use paigasus_helikon_core::{ProviderToolType, ToolDef};
use paigasus_helikon_providers_anthropic::PROVIDER_ID;

let def = ToolDef::new("memory", "", serde_json::json!({"type": "object"}))
    .with_provider_types(vec![ProviderToolType::new(PROVIDER_ID, "memory_20250818")]);
```

The adapter merges the `params` of the declared type into the typed entry. It removes `cache_control` and sets `type` and `name` itself. It passes other keys, for example `description`, through unchanged.

To turn this off, call `native_tool_types(false)` on the builder. The adapter then sends the generic entry for every tool. A wrapper tool must forward `provider_types` from the tool it wraps, or the typed entry is lost.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-providers-anthropic)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [model providers](https://smk1085.github.io/paigasus-helikon/concepts/model-providers.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
