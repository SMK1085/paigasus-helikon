# paigasus-helikon-providers-bedrock

The Amazon Bedrock **Converse API** model provider for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. `BedrockModel` implements [`paigasus-helikon-core`](https://crates.io/crates/paigasus-helikon-core)'s `Model` trait against the Bedrock Converse streaming API, with support for tool use, streaming, structured output (forced-tool synthesis, or native output on Claude Sonnet 5.5 and Opus 5.5), and a tool-input schema rewriter that makes serde/schemars schemas acceptable to Bedrock's validator.

> **Disambiguation:** this crate is the **Converse model provider** — it lets you invoke Bedrock-hosted LLMs as a drop-in `Model`. It is distinct from [`paigasus-helikon-runtime-agentcore`](https://crates.io/crates/paigasus-helikon-runtime-agentcore), which is the **Bedrock AgentCore runtime** host.

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
    let sdk_cfg = aws_config::defaults(BehaviorVersion::v2026_11_17())
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
| `Anthropic` | `anthropic.claude-*`, `us.anthropic.*`, `global.anthropic.*` | yes (forced-tool synthesis; native output on Sonnet 5.5 and Opus 5.5) | yes, except Sonnet 5.5 and Opus 5.5 |
| `AmazonNova` | `amazon.nova-*` | yes (forced-tool synthesis) | yes |
| `Mistral` | `mistral.mistral-*` | yes (forced-tool synthesis) | yes |
| `AmazonTitan` | `amazon.titan-*` | no (degrades to text) | no |
| `Llama` | `meta.llama*` | no (degrades to text) | no |
| `Cohere` | `cohere.command-*` | no (degrades to text) | no |
| `Unknown` | anything unrecognized (use `BedrockModelBuilder::family` for an opaque ID) | no | no |

Detection reads the first segment of the model ID. If the first segment is not a provider name, detection reads the second segment. This skips a geographic prefix. The known prefixes are `us.`, `eu.`, `apac.`, `jp.`, `au.`, `ca.`, `in.` and `global.`. A future prefix works too. For an ARN, detection uses the resource ID, which is the text after the last `/`. An opaque ARN, such as an application inference profile ARN, gives `Unknown`.

### Opaque model IDs

Some model IDs do not name the model. Examples are application inference profile ARNs, provisioned-throughput ARNs and custom-model ARNs. Detection gives `Unknown` for these IDs. Use `BedrockModelBuilder::family` to set the family yourself.

```rust,ignore
use paigasus_helikon_providers_bedrock::{BedrockModel, ModelFamily};

let model = BedrockModel::converse(arn)
    .sdk_config(&sdk_cfg)
    .family(ModelFamily::Anthropic)
    .build()?;
```

The override has the full effect of a detected family. It sets the default capabilities and the request translation. It also controls how the provider sends `ToolChoice`, and which requests fail locally. These local failures apply to `ResponseFormat::JsonSchema` or `JsonObject` with `ToolChoice::Tool` or `ToolChoice::None`. They also apply to `ToolChoice::Tool` with an undefined name. `ToolChoice::Required` fails locally when the request has no tools and no `JsonSchema` or `JsonObject` format. Each of these formats adds a synthesized tool. The `from_env` constructor cannot take the override. Use the builder.

The `.capabilities(...)` method changes only the flags that `capabilities()` reports. It does not change the request. The `.capabilities(...)` value wins over the defaults of the family.

Set the family of the model that the profile uses.

The static rule for Claude Sonnet 5.5 and Opus 5.5 reads the model ID. An opaque ARN does not contain the model name, so the rule does not apply. A profile on one of these models works as follows. The first structured-output request sends a forced tool choice. Bedrock rejects it once. After one rejection, the handle uses native output. On the native path, the provider omits `ToolChoice::Required` and `ToolChoice::Tool` and logs a warning.

A request that fails locally does not change the handle. If several first requests run at the same time on one handle, Bedrock rejects each of them. A new handle sends the rejected request again. Build one handle and clone it. This recovery is not verified live for application inference profiles.

## Structured output and the schema rewriter

For families that support forced-tool-choice (`Anthropic`, `AmazonNova`, `Mistral`), structured output (`ResponseFormat::JsonSchema`) is synthesized as a hidden tool call — the model is forced to call a reserved internal tool whose input schema is the user-supplied JSON Schema. Claude Sonnet 5.5 and Opus 5.5 reject a forced tool choice. These models use native output instead (see the next section).

For tool input schemas, Bedrock's Converse validator rejects JSON Schemas that use `$ref`/`$defs`, `oneOf`/`anyOf`/`allOf`, or certain other keywords (such as `$schema`, `format`, `examples`). The `rewrite_tool_schema` function (exposed publicly for testing and advanced use) rewrites schemas inline: inlining `$ref` references, collapsing combinators into a flat object, and stripping unsupported keywords — so schemars-generated schemas for tagged enums and generic structs pass validation.

### Native structured output (Claude Sonnet 5.5 and Opus 5.5)

These models reject a forced `toolChoice`. For these models, the crate puts the `JsonSchema` in `outputConfig.textFormat`. The native output schema accepts `$ref`/`$defs` and `anyOf`.

On a finalizing turn, the crate can add a hidden anchor tool with no `toolChoice`. If the model calls this anchor, the provider logs a warning. `outputConfig` does not constrain the text of that call.

An Anthropic model ID that is not on the list can also reject the forced choice. This costs one rejected request for each `BedrockModel` handle. After that, the handle and its clones use native output. The crate logs one warning. Only a `ValidationException` with one of the two known rejection texts changes the handle. The request must also send a forced choice. Other errors go to the caller and do not change the handle. A request that fails locally does not reach Bedrock, so it cannot change the handle. For example, `JsonSchema` together with `ToolChoice::Tool` fails locally on a forced-choice handle.

The crate rewrites the schema before it sends the schema:

- It adds `additionalProperties: false` to object nodes. It does not add the keyword to a node that has `patternProperties` or `propertyNames`, to a node in an `allOf` composition or to any node below an `allOf` member, to a `const` or `enum` node or any node below it, or to a node whose `required` names a property that `properties` does not declare. It also does not add the keyword to an object node next to `oneOf` or `anyOf`. The branches of such a node stay open too. Limit: a `$defs` entry outside an `allOf` member is still closed, also when a `$ref` in the member points to it. That closed entry can reject a property that another member declares. Do not use a `$ref` to an object in an `allOf` member if the other members add properties.
- It changes `oneOf` to `anyOf`.
- It removes `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`, `multipleOf`, `maxItems`, `uniqueItems`, `minItems` above 1, and `not`, `if`, `then` and `else`.

An open object in an `allOf` composition can cause a `ValidationException` from Bedrock. You cannot close each member, because each closed member would reject the properties of the other members. Put the properties into one closed object instead.

The server does not enforce the removed limits. The agent loop does not evaluate the JSON schema. It checks that the result deserializes into the output type, and asks for a repair when it does not. A removed limit that only the schema states (for example a `schemars` range attribute) is not checked. Add your own validation for such a limit. The change from `oneOf` to `anyOf` also accepts a value that matches more than one branch. If your code needs exactly one branch to match, validate the result against the original schema.

Bedrock rejects these shapes with its own error: maps (`HashMap`, or `additionalProperties` that is `true` or a schema), `patternProperties`, `propertyNames`, tuples (`prefixItems`), the schemas `true` and `{}`, and recursive types. The crate does not rewrite them.

On this path, `JsonObject` degrades to text. User tools stay callable when `JsonSchema` is set. The crate omits `ToolChoice::Required` and `ToolChoice::Tool` with a warning. `ToolChoice::Tool` with a name that is not a defined tool is still an error. `ToolChoice::Required` with no tools and no response format is still an error. With a structured response format, the crate omits it with a warning.

When a model's family does not support forced-tool-choice, a `JsonSchema` response format degrades silently to a text response; no error is returned.

Reasoning content surfaced by the model (e.g. Claude extended-thinking responses) is delivered as `ModelEvent::ReasoningDelta` in the stream — no extra configuration is required on this provider.

### `ToolChoice::None` after a tool round

A request can set `ToolChoice::None` when the history has tool calls. Bedrock needs a tool configuration for such a history. For Claude models, the provider sends tool specs for the tools that the history uses. It also sends the Anthropic field `tool_choice: none` in `additionalModelRequestFields`. For other model families, the provider sends the tool history as text and sends no tool configuration. If the model calls a tool, the stream gives an error.

On families other than Claude, the model can write text that looks like a tool call, for example `[tool call ...]`. This is text, not a tool call. On Claude, a prompt that asks for a tool call can get an empty answer.

If you set `JsonSchema` or `JsonObject` with `ToolChoice::None`, the result depends on the model. On the native path (Claude Sonnet 5.5 and Opus 5.5), the provider keeps the hidden anchor tool. For other Claude models, Mistral and Nova, the provider gives a conflict error. For Llama, Titan, Cohere and unknown models, the provider gives no error. It uses the `ToolChoice::None` handling above and sends no structured output, so the response is text.

## Credentials and TLS

- **Credential chain:** the standard AWS credential chain — environment variables, `~/.aws/credentials`/`~/.aws/config`, SSO, IMDS, ECS task role, etc. — via `aws-config`. The chain is **lazy**: credential failures surface at `invoke()` time, not at construction.
- **TLS:** uses `aws-lc-rs` (the workspace's existing crypto provider, already registered by `reqwest`/`async-openai`). Do not enable the `ring` feature on the AWS SDK crates alongside this crate — a second `CryptoProvider` panics.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-providers-bedrock)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [model providers](https://smk1085.github.io/paigasus-helikon/concepts/model-providers.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
