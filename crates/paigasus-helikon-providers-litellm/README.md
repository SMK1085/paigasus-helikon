# paigasus-helikon-providers-litellm

LiteLLM proxy provider for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. `LiteLlmModel` implements [`paigasus-helikon-core`](https://crates.io/crates/paigasus-helikon-core)'s `Model` trait against a [LiteLLM](https://docs.litellm.ai) proxy's OpenAI-compatible Chat Completions endpoint, adding LiteLLM's own router and observability fields.

## Install

```bash
cargo add paigasus-helikon-providers-litellm
```

Most users enable the `litellm` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::litellm`.

```bash
cargo add paigasus-helikon --features litellm
```

## Example

```ignore
use paigasus_helikon_providers_litellm::LiteLlmModel;

let model = LiteLlmModel::chat("claude-sonnet-4")   // your proxy's alias
    .base_url("http://litellm:4000")                 // required
    .api_key(std::env::var("LITELLM_API_KEY")?)      // optional
    .fallbacks(["gpt-4o-mini"])
    .num_retries(2)
    .tags(["team:research"])
    .metadata("trace_id", trace_id)
    .build()?;
```

Pass `model` to `LlmAgent::builder::<()>().model(model)`. Everything downstream (the `#[tool]` functions, the builder, the run loop) is provider-agnostic.

## When to use this instead of the OpenAI provider

`OpenAiModel::base_url()` can also point at a LiteLLM proxy, and is the better
choice when the proxy simply fronts OpenAI models. Reach for this crate when
you need LiteLLM's router fallbacks and retries, spend/trace metadata,
reasoning streaming from non-OpenAI backends, or arbitrary operator-chosen
model aliases.

## `base_url` is required

There is no default. An unset base URL is a build error rather than a silent
attempt at `localhost:4000`. It may also come from `LITELLM_API_BASE` (or
`LITELLM_PROXY_API_BASE`).

Both `http://host:4000` and `http://host:4000/v1` work. If your gateway is
mounted somewhere the `/v1` heuristic gets wrong, override the whole path with
`.chat_completions_path()`.

## Capabilities are your declaration

A LiteLLM alias (`prod-fast`, `team-a/gpt`) carries no information the SDK can
act on, so the model defaults to a conservative `streaming + tools` capability
set. Declare what the backend actually supports:

```ignore
use paigasus_helikon_core::ModelCapabilities;

let model = LiteLlmModel::chat("prod-fast")
    .base_url("http://litellm:4000")
    .with_capabilities(
        ModelCapabilities::empty()
            .with_streaming()
            .with_tools()
            .with_vision()
            .with_structured_output(),
    )
    .build()?;
```

`streaming` is always forced on — this provider has no non-streaming path.

## Authentication is optional

Self-hosted LiteLLM often runs without a `master_key` inside a cluster. If no
key is configured, no `Authorization` header is sent. Empty or whitespace-only
keys are treated as absent rather than sent as a malformed `Bearer `.

## Retries multiply

`.num_retries(n)` is a **server-side** retry count, and LiteLLM composes it
with `.fallbacks()` under router semantics this crate does not implement or
model. Wrapping the model in a client-side retry decorator multiplies with
both: each client attempt re-runs the entire server-side retry-and-fallback
chain, so the total upstream call count grows multiplicatively with client
attempts. Treat server-side and client-side retry as mutually exclusive unless
you have measured the composition against your own proxy configuration.

## Escape hatches

Not every LiteLLM parameter is modelled. `.extra_body()` merges arbitrary JSON
into the request root and `.header()` adds arbitrary request headers, so you
are never blocked waiting on a release:

```ignore
let model = LiteLlmModel::chat("prod-fast")
    .base_url("http://litellm:4000")
    .extra_body(serde_json::json!({"guardrails": ["pii-check"]}))
    .header("x-litellm-timeout", "30")
    .build()?;
```

Keys the provider computes per-request (`model`, `messages`, `stream`,
`tools`, `temperature`, …) are rejected at build time rather than silently
dropped. Do not put secrets in `extra_body`: its contents flow through the
same request path as the rest of the payload, so they can be captured by a
tracing subscriber or HTTP logging middleware, and can land in your own
recorded test fixtures or request logs.

## Limitations

- Chat Completions only. No Responses backend; `previous_response_id` is ignored.
- Always streams.
- `n > 1` is unsupported — only the first choice is read.
- A backend that emits per-chunk *delta* usage (rather than cumulative
  snapshots) will under-count tokens.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-providers-litellm)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [model providers](https://smk1085.github.io/paigasus-helikon/concepts/model-providers.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
