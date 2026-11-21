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

## Extended thinking in a tool loop

The stream keeps each `thinking` and `redacted_thinking` block as a `ContentPart::ProviderData` part with the key `PROVIDER_DATA_KEY`. The next request sends the blocks back unchanged for every assistant message in the history. The shape of the `data` is private to this crate.

A thinking block is sent back only in the same assistant message as a text block or a `tool_use` block of its own response. If its response had `tool_use` blocks, one of them must be in that message. So after a hand-off that removed the tool calls of a response, or when the history lost those calls, the SDK sends no thinking block of that response. At the end of a history, the SDK sends no thinking block in a last assistant message. The cache never marks a thinking block as a breakpoint. A response with no kept thinking block writes no provider data.

- For a 4.x model with `ExtendedThinking::Enabled`, add `.beta("interleaved-thinking-2025-05-14")` to the builder. Then the model can think again after a tool result.
- `claude-sonnet-5-5` needs `ExtendedThinking::Adaptive`.

**Cost.** Each request sends the thinking blocks of the history again. We measured this on Claude Sonnet 4.5 and Opus 4.5 with `Enabled { budget_tokens: 2048 }`. It added about 100 input tokens to each round without the beta header. It added about 250 to 300 input tokens with the header.

**Recovery.** Anthropic checks each signature. If it rejects one block, each later request of the session fails. To recover, remove every part with `provider == PROVIDER_DATA_KEY` from the stored history.

**Privacy.** The thinking text is stored two times: as a `Reasoning` part and inside the provider data. A host that removes `Reasoning` must also remove the parts with this key. If a host removes these parts, the SDK cannot send the thinking blocks back.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-providers-anthropic)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [model providers](https://smk1085.github.io/paigasus-helikon/concepts/model-providers.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
