# paigasus-helikon-providers-gemini

The Google Gemini model provider for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. `GeminiModel` implements [`paigasus-helikon-core`](https://crates.io/crates/paigasus-helikon-core)'s `Model` trait against the Gemini streaming API, with support for tool use, streaming, and native structured output via `responseSchema`.

## Transports

Gemini 1.x and 2.x models are not supported. A model id that the crate does not know reports a conservative capability set (streaming and tools). Use `.with_capabilities()` on the builder to set other values.

Two transports are available:

- **Developer API** — authenticates with an API key (`GEMINI_API_KEY` or `GOOGLE_API_KEY`). Simplest path for personal projects and CI.
- **Vertex AI** — authenticates with an OAuth bearer token or a `TokenProvider` implementation. Used for production workloads on Google Cloud. Enable the `vertex-adc` feature for Application Default Credentials (ADC) support via `gcp_auth`.

## Install

```bash
cargo add paigasus-helikon-providers-gemini
```

Most users enable the `gemini` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::gemini`.

```bash
cargo add paigasus-helikon --features gemini
```

For Vertex AI with ADC, enable the `vertex-adc` feature on this crate directly:

```bash
cargo add paigasus-helikon-providers-gemini --features vertex-adc
```

## Example

```ignore
use paigasus_helikon_providers_gemini::GeminiModel;

// Reads GEMINI_API_KEY or GOOGLE_API_KEY from the environment.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = GeminiModel::from_env("gemini-3.8-flash")?;

    // Pass `model` to LlmAgent::builder::<()>().model(model).
    // Everything downstream is provider-agnostic.
    Ok(())
}
```

For explicit builder construction:

```ignore
use paigasus_helikon_providers_gemini::GeminiModel;

fn build_model() -> Result<GeminiModel, paigasus_helikon_providers_gemini::BuildError> {
    GeminiModel::developer("gemini-3.8-flash")
        .api_key("your-api-key")
        .build()
}
```

## Structured output

The Gemini provider uses Gemini's **native `responseSchema`** field — it does not use forced-tool synthesis. When `ResponseFormat::JsonSchema` is set on a `ModelSettings`, the schema is passed directly to the Gemini API as `generationConfig.responseSchema`, and `responseMimeType` is set to `"application/json"`.

> **Note:** Gemini rejects requests that combine `responseSchema` with active function calling. The provider returns a `ModelError::Other` conflict error before sending if the request has non-empty `tools` OR a `tool_choice` other than `None` (including `ToolChoice::Auto`, `ToolChoice::Required`, or a specific named tool). Setting `response_format` alongside an empty `tools` list and no active `tool_choice` is fine.

A JSON-Schema sanitizer runs on every schema passed to `responseSchema`: it inlines `$ref` references, converts `[T, "null"]` type arrays to `nullable: true`, replaces `const` with single-item `enum`, renames `oneOf` to `anyOf`, strips unsupported keywords (`$schema`, `additionalProperties`, `unevaluatedProperties`, `patternProperties`, `examples`, `default`, `$defs`/`definitions`), and drops `format` values Gemini doesn't recognize for their type (keeping e.g. `enum`/`date-time`).

## Vertex AI and `TokenProvider`

For Vertex AI, supply a bearer token directly or implement the `TokenProvider` trait for per-request token refresh. Model ids are examples. Use any model that your account can reach. Vertex model availability differs by region.

```ignore
use paigasus_helikon_providers_gemini::{GeminiModel, TokenProvider};

// Static bearer token (for testing; tokens expire in ~1 hour)
let model = GeminiModel::vertex("gemini-3.8-flash", "my-project", "global")
    .bearer_token("ya29.your-token")
    .build()?;
```

With the `vertex-adc` feature enabled, use Application Default Credentials for production:

```ignore
use paigasus_helikon_providers_gemini::GeminiModel;

// Reads GOOGLE_CLOUD_PROJECT (required) and GOOGLE_CLOUD_LOCATION (default: "global").
// ADC credentials are discovered from GOOGLE_APPLICATION_CREDENTIALS, the gcloud
// application_default_credentials.json, the GCE metadata server, or the gcloud CLI.
let model = GeminiModel::vertex_from_env("gemini-3.8-flash").await?;
```

## Limitations

Request translation drops the following content (some drops log a `tracing` warning):

- **Remote-URL images** — only inline base64-encoded images are forwarded; `ContentPart::ImageUrl` variants with remote URLs are omitted.
- **Audio parts** — audio `ContentPart` variants are not supported by this provider and are dropped.
- **Non-text tool-result parts** — only the text content of tool results is forwarded; image or structured-data parts in tool results are omitted.
- **Duplicate tool results and empty text** — a second tool result for one call is dropped, with a `tracing` warning on target `paigasus::gemini::translate`. Empty text parts are dropped too.

Tool loops work. The provider keeps each `thoughtSignature` as `ContentPart::ProviderData` (key `PROVIDER_DATA_KEY`, value `"gemini"`) and sends it back on its own call. It restores the call order that Gemini returned. A signature from another model id or another API counts as not valid. When no call in a model content of the current turn has a valid signature, the provider puts the documented dummy value `skip_thought_signature_validator` on each call of that content. A request that ends with a model turn gets a `user` text turn "Continue." appended, because Gemini rejects such a request. Gemini 1.x and 2.x models are not supported. These limits apply:

- **Vertex** — Vertex signatures are not live-tested.
- **Dummy value** — it is live-tested only on `gemini-3.8-flash` (Developer API). It is not tested on Vertex.
- **Text-level signatures** — they are not sent back.

The following feature is not yet implemented:

- **Reasoning streaming** — Gemini's `thoughtsContent` field in streaming responses is currently discarded. Reasoning delta events (`ModelEvent::ReasoningDelta`) are not emitted by this provider. This will be addressed in a future release.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-providers-gemini)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [model providers](https://smk1085.github.io/paigasus-helikon/concepts/model-providers.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
