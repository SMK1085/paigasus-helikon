# paigasus-helikon-providers-bedrock

The Amazon Bedrock **Converse API** model provider for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. `BedrockModel` implements [`paigasus-helikon-core`](https://crates.io/crates/paigasus-helikon-core)'s `Model` trait against the Bedrock Converse streaming API, with support for tool use, streaming, structured output (family-gated via forced-tool synthesis), and a tool-input schema rewriter that makes serde/schemars schemas acceptable to Bedrock's validator.

> **Disambiguation:** this crate is the **Converse model provider** — it lets you invoke Bedrock-hosted LLMs as a drop-in `Model`. It is distinct from [`paigasus-helikon-runtime-agentcore`](https://crates.io/crates/paigasus-helikon-runtime-agentcore), which is the **Bedrock AgentCore runtime** host (not yet implemented; `0.0.0` stub).

## Install

```bash
cargo add paigasus-helikon-providers-bedrock
```

Most users enable the `bedrock` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::bedrock`.

```bash
cargo add paigasus-helikon --features bedrock
```

## Example

```ignore
use paigasus_helikon_providers_bedrock::BedrockModel;

// Loads AWS credentials and region from the standard credential chain
// (environment variables, ~/.aws/config, SSO, IMDS, …).
//
// The credential chain is lazy — auth failures surface at invoke() time,
// not during construction.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = BedrockModel::from_env("anthropic.claude-3-5-sonnet-20241022-v2:0").await?;

    // Pass `model` to LlmAgent::builder::<()>().model(model).
    // Everything downstream is provider-agnostic.
    Ok(())
}
```

For explicit client construction (useful in tests or when you already have an `aws_config::SdkConfig`):

```ignore
use aws_config::BehaviorVersion;
use paigasus_helikon_providers_bedrock::BedrockModel;

async fn build_model() -> Result<BedrockModel, paigasus_helikon_providers_bedrock::BuildError> {
    let sdk_cfg = aws_config::defaults(BehaviorVersion::v2026_01_12())
        .region(aws_config::Region::new("us-east-1"))
        .load()
        .await;

    BedrockModel::converse("amazon.nova-pro-v1:0")
        .sdk_config(&sdk_cfg)
        .build()
}
```

## Model families

The crate detects the model family from the Bedrock model ID and routes capability flags accordingly:

| Family | Examples | Structured output | Forced tool choice |
| --- | --- | --- | --- |
| `Anthropic` | `anthropic.claude-*`, `us.anthropic.*` | yes (forced-tool synthesis) | yes |
| `AmazonNova` | `amazon.nova-*` | yes (forced-tool synthesis) | yes |
| `Mistral` | `mistral.mistral-*` | yes (forced-tool synthesis) | yes |
| `AmazonTitan` | `amazon.titan-*` | no (degrades to text) | no |
| `Llama` | `meta.llama*` | no (degrades to text) | no |
| `Cohere` | `cohere.command-*` | no (degrades to text) | no |
| `Unknown` | anything unrecognized | no | no |

Cross-region inference profile prefixes (`us.`, `eu.`, `ap.`, `apac.`) are stripped before detection.

## Structured output and the schema rewriter

For families that support forced-tool-choice (`Anthropic`, `AmazonNova`, `Mistral`), structured output (`ResponseFormat::JsonSchema`) is synthesized as a hidden tool call — the model is forced to call a reserved internal tool whose input schema is the user-supplied JSON Schema. This approach works without Bedrock native structured-output support.

Bedrock's Converse API validator rejects JSON Schemas that use `$ref`/`$defs`, `oneOf`/`anyOf`/`allOf`, or certain other keywords (such as `$schema`, `format`, `examples`). The `rewrite_tool_schema` function (exposed publicly for testing and advanced use) rewrites schemas inline: inlining `$ref` references, collapsing combinators into a flat object, and stripping unsupported keywords — so schemars-generated schemas for tagged enums and generic structs pass validation.

When a model's family does not support forced-tool-choice, a `JsonSchema` response format degrades silently to a text response; no error is returned.

Reasoning content surfaced by the model (e.g. Claude extended-thinking responses) is delivered as `ModelEvent::ReasoningDelta` in the stream — no extra configuration is required on this provider.

## Credentials and TLS

- **Credential chain:** the standard AWS credential chain — environment variables, `~/.aws/credentials`/`~/.aws/config`, SSO, IMDS, ECS task role, etc. — via `aws-config`. The chain is **lazy**: credential failures surface at `invoke()` time, not at construction.
- **TLS:** uses `aws-lc-rs` (the workspace's existing crypto provider, already registered by `reqwest`/`async-openai`). Do not enable the `ring` feature on the AWS SDK crates alongside this crate — a second `CryptoProvider` panics.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-providers-bedrock)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [model providers](https://smk1085.github.io/paigasus-helikon/concepts/model-providers.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
