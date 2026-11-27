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

Claude 3.x models are not supported. A model id that the crate does not know reports a conservative capability set (streaming and tools). Use `.with_capabilities()` on the builder to set other values.

## TLS and errors

The default feature `rustls-aws-lc` uses the aws-lc rustls provider, as before. The facade feature `anthropic` uses this default. A host that needs `rustls-no-provider` must not enable the facade feature `anthropic`, because it turns on `rustls-aws-lc`, and a direct dependency with `default-features = false` cannot turn it off again (Cargo features only add).

A host that needs another rustls provider depends on this crate directly:

```bash
cargo add paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider
```

With `default-features = false, features = ["rustls-no-provider"]`, the crate has no rustls provider. The host must install a rustls `CryptoProvider` as the process default before `build()`, for example `rustls::crypto::ring::default_provider().install_default()`. Or the host passes its own client with `http_client(...)`. Otherwise `build()` returns `BuildError::NoCryptoProvider`. With both TLS features off, `build()` always needs `http_client(...)`. The rule reads only the features of this crate. It applies also when another crate gives `reqwest` a provider.

`build()` checks the API key one time. The key is not trimmed. A key with a byte that an HTTP header cannot hold, for example a trailing newline, gives `BuildError::InvalidAuthCredential`. `Debug` writes `<redacted>` for the key, and the auth header is marked sensitive.

The adapter maps HTTP errors and stop reasons as follows:

| Source | `ModelError` |
| --- | --- |
| `invalid_request_error` with `prompt is too long` (checked first) | `ContextLengthExceeded` |
| HTTP 400, 401, 403, 404, 413 | `Rejected { status, error_type, message }` |
| HTTP 429 with `rate_limit_error` or `billing_error` | `RateLimited`, with the `retry-after` value |
| HTTP 500 to 504 and 529 | `Unavailable` |
| Stop reason `refusal` | `Refused` |

`RetryingModel` does not retry `Rejected`.

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

The stream keeps each `thinking` and `redacted_thinking` block as a `ContentPart::ProviderData` part with the key `PROVIDER_DATA_KEY`. The next request sends the eligible blocks back unchanged. The rules below say which blocks are eligible, and only `Adaptive` and `Enabled` send them. The shape of the `data` is private to this crate.

A thinking block is sent back only in the same assistant message as a text block or a `tool_use` block of its own response. If its response had `tool_use` blocks, one of them must be in that message. So after a hand-off that removed the tool calls of a response, or when the history lost those calls, the SDK sends no thinking block of that response. At the end of a history, the SDK sends no thinking block in a last assistant message. With `ExtendedThinking::Disabled`, the SDK sends no thinking block at all. The cache never marks a thinking block as a breakpoint. A response with no kept thinking block writes no provider data.

- The beta `interleaved-thinking-2025-05-14` has an effect only on models with manual interleaving, with `ExtendedThinking::Enabled`. These models are Claude Haiku 4.5, Opus 4.5 and Sonnet 4.5. On Claude Sonnet 4.6 the beta also works, but Anthropic deprecated it there. Claude Opus 4.6 has no manual interleaving. Add `.beta("interleaved-thinking-2025-05-14")` to the builder. Then the model can think again after a tool result. `Adaptive` interleaves by itself.
- With `ExtendedThinking::Disabled` (the default), the provider sends no thinking block back. On a model that thinks by default, for example `claude-sonnet-5-5`, set `ExtendedThinking::Adaptive` to keep the reasoning between tool calls. The provider then also sends `drop_block`.
- `claude-sonnet-5-5` needs `ExtendedThinking::Adaptive`.
- With `ExtendedThinking::Adaptive`, the provider sends `block_binding.prefix_mismatch_behavior = "drop_block"` and the beta header `thinking-binding-controls-2026-08-01`. If the history before a thinking block changed, the API drops that block and every later thinking block. The model loses that reasoning. The request does not fail. Examples are a hand-off with other `system` text or tools, and an edit of an earlier message.
- With `ExtendedThinking::Enabled`, the provider sends no binding. Use `Enabled` only on models that do not run the prefix check, for example `claude-haiku-4-5`. Claude Opus 4.7 and later, Sonnet 5 and later, Haiku 5.5 and Fable reject `Enabled`.
- The default sends no effort, and the model uses its own default. On some models, for example `claude-sonnet-5-5` with the default `medium`, adaptive thinking can skip thinking on a short task. A higher effort makes thinking more likely. In a test, `claude-sonnet-5-5` skipped thinking on a short tool task at `medium` and at `high`, and it thought at `max`. To set the effort, call `.effort(Effort::Max)` on the builder. The provider then sends `output_config: { effort: "max" }`. Some models reject this field, so set it only for a model that supports effort.

**Cost.** Each request sends the eligible thinking blocks of the history again. We measured this on Claude Sonnet 4.5 and Opus 4.5 with `Enabled { budget_tokens: 2048 }`. It added about 100 input tokens to each round without the beta header. It added about 250 to 300 input tokens with the header.

**Recovery.** Anthropic checks each signature. If it rejects one block, each later request of the session fails. To recover, remove every part with `provider == PROVIDER_DATA_KEY` from the stored history.

**Privacy.** The thinking text is stored two times: as a `Reasoning` part and inside the provider data. A host that removes `Reasoning` must also remove the parts with this key. If a host removes these parts, the SDK cannot send the thinking blocks back.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-providers-anthropic)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [model providers](https://smk1085.github.io/paigasus-helikon/concepts/model-providers.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
