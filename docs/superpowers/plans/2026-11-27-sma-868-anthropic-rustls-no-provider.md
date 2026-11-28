# SMA-868 Anthropic provider without a built-in TLS provider — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `paigasus-helikon-providers-anthropic` 0.4.0 can run with a rustls provider that the caller installs, never panics in `build()` or `invoke()` because of TLS or a bad key, never shows the key in `Debug`, and maps a non-retryable HTTP 4xx to the new core error `ModelError::Rejected`.

**Architecture:** The provider gets two TLS features. The default `rustls-aws-lc` keeps today's behaviour. `rustls-no-provider` uses the caller's process-default rustls provider. `build()` checks the TLS state and the key once and returns a `BuildError` in place of a panic. Core 0.8.4 adds `ModelError::Rejected`. The provider maps 400, 401, 403, 404 and 413 to it. All crate versions are bumped by hand in this PR, so that the release publishes on the merge and the breaking-cascade guard checks the PR itself.

**Tech Stack:** Rust 1.94 (edition and MSRV from `[workspace.package]`), reqwest 0.13.5, rustls 0.23.46, wiremock 0.6, thiserror, `paigasus-helikon-runtime-tokio` `RetryingModel` (dev-dependency only).

**Spec:** `docs/superpowers/specs/2026-11-27-sma-868-anthropic-rustls-no-provider-design.md` (Task 1 writes it). Source: paigasus-core `docs/superpowers/specs/2026-11-27-sma-851-real-model-provider-design.md`, §7.1 (part H) and §5. Sven approved that spec on 2026-11-27 with D1 (a), D2 (a), D3 (a).

## Global Constraints

- Worktree: `/Users/smaschek/dev/paigasus/paigasus-helikon-sma-868`, branch `feature/sma-868-anthropic-rustls-no-provider`, base `origin/main` 498beeaa. Run every command there. Do not run `git checkout`, `git switch`, `git reset`, `git stash` or `git rebase`. Do not use `git add -A`. Add explicit paths only.
- Work in the foreground. Do not start `cargo` in the background.
- Commit message form: `<type>(<scope>): SMA-868 <lowercase subject>`. Scopes from `.versionrc` only (`core`, `providers-anthropic`, `ci`, `specs`, `docs`, `release`). End each message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. If a commit fails with "failed to fill whole buffer", 1Password is locked: stop and report. Do not bypass signing or hooks.
- PR title (for the coordinator): `feat(providers-anthropic)!: SMA-868 add a rustls-no-provider feature and map http 4xx to rejected`.
- Feature names, exactly: `rustls-aws-lc` (default, `["reqwest/rustls"]`) and `rustls-no-provider` (`["reqwest/rustls-no-provider", "dep:rustls"]`). The default feature set stays `["rustls-aws-lc"]` (spec A3).
- No `compile_error!`. No panic in `build()` for a missing TLS provider or a bad key.
- Helikon stays aws-lc-only (spec A2). No crate in the workspace may enable the rustls `ring` feature. `cargo tree -i ring --all-features -e normal --target all` must print nothing.
- No key in a `Debug` text, a `Display` text or an error text (spec A4).
- Only these crate directories change: `paigasus-helikon-core`, `paigasus-helikon-providers-anthropic`, `paigasus-helikon` (facade), `paigasus-helikon-runtime-agentcore` (CHANGELOG and version only), `paigasus-helikon-cli` (CHANGELOG and version only). Do not touch `paigasus-helikon-runtime-tokio` or any other crate directory. A changed file in another crate makes release-plz give that crate a minor bump from the `!` commit, and the cascade guard then fails.
- Release versions (Task 9, by hand): core 0.8.3 → **0.8.4**, providers-anthropic 0.3.6 → **0.4.0**, runtime-agentcore 0.4.6 → **0.5.0**, facade 0.9.7 → **0.10.0**, cli 0.1.54 → **0.1.55**.
- Before each commit: `cargo fmt --all`, and `cargo clippy -p <changed crate> --all-features --all-targets -- -D warnings` is clean.
- Each new HTTP error body in a test carries the comment `HAND-AUTHORED, not captured. Shape from the paigasus-gateway Anthropic error envelope (SMA-750 §6.1, quoted in the SMA-868 design).` (CONTRIBUTING.md "Wire-format fixtures", rule 5).

## Review Focus

1. **reqwest gets aws-lc from another crate, but the provider has only `rustls-no-provider`.** Features unify, so `reqwest::Client::new()` would work. `build()` still returns `NoCryptoProvider` when no process default exists, because the rule reads only this crate's features. A reasonable caller expects a clear error, not a silent success that depends on the dependency graph. → Task 3 test `no_provider_build_needs_an_installed_default` and the doc text in Task 8.
2. **A key with a control character (`\n`, `\r`, `\0`) from a file or the environment.** Expect `BuildError::InvalidAuthCredential` at `build()`, never a panic in `invoke()`, and no key bytes in the error text. → Task 4 tests `api_key_with_newline_errors_invalid_auth_credential`, `env_key_with_carriage_return_errors_invalid_auth_credential`, `invalid_auth_credential_text_has_no_key`.
3. **A 401, 403 or 404 with a body that is not JSON or has no `error.type`** (a proxy page, an empty body). Expect `Rejected` with the status and an empty `error_type`, not `Other`. paigasus-core classifies by status. → Task 5 test `http_401_without_error_body_maps_to_rejected`.
4. **`RetryingModel` around the provider and a `Rejected` error.** Expect exactly one HTTP call. A 529 control in the same file must show three calls, so the test can fail. → Task 5 tests `rejected_is_not_retried_by_retrying_model` and `unavailable_is_retried_by_retrying_model`.
5. **The CI `cargo tree` assertion passes because cargo failed or printed to stderr only.** Expect a fail-closed step: `set -euo pipefail`, empty stdout required, and a control that the default graph does contain `aws-lc-rs`. → Task 7 step text and its local run.

---

### Task 1: The Helikon design document

**Files:**
- Create: `docs/superpowers/specs/2026-11-27-sma-868-anthropic-rustls-no-provider-design.md`
- Add to git: `docs/superpowers/plans/2026-11-27-sma-868-anthropic-rustls-no-provider.md` (this file, already written)

**Interfaces:**
- Produces: the spec path that this plan's header names.

- [ ] **Step 1:** Write the design in ASD-STE100 Simplified Technical English, at most about 150 lines. Content, in this order:
  1. Header: Linear SMA-868 (Paigasus Helikon project, blocks SMA-851). Status: approved through the paigasus-core spec (Sven, 2026-11-27, D1 a, D2 a, D3 a). Source: paigasus-core `docs/superpowers/specs/2026-11-27-sma-851-real-model-provider-design.md` (§7.1 is this work, §5 has the facts).
  2. Intent: one paragraph. The paigasus-core agent host is ring-only. It must use this provider without `aws-lc-rs` in its lock file, and it must see the HTTP status of a non-retryable 4xx.
  3. Facts: copy these §5 bullets word for word: the reqwest `rustls` / `rustls-no-provider` bullet; the `build_config()` `unwrap_or_default()` bullet; the `build_headers` `expect` bullet; the paigasus-core workspace `reqwest` bullet; the gateway error-envelope bullet (400, 401, 403, 404, 413, 429, 5xx, 504); the provider-mapping bullet (401/403 → `Refused`, 429 `billing_error` → `Other`); `ModelError` is `#[non_exhaustive]`; the unknown-id `KNOWN_MODELS` bullet; the "Helikon CI builds and tests the provider only with `--all-features`" bullet. Copy A2 and A3 from §4.
  4. Design: copy §7.1 "TLS features", "Credential safety", "Errors" and "Models" word for word.
  5. Decisions made in the plan: the `BuildError` variants and their rules (Task 3, Task 4), the mapping table (Task 5), the two model rows with their sources (Task 6), the version table (Task 9) and why every version is bumped by hand (Task 9, "Why by hand").
  6. Out of scope: the other providers' 401/403 mapping (§12 follow-up); validating `anthropic-version` and `anthropic-beta` values at `build()`.
- [ ] **Step 2:** Commit.

```bash
git add docs/superpowers/specs/2026-11-27-sma-868-anthropic-rustls-no-provider-design.md \
        docs/superpowers/plans/2026-11-27-sma-868-anthropic-rustls-no-provider.md
git commit -m "docs(specs): SMA-868 add the design and the plan for the rustls-no-provider feature"
```

---

### Task 2: Core — `ModelError::Rejected`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs:516-558` (enum `ModelError`), tests module at `:1106`

**Interfaces:**
- Produces (public, consumed by Task 5 and by paigasus-core):

```rust
/// The provider rejected the request with a non-retryable HTTP 4xx status.
/// The same request fails again in the same way, so do not retry it.
#[error("request rejected (HTTP {status} {error_type}): {message}")]
Rejected {
    /// The HTTP status, for example 401.
    status: u16,
    /// The provider's error type, for example `authentication_error`.
    /// Empty when the response body has no error type.
    error_type: String,
    /// The provider's error message.
    message: String,
},
```

  Put it after `Refused`. Do NOT put `#[non_exhaustive]` on the variant: paigasus-core test fakes must be able to construct it.

- [ ] **Step 1: Write the failing test** in the `model.rs` tests module:

```rust
#[test]
fn rejected_display_names_status_type_and_message() {
    let e = ModelError::Rejected {
        status: 401,
        error_type: "authentication_error".to_owned(),
        message: "invalid x-api-key".to_owned(),
    };
    assert_eq!(
        e.to_string(),
        "request rejected (HTTP 401 authentication_error): invalid x-api-key"
    );
}
```

- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-core --lib rejected_display` — expected: FAIL, "no variant named `Rejected`".
- [ ] **Step 3:** Add the variant. Extend the `ModelError` doc paragraph (`:519-523`) with one sentence: "`Rejected` is not transient: `RetryingModel` does not retry it."
- [ ] **Step 4:** Run `cargo test -p paigasus-helikon-core` and `cargo test -p paigasus-helikon-runtime-tokio` — expected: PASS. `RetryPolicy::is_retryable` uses `matches!` on three variants (`retry.rs:80-84`), so `Rejected` is non-retryable with no change there. The test helper `clone_err` (`retry.rs:405-418`) has a `_` arm, so it compiles. A workspace grep found no other exhaustive `match` on `ModelError` outside test code, and `ModelError` has no serde derive.
- [ ] **Step 5:** Run `cargo build --workspace --all-features` — expected: no error and no new warning.
- [ ] **Step 6:** Commit `feat(core): SMA-868 add a rejected model error for a non-retryable http 4xx`.

---

### Task 3: Provider — TLS features and `BuildError::NoCryptoProvider`

**Files:**
- Modify: `Cargo.toml` (root `[workspace.dependencies]`): add `rustls = { version = "0.23", default-features = false }` next to `tokio-rustls`.
- Modify: `crates/paigasus-helikon-providers-anthropic/Cargo.toml`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/builder.rs` (enum `BuildError` `:15-29`, `build_config` `:200-262`, test helper `env_lock` `:269-273`)
- Create: `crates/paigasus-helikon-providers-anthropic/src/test_support.rs`; Modify: `src/lib.rs` (add `#[cfg(test)] mod test_support;`)
- Modify (test call sites only): `src/http.rs`, `src/model.rs`, `src/translate/mod.rs` tests; `tests/messages_wire.rs`, `tests/messages_streaming.rs`, `tests/cancellation.rs`, `tests/structured_output.rs`, `tests/prompt_caching.rs`
- Create: `crates/paigasus-helikon-providers-anthropic/tests/common/mod.rs`, `tests/crypto_provider.rs`

**Interfaces:**
- Produces, provider `Cargo.toml`:

```toml
[features]
default            = ["rustls-aws-lc"]
rustls-aws-lc      = ["reqwest/rustls"]
rustls-no-provider = ["reqwest/rustls-no-provider", "dep:rustls"]

[dependencies]
reqwest = { workspace = true, features = ["json", "stream"] }
rustls  = { workspace = true, optional = true, features = ["std"] }

[dev-dependencies]
rustls  = { workspace = true, features = ["std", "aws-lc-rs"] }
```

  The normal `rustls` dependency has no provider feature. It exists only to call `rustls::crypto::CryptoProvider::get_default()` (reqwest does not re-export rustls). The dev-dependency gives the tests an aws-lc provider to install. `-e normal` does not see dev-dependencies, so A2 holds.
- Produces, `BuildError` (keep `#[non_exhaustive]`), two new variants:

```rust
/// `build()` had to make its own HTTP client, but no TLS crypto provider is
/// available: the `rustls-aws-lc` feature is off and no rustls process
/// default is installed.
#[error("no TLS crypto provider: enable the `rustls-aws-lc` feature, install a rustls CryptoProvider before build(), or pass http_client()")]
NoCryptoProvider,
/// reqwest could not build the default HTTP client.
#[error("cannot build the HTTP client: {0}")]
HttpClient(String),
```

- Produces: `fn default_http_client() -> Result<reqwest::Client, BuildError>` (private, in `builder.rs`). Rule, by cfg:
  - `rustls-aws-lc` on: no provider check.
  - `rustls-aws-lc` off and `rustls-no-provider` on: `rustls::crypto::CryptoProvider::get_default().is_none()` → `Err(NoCryptoProvider)`.
  - both off: `Err(NoCryptoProvider)`.
  - Then `reqwest::Client::builder().build().map_err(|e| BuildError::HttpClient(e.to_string()))`.
- Produces: `build_config` uses `match self.http_client { Some(c) => c, None => default_http_client()? }`. A caller client skips the check.
- Produces: `crate::test_support::install_crypto_provider()` and `tests/common/mod.rs` `pub fn install_crypto_provider()`. Body under `#[cfg(not(feature = "rustls-aws-lc"))]`: `let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();`. Under `rustls-aws-lc` the body is empty, so the `--all-features` run proves the default path without an installed provider.

- [ ] **Step 1: Write the failing test** `tests/crypto_provider.rs`. It is its own test binary, so no other test installs a provider first. It has one test only, because the process default can be set only once.

```rust
#![cfg(all(feature = "rustls-no-provider", not(feature = "rustls-aws-lc")))]
//! `build()` without a caller client under `rustls-no-provider` (SMA-868).
use paigasus_helikon_providers_anthropic::{AnthropicModel, BuildError};

#[test]
fn no_provider_build_needs_an_installed_default() {
    assert!(rustls::crypto::CryptoProvider::get_default().is_none());
    let err = AnthropicModel::messages("claude-sonnet-5-5")
        .api_key("sk-test")
        .build()
        .unwrap_err();
    assert!(matches!(err, BuildError::NoCryptoProvider), "got {err:?}");

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .unwrap();
    AnthropicModel::messages("claude-sonnet-5-5")
        .api_key("sk-test")
        .build()
        .expect("build succeeds once a default is installed");
}
```

- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider --test crypto_provider` — expected: FAIL (the feature does not exist yet).
- [ ] **Step 3:** Change the two `Cargo.toml` files and `builder.rs` as in Interfaces. Update the `BuildError` doc and the `http_client()` doc: "Under `rustls-no-provider`, a caller client skips the crypto-provider check."
- [ ] **Step 4:** Add `install_crypto_provider()` calls: in `builder.rs` inside `env_lock()` (every builder test calls it first); at the start of each other unit test or test helper that reaches `build_config()` or `build()` (`http.rs` `build_config_with` and the two direct tests, `model.rs` two tests, `translate/mod.rs` seven sites); and in each listed integration file through `mod common;` before its `build()` calls. Do not change `tests/live.rs` or `src/live_round_trip.rs`: they are `#[ignore]` live tests.
- [ ] **Step 5:** Run, each must pass:
  - `cargo test -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider` — all tests PASS, `crypto_provider` included. A missed call site fails with `NoCryptoProvider`; add the call there.
  - `cargo test -p paigasus-helikon-providers-anthropic --all-features` — PASS; `crypto_provider` compiles to zero tests.
  - `cargo build -p paigasus-helikon-providers-anthropic --no-default-features` — PASS (no TLS feature at all).
  - `cargo clippy -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider --all-targets -- -D warnings` and the same with `--all-features` — clean.
- [ ] **Step 6:** Run the graph checks. Each of the first three must print nothing on stdout, with exit 0:
  - `cargo tree -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider -e normal --target all -i aws-lc-rs`
  - the same with `-i ring`
  - `cargo tree -i ring --all-features -e normal --target all`
  - Control: `cargo tree -p paigasus-helikon-providers-anthropic -e normal --target all -i aws-lc-rs` must print a tree.
- [ ] **Step 7:** Commit `Cargo.toml`, `Cargo.lock` and the provider files: `feat(providers-anthropic): SMA-868 add the rustls-no-provider feature`.

---

### Task 4: Provider — credential safety and `BuildError::InvalidAuthCredential`

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/builder.rs` (`AuthSource` `:31-36`, `AuthHeader` `:57-62`, `build_config` auth block `:203-224`, `BuildError`)
- Modify: `crates/paigasus-helikon-providers-anthropic/src/http.rs:15-31` and its tests

**Interfaces:**
- Produces, `BuildError`:

```rust
/// The auth credential contains a byte that an HTTP header value cannot
/// hold (for example a newline). The text does not contain the credential.
#[error("auth credential contains a byte that is not valid in an HTTP header")]
InvalidAuthCredential,
```

- Produces: `pub(crate) enum AuthHeader { ApiKey(HeaderValue), Bearer(HeaderValue) }`. `build_config` makes the value once: `HeaderValue::from_str(key)` for `x-api-key`, `HeaderValue::from_str(&format!("Bearer {token}"))` for `authorization`. On `Err` it returns `InvalidAuthCredential`. Then `set_sensitive(true)`. The empty check (`EmptyAuthCredential`) stays first. The rule applies to all three sources: `api_key()`, `bearer()` and `ANTHROPIC_API_KEY`.
- Produces: `build_headers` inserts `value.clone()` for the auth header. The two `expect` calls on the key and the token go away. The `expect` calls on `anthropic-version` and `anthropic-beta` stay (out of scope).
- Produces: hand-written `impl fmt::Debug` for `AuthSource` and `AuthHeader`. Output: `Env`, `ApiKey(<redacted>)`, `Bearer(<redacted>)`. Use `f.debug_tuple("ApiKey").field(&format_args!("<redacted>")).finish()`, so no quotes appear. `AnthropicModelBuilder`, `Config` and `AnthropicModel` keep `#[derive(Debug)]`.

- [ ] **Step 1: Write the failing tests** in `builder.rs` tests (each takes `env_lock()`):
  - `debug_of_builder_redacts_api_key`: `format!("{:?}", AnthropicModelBuilder::new("claude-sonnet-4-6").api_key("sk-secret-868"))` does not contain `sk-secret-868` and contains `ApiKey(<redacted>)`.
  - `debug_of_builder_redacts_bearer`: the same with `.bearer("tok-secret-868")` and `Bearer(<redacted>)`.
  - `debug_of_config_redacts_key`: `format!("{:?}", builder.api_key("sk-secret-868").build_config().unwrap())` does not contain `sk-secret-868`.
  - `api_key_with_newline_errors_invalid_auth_credential`: `.api_key("sk-a\nb")` → `Err(BuildError::InvalidAuthCredential)`.
  - `bearer_with_nul_errors_invalid_auth_credential`: `.bearer("tok\0x")` → same.
  - `env_key_with_carriage_return_errors_invalid_auth_credential`: `save_and_set_env(Some("sk-env\rx"))` → same.
  - `invalid_auth_credential_text_has_no_key`: for the error of `.api_key("sk-secret\n868")`, neither `format!("{err}")` nor `format!("{err:?}")` contains `sk-secret`.
- [ ] **Step 2: Write the failing tests** in `http.rs` tests:
  - `api_key_header_is_sensitive`: `build_headers(&cfg).get("x-api-key").unwrap().is_sensitive()` is `true`, and `format!("{h:?}")` does not contain `sk-test`.
  - `bearer_header_is_sensitive`: the same for `authorization` and `ey...`.
  - Keep `api_key_auth_uses_x_api_key` and `bearer_uses_authorization`: the header bytes do not change.
- [ ] **Step 3:** Run `cargo test -p paigasus-helikon-providers-anthropic --lib` — expected: the new tests FAIL (key visible in `Debug`, no `InvalidAuthCredential` variant).
- [ ] **Step 4:** Implement as in Interfaces.
- [ ] **Step 5:** Run `cargo test -p paigasus-helikon-providers-anthropic --all-features` and `cargo test -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider` — expected: PASS.
- [ ] **Step 6:** Commit `fix(providers-anthropic): SMA-868 redact the credential and check it once at build`.

---

### Task 5: Provider — map a non-retryable 4xx to `Rejected`

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/error.rs` (module doc `:1-9`, `map_error_type` `:15-36`, tests)
- Modify: `crates/paigasus-helikon-providers-anthropic/Cargo.toml` (`[dev-dependencies]`: add `paigasus-helikon-runtime-tokio = { workspace = true }`)
- Modify: `crates/paigasus-helikon-providers-anthropic/tests/messages_wire.rs` (replace `http_401_auth_maps_to_refused` `:167-198`; add tests)

**Interfaces:**
- Consumes: `ModelError::Rejected { status: u16, error_type: String, message: String }` (Task 2); `RetryingModel::new(inner, RetryPolicy)`, `RetryPolicy::new().max_attempts(u32).base_delay(Duration).jitter(bool)` from `paigasus_helikon_runtime_tokio`.
- Produces: `map_error_type` keeps its signature. Arms, in this order (first match wins):

| # | `(status, error_type)` | Result | Change |
|---|---|---|---|
| 1 | `(_, "overloaded_error")` | `Unavailable` | same |
| 2 | `(_, "rate_limit_error")` | `RateLimited { retry_after_ms }` | same |
| 3 | `(Some(429), "billing_error")` | `RateLimited { retry_after_ms }` | new (was `Other`, `retry-after` lost) |
| 4 | `(_, "invalid_request_error")` and message contains `prompt is too long` | `ContextLengthExceeded` | same |
| 5 | `(Some(s @ (400 \| 401 \| 403 \| 404 \| 413)), _)` | `Rejected { status: s, error_type, message }` | new (401/403 were `Refused`; 400/404/413 were `Other`) |
| 6 | `(None, "authentication_error" \| "permission_error")` | `Refused { reason: message }` | in-stream path only, same as today |
| 7 | `(Some(500..=504 \| 529), _)` | `Unavailable` | same |
| 8 | `(Some(_), _)` | `Other` | same (for example 402, 409, 422, a 429 with another type) |
| 9 | `(None, _)` | `Transport(message)` | same |

  The `refusal` stop reason in `stream.rs:485` stays `Refused { reason: "model refused" }`. Do not change `stream.rs`.
- Produces: a module doc that states this table in short form and says that `Rejected` is not retried.

- [ ] **Step 1: Write the failing unit tests** in `error.rs` (each asserts the full variant, with `match … { ModelError::Rejected { status, error_type, message } => { assert_eq!(…) } other => panic!(…) }`):
  - `http_400_maps_to_rejected`: `(Some(400), "invalid_request_error", "missing field")` → `Rejected { 400, "invalid_request_error", "missing field" }`. Replaces `http_400_other_maps_to_other`.
  - `http_401_maps_to_rejected`: `(Some(401), "authentication_error", "bad key")` → `Rejected { 401, "authentication_error", "bad key" }`. Replaces `auth_error_maps_to_refused`.
  - `http_403_maps_to_rejected` (`permission_error`), `http_404_maps_to_rejected` (`not_found_error`), `http_413_maps_to_rejected` (`request_too_large`).
  - `http_401_without_error_body_maps_to_rejected`: `(Some(401), "", "<html>denied</html>")` → `Rejected { 401, "", "<html>denied</html>" }`.
  - `http_429_billing_error_maps_to_rate_limited_with_retry_after`: `(Some(429), "billing_error", "budget", Some(3000))` → `RateLimited { retry_after_ms: Some(3000) }`.
  - `stream_auth_error_stays_refused`: `(None, "authentication_error", "bad key")` → `Refused { reason: "bad key" }`.
  - `http_422_maps_to_other`: `(Some(422), "invalid_request_error", "x")` → `Other(_)`.
  - `http_504_timeout_error_maps_to_unavailable`: `(Some(504), "timeout_error", "slow")` → `Unavailable`.
  - Keep `prompt_too_long_maps_to_context_length_exceeded` unchanged.
- [ ] **Step 2: Write the failing wire tests** in `tests/messages_wire.rs`. Add one helper `async fn first_item(status: u16, body: serde_json::Value, retry_after: Option<&str>) -> Result<ModelEvent, ModelError>` that mounts the response on a `MockServer`, builds the model with `.api_key("sk-test").base_url(server.uri())`, invokes `req_with(vec![user("hi")])` and returns the first stream item.
  - `http_4xx_statuses_map_to_rejected`: for each of `(400, "invalid_request_error")`, `(401, "authentication_error")`, `(403, "permission_error")`, `(404, "not_found_error")`, `(413, "request_too_large")` with body `{"type":"error","error":{"type":<t>,"message":"m"}}`: the item is `Err(Rejected { status, error_type: t, message: "m" })`. Replaces `http_401_auth_maps_to_refused`.
  - `http_429_billing_error_keeps_retry_after`: 429, `billing_error`, `retry-after: 4` → `Err(RateLimited { retry_after_ms: Some(4_000) })`.
  - `rejected_is_not_retried_by_retrying_model`: mount 401 with `.expect(1)`; wrap the model in `RetryingModel::new(model, RetryPolicy::new().max_attempts(3).base_delay(Duration::from_millis(1)).jitter(false))`; the first item is `Err(Rejected { status: 401, .. })`. The `MockServer` drop checks the count.
  - `unavailable_is_retried_by_retrying_model` (control): the same with a 529 `overloaded_error` and `.expect(3)`; the first item is `Err(Unavailable)`.
- [ ] **Step 3:** Run `cargo test -p paigasus-helikon-providers-anthropic --all-features --lib error::` and `cargo test -p paigasus-helikon-providers-anthropic --all-features --test messages_wire` — expected: the new tests FAIL (401 gives `Refused`, 400 gives `Other`). The control `unavailable_is_retried_by_retrying_model` passes already.
- [ ] **Step 4:** Implement the table in `map_error_type` and add the dev-dependency.
- [ ] **Step 5:** Run `cargo test -p paigasus-helikon-providers-anthropic --all-features` and `cargo test -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider` — expected: PASS. Run `cargo test -p paigasus-helikon-provider-stream-conformance` — expected: PASS (it asserts no 4xx mapping).
- [ ] **Step 6:** Commit `feat(providers-anthropic)!: SMA-868 map a non-retryable http 4xx to rejected`.

---

### Task 6: Provider — `KNOWN_MODELS` rows for Claude Sonnet 5.5 and Opus 5.5

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/capabilities.rs:30-139` (table), tests `:156-201`; `src/builder.rs` tests

**Interfaces:**
- Produces two rows in the "Claude 4 and 5 families" block. Put `claude-opus-5-5` before `claude-opus-4-7` and `claude-sonnet-5-5` before `claude-sonnet-4-6`. Both rows are equal:

```rust
caps: ModelCapabilities::empty()
    .with_streaming()
    .with_tools()
    .with_parallel_tool_calls()
    .with_vision()
    .with_reasoning()
    .with_prompt_caching(),
max_output_default: 32_768,
```

  Put one comment line above each row: `// Sources: SMA-868 plan, Task 6. structured_output is off: this crate forces a tool choice, and these models reject one.`

  Sources for each value (repeat them in the Task 1 design):

| Value | Sonnet 5.5 | Opus 5.5 |
|---|---|---|
| streaming, tools | Captured fixture `tests/fixtures/empty_thinking_tool_use.txt` (SMA-840, a `claude-sonnet-5-5` stream with `tool_use`). SMA-850 design §"Haiku 5.5 capabilities" quotes the models overview: "All current models support … vision, and tool use." | The same models-overview quote. No capture in the repo. |
| parallel tool calls | Neighbour rows only. `parallel_tool_use.txt` is HAND-AUTHORED. **No repo source.** | Neighbour rows only. **No repo source.** |
| structured output: off | `translate/response_format.rs:57-77` sends `tool_choice: {"type":"tool"}`. Bedrock `family.rs:143-154` and the Bedrock README (lines 65, 100) record that Sonnet 5.5 and Opus 5.5 reject a forced tool choice. | The same. |
| vision | The models-overview quote. | The models-overview quote. |
| reasoning | `lib.rs:37-38`: `claude-sonnet-5-5` thinks by default and needs `Adaptive`. The captured fixture has a thinking block. | `lib.rs:40`: "Claude Opus 4.7 and later … reject `Enabled`", so the model has adaptive thinking. No capture. |
| prompt caching | Neighbour rows. The captured fixture's `usage` has cache fields with value 0. **No direct repo source.** | Neighbour rows. **No repo source.** |
| `max_output_default` 32 768 | SMA-850 design line 55: Sven's call, "the same as the other current rows". It is the default `max_tokens`, not the model maximum. | The same. |

- [ ] **Step 1: Write the failing tests** in `capabilities.rs`:
  - `sonnet_5_5_row_matches_the_recorded_capabilities`: `lookup("claude-sonnet-5-5")` has `streaming`, `tools`, `parallel_tool_calls`, `vision`, `reasoning`, `prompt_caching` all `true`, `structured_output` `false`, `max_output_default == 32_768`.
  - `opus_5_5_row_matches_the_recorded_capabilities`: the same for `claude-opus-5-5`.
  - In `builder.rs` tests, `sonnet_5_5_uses_its_known_row`: `build_config()` for `claude-sonnet-5-5` gives `max_output_default == 32_768` (today 4096).
- [ ] **Step 2:** Run `cargo test -p paigasus-helikon-providers-anthropic --lib capabilities` — expected: FAIL (unknown id gives 4096 and no reasoning).
- [ ] **Step 3:** Add the two rows.
- [ ] **Step 4:** Run `cargo test -p paigasus-helikon-providers-anthropic --all-features` — expected: PASS, `known_models_have_no_duplicate_ids` included.
- [ ] **Step 5:** Commit `feat(providers-anthropic): SMA-868 add claude sonnet 5.5 and opus 5.5 to the known models`.

---

### Task 7: CI — build and test the provider without the aws-lc provider

**Files:**
- Modify: `.github/workflows/ci.yml:186-217` (job `build-no-default-features`). Add the steps after the agentcore `cargo test` step (line 193). Add no new action.

**Interfaces:**
- Produces: these steps, in the job's existing style:

```yaml
      # SMA-868: paigasus-core installs its own rustls provider (ring) and uses
      # the anthropic provider with `rustls-no-provider`. Build and test that
      # combination; every other job builds the provider with --all-features only.
      - run: cargo build -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider
      - run: cargo test -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider
      - name: Assert no rustls crypto provider under rustls-no-provider
        run: |
          set -euo pipefail
          for provider in aws-lc-rs ring; do
            out="$(cargo tree -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider -e normal --target all -i "$provider")"
            if [ -n "$out" ]; then
              echo "::error::$provider is a normal dependency of paigasus-helikon-providers-anthropic under rustls-no-provider (SMA-868)"
              echo "$out"
              exit 1
            fi
          done
          # Control: the default graph must contain aws-lc-rs, or the check above proves nothing.
          out="$(cargo tree -p paigasus-helikon-providers-anthropic -e normal --target all -i aws-lc-rs)"
          if [ -z "$out" ]; then
            echo "::error::control failed: the default graph has no aws-lc-rs, so the check cannot detect it"
            exit 1
          fi
          echo "no rustls crypto provider under rustls-no-provider"
```

- [ ] **Step 1:** Add the steps.
- [ ] **Step 2:** Run the `run:` script body locally under `/bin/bash` — expected: it prints `no rustls crypto provider under rustls-no-provider`, rc 0.
- [ ] **Step 3:** Prove it can fail: add `"rustls/aws-lc-rs"` to `rustls-no-provider` in the provider `Cargo.toml`, run the script again — expected: `::error::aws-lc-rs is a normal dependency …`, rc 1. Remove the edit and run `git diff --stat crates/paigasus-helikon-providers-anthropic/Cargo.toml` — expected: no output.
- [ ] **Step 4:** Run `bash scripts/check-cargo-profile-env-sync.sh` — expected: PASS (no env change). Run `actionlint .github/workflows/ci.yml` if it is on PATH; do not install it.
- [ ] **Step 5:** Commit `ci(workflows): SMA-868 build and test the anthropic provider with rustls-no-provider`.

---

### Task 8: Docs — provider README and mdBook

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/README.md` (add a section after "Example" `:13-25`)
- Modify: `crates/paigasus-helikon-providers-anthropic/src/lib.rs` (crate doc: a short "# TLS" and "# Errors" part)
- Modify: `docs/book/src/concepts/model-providers.md:72-73` (variant list), the Anthropic section `:119-135`, and `:539-547` (`version = "0.8"` → `"0.10"`)

**Interfaces:**
- Consumes: the names from Tasks 2-6.

- [ ] **Step 1:** README section `## TLS and errors`, in STE, with these facts:
  - The default feature `rustls-aws-lc` uses the aws-lc rustls provider, as before.
  - With `default-features = false, features = ["rustls-no-provider"]`, install a rustls `CryptoProvider` as the process default before `build()`, or pass `http_client(...)`. Else `build()` returns `BuildError::NoCryptoProvider`. The rule reads only this crate's features, also when another crate gives reqwest a provider.
  - `build()` checks the key once. A key with a byte that a header cannot hold gives `BuildError::InvalidAuthCredential`. `Debug` writes `<redacted>` for the key, and the auth header is marked sensitive.
  - The status table: 400, 401, 403, 404, 413 → `ModelError::Rejected { status, error_type, message }`; 429 `rate_limit_error` or `billing_error` → `RateLimited` with `retry-after`; 529 and 5xx → `Unavailable`; a `refusal` stop reason → `Refused`. `RetryingModel` does not retry `Rejected`.
  - Install line for the no-provider case: `cargo add paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider` (no hardcoded version).
- [ ] **Step 2:** The same facts in short form in the `lib.rs` crate doc. Use intra-doc links for `BuildError::NoCryptoProvider` and `BuildError::InvalidAuthCredential`.
- [ ] **Step 3:** mdBook: add `Rejected` to the `ModelError` list at `:72-73` with one clause ("a non-retryable HTTP 4xx, with the status"). In the Anthropic section, add one paragraph with the two features and the `build()` errors, and one with the status table. Change the five `version = "0.8"` lines to `"0.10"`.
- [ ] **Step 4:** Run `npx markdownlint-cli2`, `bash scripts/check-markdownlint-config.sh`, `mdbook build docs/book` and `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-providers-anthropic -p paigasus-helikon-core --all-features --no-deps` — expected: all clean.
- [ ] **Step 5:** Commit `docs(providers-anthropic): SMA-868 document the tls features, the rejected error and the redaction`.

---

### Task 9: Release metadata — hand bumps and CHANGELOGs

**Files:**
- Modify: `Cargo.toml` (root `[workspace.dependencies]` `:192-203`)
- Modify: `crates/paigasus-helikon-core/{Cargo.toml,CHANGELOG.md}`, `crates/paigasus-helikon-providers-anthropic/{Cargo.toml,CHANGELOG.md}`, `crates/paigasus-helikon-runtime-agentcore/{Cargo.toml,CHANGELOG.md}`, `crates/paigasus-helikon/{Cargo.toml,CHANGELOG.md}`, `crates/paigasus-helikon-cli/{Cargo.toml,CHANGELOG.md}`, `Cargo.lock`

**Why by hand.** The provider uses a core API from the same PR, so core needs a hand bump (CLAUDE.md, runbook "Caveat 1"). A hand-bumped sibling needs a hand-bumped facade ("Caveat 2"). A hand-bumped facade publishes on the merge with the old provider requirement, so a later release-plz bump of the provider to 0.4.0 would give the facade only a patch, and the cascade guard would fail on the release PR. So every crate in the chain gets its final version here. The crates publish on the merge, in dependency order, and the guard checks this PR.

**Interfaces:**
- Produces this version table. Each `version` line and each root pin changes together:

| Crate | From | To | Root pin |
|---|---|---|---|
| `paigasus-helikon-core` | 0.8.3 | 0.8.4 | `version = "0.8.4"` |
| `paigasus-helikon-providers-anthropic` | 0.3.6 | 0.4.0 | `version = "0.4.0"` |
| `paigasus-helikon-runtime-agentcore` | 0.4.6 | 0.5.0 | `version = "0.5.0"` |
| `paigasus-helikon` (facade) | 0.9.7 | 0.10.0 | `version = "0.10.0"` (self-pin) |
| `paigasus-helikon-cli` | 0.1.54 | 0.1.55 | none (no root pin) |

  Dependents of the provider (a normal dependency, from the root `Cargo.toml` and each manifest): the facade (optional, feature `anthropic`), `runtime-agentcore` (optional, feature `example-anthropic`) and the CLI (exempt from the guard; binary crate). `tests/provider-stream-conformance` is `publish = false`. The only dependent of `runtime-agentcore` is the facade. Core gets a patch, so it needs no cascade.
- Expected after the merge (state it in the PR body): release-plz opens a release PR that gives the other core dependents a patch ("updated the following local packages: paigasus-helikon-core"), because their packaged core requirement is now `^0.8.4`. That is normal and non-breaking.

- [ ] **Step 1:** Change the five `version` lines and the four root pins.
- [ ] **Step 2:** Write a dated section `- 2026-11-27` under `## [Unreleased]` in each CHANGELOG. Use the compare-link form of the existing headings, for example `## [0.4.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.3.6...paigasus-helikon-providers-anthropic-v0.4.0) - 2026-11-27`. Content:
  - core 0.8.4, `### Added`: `ModelError::Rejected { status, error_type, message }` for a non-retryable HTTP 4xx. `RetryingModel` does not retry it.
  - provider 0.4.0, `### Added`: the features `rustls-aws-lc` (default) and `rustls-no-provider`; `BuildError::NoCryptoProvider`, `BuildError::HttpClient`, `BuildError::InvalidAuthCredential`; the two model rows. `### Fixed`: the key is redacted in `Debug` and sent as a sensitive header; a bad key fails at `build()`, not with a panic in `invoke()`. `### Upgrade notes` (**breaking**): 401 and 403 now give `Rejected`, not `Refused`; 400, 404 and 413 give `Rejected`, not `Other`; 429 `billing_error` gives `RateLimited` with `retry-after`, not `Other`. Code that matches `Refused` for an auth failure must match `Rejected { status: 401 | 403, .. }`. Requires core 0.8.4.
  - runtime-agentcore 0.5.0, `### Other`: moves to `paigasus-helikon-providers-anthropic` 0.4 (breaking), used by the `example-anthropic` feature. No API change of this crate.
  - facade 0.10.0, `### Other`: `[**breaking**]` the `anthropic` feature re-exports provider 0.4 and `runtime-agentcore` re-exports 0.5, so their types are new types. The facade's own API does not change. `### Upgrade notes`: change your requirement to `0.10`; read the provider 0.4.0 notes; run `cargo tree -d` to find two copies of a crate.
  - cli 0.1.55, `### Other`: moves to provider 0.4.0 and core 0.8.4.
- [ ] **Step 3:** Run `cargo update -w` — expected: `Cargo.lock` changes only the five workspace versions (and nothing from the registry). Check with `git diff --stat Cargo.lock`.
- [ ] **Step 4:** Run `bash scripts/check-breaking-cascade.sh` (local run: the base is the merge-base with `origin/main`) — expected: pass. Run `bash scripts/check-breaking-cascade-selftest.sh` — expected: pass.
- [ ] **Step 5:** Prove that the guard sees this PR: set the facade back to 0.9.8 in a scratch edit, run the guard — expected: a failure that lists `paigasus-helikon` with lowest acceptable `0.10.0`. Restore 0.10.0. `git diff` must show only the intended lines.
- [ ] **Step 6:** Commit `chore(release): SMA-868 bump the anthropic provider, core and their dependents`.

---

### Task 10: Full local gate run

**Files:** none (fix only what a gate reports, in the task that owns the file, then come back here).

- [ ] **Step 1:** Run every gate from CLAUDE.md "Common commands", in order. Each must pass:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-features --all-targets -- -D warnings`
  - `cargo test --workspace --all-features`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
  - `DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh` (if the nightly is missing, report it; do not install it)
  - `npx markdownlint-cli2` and `bash scripts/check-markdownlint-config.sh`
  - `bash scripts/check-cargo-profile-env-sync.sh` and `bash scripts/check-cargo-profile-env-sync-selftest.sh`
  - `bash scripts/check-release-gaps-selftest.sh`
  - `bash scripts/check-breaking-cascade-selftest.sh` and `bash scripts/check-breaking-cascade.sh`
  - `mdbook build docs/book`
- [ ] **Step 2:** Run the SMA-868 checks:
  - `cargo build -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider`
  - `cargo test -p paigasus-helikon-providers-anthropic --no-default-features -F rustls-no-provider`
  - `cargo build -p paigasus-helikon-providers-anthropic --no-default-features`
  - the Task 7 script body — expected: rc 0
  - `cargo tree -i ring --all-features -e normal --target all` — expected: no stdout
- [ ] **Step 3:** Run `git status --short` and `git diff --stat origin/main...HEAD` — expected: only the files in Tasks 1-9. No file under another crate directory (Global Constraints).
- [ ] **Step 4:** If a gate needed a fix, commit it with the type and scope of the task that owns the file.
