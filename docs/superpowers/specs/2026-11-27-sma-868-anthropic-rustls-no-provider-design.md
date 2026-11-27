# SMA-868: the Anthropic provider without a built-in TLS provider

- Linear: SMA-868 (Paigasus Helikon project). It blocks SMA-851.
- Status: approved through the paigasus-core spec (Sven, 2026-11-27: D1 a, D2 a, D3 a).
- Source: paigasus-core `docs/superpowers/specs/2026-11-27-sma-851-real-model-provider-design.md`.
  Section 7.1 is this work. Section 5 has the facts.

## 1. Intent

The paigasus-core agent host uses only the `ring` rustls provider. It must use
`paigasus-helikon-providers-anthropic` and keep `aws-lc-rs` out of its lock file. The
provider must let the caller install the rustls provider. The host must also see the
HTTP status of a non-retryable 4xx answer, so that it can classify the fault.

## 2. Anti-goals (from section 4 of the source spec)

- A2. No second rustls crypto provider in the Helikon workspace. Helikon uses only
  `aws-lc-rs`. The command `cargo tree -i ring --all-features -e normal --target all`
  must print nothing.
- A3. The default features of the provider do not change. A change of behavior is
  allowed only in a release that is marked as breaking.
- A4. The key must not appear in a log line, a `Debug` text, a `Display` text or an error text.

## 3. Facts (from section 5 of the source spec)

- The provider sets `reqwest = { features = ["json", "stream", "rustls"] }`. In reqwest
  0.13.5 the `rustls` feature enables `__rustls-aws-lc-rs`. The `rustls-no-provider`
  feature enables only the platform verifier and `__rustls`. Cargo features only add.
- `build_config()` calls `self.http_client.unwrap_or_default()`. Without an installed
  provider and without the aws-lc feature, reqwest panics in `Client::new()`.
- `build_headers` calls `HeaderValue::from_str(key).expect(..)` on every `invoke`. It
  does not mark the auth header as sensitive. `AuthSource` and `AuthHeader` derive `Debug`.
- The paigasus-core workspace sets `reqwest` with `default-features = false` and the
  features `rustls-no-provider`, `json` and `form`. Cargo features unify, so the provider
  gets `rustls-no-provider` from that line.
- The paigasus-gateway answers errors in the Anthropic envelope (SMA-750 section 6.1):
  400 `invalid_request_error`, 401 `authentication_error`, 403 `permission_error`,
  404 `not_found_error`, 413 `request_too_large`, 429 `rate_limit_error` or
  `billing_error` (with `retry-after`), 5xx `api_error`, 504 `timeout_error`.
- The provider maps 401 and 403 to `Refused { reason }`. It maps 429 `billing_error`,
  400, 404 and 413 to `ModelError::Other`, and it drops `retry-after`. The host classifies
  every `Refused` as deterministic, and `Other` as transient.
- `ModelError` is `#[non_exhaustive]`.
- A model id that is not in `KNOWN_MODELS` gets `max_output_default: 4096` and no caching,
  reasoning or parallel-tool capability.
- Helikon CI builds and tests the provider only with `--all-features`.

## 4. Design (from section 7.1 of the source spec)

One PR. The PR is marked breaking (`!`).

### 4.1 TLS features

```toml
[features]
default            = ["rustls-aws-lc"]
rustls-aws-lc      = ["reqwest/rustls"]
rustls-no-provider = ["reqwest/rustls-no-provider", "dep:rustls"]

[dependencies]
reqwest = { workspace = true, features = ["json", "stream"] }
rustls  = { workspace = true, optional = true, features = ["std"] }
```

- Delta from the source spec: the source text gives `rustls-no-provider` as
  `["reqwest/rustls-no-provider"]`. This design adds `dep:rustls`. The optional direct
  `rustls` dependency has `default-features = false` and the `std` feature only. The
  provider uses it only to read the process-default provider, because reqwest does not
  re-export rustls. It has no provider feature, so it adds no crypto provider.
- The default feature set stays `["rustls-aws-lc"]` (A3). The facade `anthropic` feature
  keeps the default `rustls-aws-lc`. A caller that wants `rustls-no-provider` makes a
  direct dependency on the provider with `default-features = false`.
- There is no `compile_error!`. When `build()` must make its own client, and no TLS feature
  can give one, `build()` returns the new `BuildError::NoCryptoProvider`. It never panics.
  This is the case when `rustls-aws-lc` is off and
  `rustls::crypto::CryptoProvider::get_default()` is `None`. A client from the caller
  (`http_client()`) skips this check.
- The check reads only the features of this crate. It does not depend on the dependency
  graph. If another crate gives reqwest an aws-lc provider, `build()` still returns
  `NoCryptoProvider` when no process default exists.
- The docs say: under `rustls-no-provider`, install a provider before `build()`, or pass
  `http_client`.
- Helikon CI adds `cargo build` and
  `cargo test -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider`
  to `build-no-default-features`. The tests install a provider from a dev-dependency, so
  the A2 `-e normal` rule holds. CI also runs `cargo tree ... -e normal --target all -i aws-lc-rs`
  and the same with `-i ring` for that feature set. Both must print nothing.

### 4.2 Credential safety

- A hand-written `Debug` for `AuthSource` and `AuthHeader` writes `<redacted>`.
- `set_sensitive(true)` is set on the `x-api-key` and `authorization` header values.
- `build()` checks the key once. `HeaderValue::from_str` must accept it, or `build()`
  returns `BuildError::InvalidAuthCredential`. The `expect` in `build_headers` goes away.

### 4.3 Errors

- Core gets `ModelError::Rejected { status: u16, error_type: String, message: String }`
  for a non-retryable HTTP 4xx. This is an addition to a `#[non_exhaustive]` enum, so core
  gets a non-breaking release.
- The Anthropic provider maps 400, 401, 403, 404 and 413 to `Rejected`. 429
  `billing_error` becomes `RateLimited` and keeps `retry-after`. The `refusal` stop
  reason stays `Refused`.
- The other providers do not change in this PR.

### 4.4 Models

Add `claude-sonnet-5-5` and `claude-opus-5-5` to `KNOWN_MODELS`, with their output limits
and capabilities.

### 4.5 Release

The provider becomes 0.4.0 (breaking: new error mapping). Core gets a patch with
`Rejected`. release-plz publishes. paigasus-core waits for the crates.io release. It uses
no `[patch]` and no path or git dependency.

## 5. Decisions made in the plan

### 5.1 `BuildError` variants

`BuildError` stays `#[non_exhaustive]`. It gets three new variants.

| Variant | Text | Rule |
|---|---|---|
| `NoCryptoProvider` | `no TLS crypto provider: enable the rustls-aws-lc feature, install a rustls CryptoProvider before build(), or pass http_client()` | `rustls-aws-lc` off and no process default (or both TLS features off). |
| `HttpClient(String)` | `cannot build the HTTP client: {0}` | `reqwest::Client::builder().build()` failed. |
| `InvalidAuthCredential` | `auth credential contains a byte that is not valid in an HTTP header` | `HeaderValue::from_str` rejected the key or the bearer text. |

- The empty-key check (`EmptyAuthCredential`) runs first.
- The rule for `InvalidAuthCredential` applies to all three sources: `api_key()`,
  `bearer()` and `ANTHROPIC_API_KEY`. The text never contains the key.
- `AnthropicModelBuilder`, `Config` and `AnthropicModel` keep a derived `Debug`. They
  show the redacted form of the key.
- Test support: under `rustls-no-provider`, the unit and integration tests install an
  aws-lc provider from a dev-dependency. Under `rustls-aws-lc` the install is empty, so the
  `--all-features` run proves the default path with no installed provider.

### 5.2 Mapping of an HTTP error to `ModelError`

`map_error_type` uses these rows. The first match wins.

| # | `(status, error_type)` | Result |
|---|---|---|
| 1 | any, `overloaded_error` | `Unavailable` |
| 2 | any, `rate_limit_error` | `RateLimited { retry_after_ms }` |
| 3 | 429, `billing_error` | `RateLimited { retry_after_ms }` (was `Other`) |
| 4 | any, `invalid_request_error`, and the message has `prompt is too long` | `ContextLengthExceeded` |
| 5 | 400, 401, 403, 404 or 413, any type | `Rejected { status, error_type, message }` |
| 6 | no status, `authentication_error` or `permission_error` | `Refused { reason: message }` (in-stream error only) |
| 7 | 500 to 504, or 529 | `Unavailable` |
| 8 | any other status | `Other` (for example 402, 409, 422) |
| 9 | no status | `Transport(message)` |

- A 401, 403 or 404 with a body that is not JSON (a proxy page, an empty body) gives
  `Rejected` with the status and an empty `error_type`. The mapping uses the status first.
- `RetryingModel` does not retry `Rejected`. It retries `Unavailable`.
- The `refusal` stop reason in `stream.rs` stays `Refused { reason: "model refused" }`.
- `Display` of `Rejected`: `request rejected (HTTP {status} {error_type}): {message}`.
  When `error_type` is empty: `request rejected (HTTP {status}): {message}`.

### 5.3 The two model rows

Both rows have the same values: streaming, tools, parallel tool calls, vision, reasoning,
prompt caching. `structured_output` is off. `max_output_default` is 32 768.

| Value | Sonnet 5.5 | Opus 5.5 |
|---|---|---|
| streaming, tools | Captured fixture `empty_thinking_tool_use.txt` (SMA-840). The models overview (quoted in SMA-850) says all current models support vision and tool use. | The same overview quote. No capture in the repo. |
| parallel tool calls | Neighbor rows only. No source in the repo. | Neighbor rows only. No source in the repo. |
| `structured_output` off | This crate forces a tool choice (`tool_choice: {"type":"tool"}`). The Bedrock provider records that these models reject a forced tool choice. See SMA-839 (Bedrock). SMA-848 is an open check for this provider. | The same. |
| vision | The overview quote. | The overview quote. |
| reasoning | The model thinks by default and needs `Adaptive`. The captured fixture has a thinking block. | Opus 4.7 and later reject `Enabled`, so the model has adaptive thinking. No capture. |
| prompt caching | Neighbor rows. The captured `usage` has cache fields with value 0. No direct source. | Neighbor rows. No source in the repo. |
| `max_output_default` 32 768 | SMA-850 design: Sven's call, the same as the other current rows. It is the default `max_tokens`, not the model maximum. | The same. |

Each value with the text "no source" is an unverified assumption. The reader must not
treat it as measured.

### 5.4 Versions, and why every version is bumped by hand

| Crate | From | To |
|---|---|---|
| `paigasus-helikon-core` | 0.8.3 | 0.8.4 |
| `paigasus-helikon-providers-anthropic` | 0.3.6 | 0.4.0 |
| `paigasus-helikon-runtime-agentcore` | 0.4.6 | 0.5.0 |
| `paigasus-helikon` (facade) | 0.9.7 | 0.10.0 |
| `paigasus-helikon-cli` | 0.1.54 | 0.1.55 |

The provider uses a core API from the same PR, so core needs a hand bump. A hand-bumped
sibling needs a hand-bumped facade. A hand-bumped facade publishes on the merge with the
old provider requirement. A later release-plz bump of the provider to 0.4.0 would then give
the facade only a patch, and the breaking-cascade guard would fail on the release PR. So
every crate in the chain gets its final version in this PR. The guard then checks this PR.
After the merge, release-plz opens a release PR that gives the other core dependents a
patch. That is normal and non-breaking.

## 6. Out of scope

- The 401 and 403 mapping of the other providers (follow-up, section 12 of the source spec).
- Validation of the `anthropic-version` and `anthropic-beta` values at `build()`.
