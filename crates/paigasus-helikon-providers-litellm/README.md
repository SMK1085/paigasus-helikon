# paigasus-helikon-providers-litellm

LiteLLM proxy provider for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK.

Talks to a [LiteLLM](https://docs.litellm.ai) proxy over its OpenAI-compatible
Chat Completions endpoint, adding LiteLLM's own router and observability
fields.

```sh
cargo add paigasus-helikon-providers-litellm
```

## Quick start

```rust,ignore
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

```rust,ignore
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

`.num_retries(n)` is a **server-side** retry count. Wrapping the model in a
client-side retry decorator multiplies with it, and each client attempt re-runs
the entire fallback chain. A 3-attempt client policy around `.num_retries(2)`
with two fallbacks is up to 18 upstream calls per turn. Treat server-side and
client-side retry as mutually exclusive unless you have measured otherwise.

## Escape hatches

Not every LiteLLM parameter is modelled. `.extra_body()` merges arbitrary JSON
into the request root and `.header()` adds arbitrary request headers, so you
are never blocked waiting on a release:

```rust,ignore
let model = LiteLlmModel::chat("prod-fast")
    .base_url("http://litellm:4000")
    .extra_body(serde_json::json!({"guardrails": ["pii-check"]}))
    .header("x-litellm-timeout", "30")
    .build()?;
```

Keys the provider computes per-request (`model`, `messages`, `stream`,
`tools`, `temperature`, …) are rejected at build time rather than silently
dropped. Do not put secrets in `extra_body` — it is serialised into request
snapshots in tests.

## Limitations

- Chat Completions only. No Responses backend; `previous_response_id` is ignored.
- Always streams.
- `n > 1` is unsupported — only the first choice is read.
- A backend that emits per-chunk *delta* usage (rather than cumulative
  snapshots) will under-count tokens.

## License

Apache-2.0 OR MIT.
