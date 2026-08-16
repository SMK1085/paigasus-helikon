# LiteLLM Provider (`paigasus-helikon-providers-litellm`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a self-contained crate implementing `paigasus_helikon_core::Model` against a LiteLLM proxy, with LiteLLM's router/observability extras, wired into the facade behind a `litellm` feature.

**Architecture:** Own `reqwest` + `eventsource-stream` HTTP client (no `async-openai`), building the request body as `serde_json::Value` so LiteLLM's non-OpenAI fields can be carried, and deserializing SSE chunks into deliberately permissive serde types so heterogeneous proxied backends cannot break the stream. Streaming only. `Finish` is buffered and emitted at `[DONE]`/EOF, never inline.

**Tech Stack:** Rust 2021, MSRV 1.94, `reqwest` 0.13, `eventsource-stream` 0.2, `serde_json`, `async-trait`, `async-stream`, `tokio`, `tracing`; tests with `wiremock` + `insta`.

**Spec:** `docs/superpowers/specs/2026-08-16-sma-451-litellm-provider-design.md` — read §7, §9, §10 before starting. Section references below (§N) point there.

## Global Constraints

- **Crate name:** `paigasus-helikon-providers-litellm`. **Version:** `0.1.0`. Publishes normally — no `publish = false`, no `release-plz.toml` entry.
- **Workspace inheritance is mandatory.** The crate's `Cargo.toml` sets only `name`, `description`, `version`, and its deps/lints. Everything else (`edition`, `rust-version`, `authors`, `license`, `repository`, `homepage`, `keywords`, `categories`) uses `.workspace = true`.
- **All third-party deps use `workspace = true`.** No new third-party crate may be introduced — every dep this plan names already exists in the root `[workspace.dependencies]`.
- **`[lints] workspace = true`** in the crate `Cargo.toml`. `missing_docs` is `warn` workspace-wide and the docs CI job runs `RUSTDOCFLAGS="-D warnings"`, so **every `pub` item needs a `///` doc comment**, including the facade re-export.
- **Commit scope is `providers`** — never `providers-litellm`, which is in neither `.versionrc:18` nor `.github/workflows/pr-title.yml`'s `scopes:` list. Commit format: `<type>(providers): SMA-451 <lowercase subject>`.
- **Never `git add -A`** — `.env` and `.claude` are untracked but not gitignored. Stage explicit paths only.
- **`provider()` returns `"litellm"`.** `model()` returns the alias verbatim.
- **The provider always streams.** There is no non-streaming path.
- **Run `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings` before every commit.** The pre-commit hook is a deliberate no-op; pre-push runs these and is slow, so catching them per-commit avoids a long stall at push time.

---

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | Crate manifest; workspace-inherited metadata, deps, lints |
| `README.md` | crates.io landing page |
| `.gitattributes` | `*.snap text eol=lf` |
| `src/lib.rs` | Crate docs + public re-exports; declares modules |
| `src/capabilities.rs` | `conservative_defaults()`; no model table |
| `src/transport.rs` | Base-URL normalisation, endpoint URL, header assembly |
| `src/builder.rs` | `LiteLlmModelBuilder`, `Config`, `BuildError`, redacting `Debug` |
| `src/translate/request.rs` | `Vec<Item>` → OpenAI-chat `messages` |
| `src/translate/tools.rs` | `ToolDef` → `tools[]`; `ToolChoice` → `tool_choice` |
| `src/translate/response_format.rs` | `ResponseFormat` → `response_format` |
| `src/translate/extras.rs` | LiteLLM extras merge + reserved-key enforcement |
| `src/translate/mod.rs` | `build_request()` — assembles the whole body |
| `src/sse.rs` | Permissive serde types for one SSE chunk |
| `src/stream.rs` | `ChatTranslator` — chunk → `ModelEvent`, plus `finish()` |
| `src/error.rs` | `classify()` + `parse_retry_after_ms()` |
| `src/model.rs` | `LiteLlmModel`, `impl Model`, HTTP + SSE driving loop |
| `tests/*.rs` | Wire, streaming, cancellation, live |

---

### Task 1: Crate scaffold + capabilities

**Files:**
- Create: `crates/paigasus-helikon-providers-litellm/Cargo.toml`
- Create: `crates/paigasus-helikon-providers-litellm/.gitattributes`
- Create: `crates/paigasus-helikon-providers-litellm/src/lib.rs`
- Create: `crates/paigasus-helikon-providers-litellm/src/capabilities.rs`
- Modify: `Cargo.toml` (root — add the `[workspace.dependencies]` entry)

**Interfaces:**
- Consumes: nothing.
- Produces: `capabilities::conservative_defaults() -> ModelCapabilities`, `capabilities::apply_override(base: ModelCapabilities, over: Option<ModelCapabilities>) -> ModelCapabilities`.

- [ ] **Step 1: Create the manifest**

`crates/paigasus-helikon-providers-litellm/Cargo.toml`:

```toml
[package]
name        = "paigasus-helikon-providers-litellm"
description = "LiteLLM proxy provider for the Paigasus Helikon AI SDK."
version                = "0.1.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[dependencies]
paigasus-helikon-core = { workspace = true }
async-trait           = { workspace = true }
async-stream          = { workspace = true }
eventsource-stream    = { workspace = true }
futures-core          = { workspace = true }
futures-util          = { workspace = true }
reqwest               = { workspace = true, features = ["json", "stream", "rustls"] }
serde                 = { workspace = true }
serde_json            = { workspace = true }
thiserror             = { workspace = true }
anyhow                = { workspace = true }
tokio                 = { workspace = true }
tokio-util            = { workspace = true }
tracing               = { workspace = true }

[dev-dependencies]
wiremock = { workspace = true }
insta    = { workspace = true, features = ["json", "yaml"] }
tokio    = { workspace = true, features = ["macros", "rt-multi-thread", "time"] }
reqwest  = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 2: Add the workspace dependency entry**

In the root `Cargo.toml`, in `[workspace.dependencies]`, directly after the
`paigasus-helikon-providers-gemini` line (keeping the block's alignment style):

```toml
paigasus-helikon-providers-litellm   = { path = "crates/paigasus-helikon-providers-litellm", version = "0.1.0" }
```

`members` is the glob `["crates/*", …]`, so no members edit is needed.

- [ ] **Step 3: Create `.gitattributes`**

`crates/paigasus-helikon-providers-litellm/.gitattributes`:

```
*.snap text eol=lf
```

- [ ] **Step 4: Write the failing capabilities test**

`crates/paigasus-helikon-providers-litellm/src/capabilities.rs`:

```rust
//! Capability defaults for a LiteLLM proxy alias.
//!
//! There is deliberately **no** `KNOWN_MODELS` table: LiteLLM model names are
//! operator-chosen aliases (`prod-fast`, `team-a/gpt`) that carry no
//! information the SDK can act on. See the SMA-451 design §11.

use paigasus_helikon_core::ModelCapabilities;

/// Conservative starting capabilities for an unknown proxied backend.
///
/// `parallel_tool_calls` is intentionally unset: most OpenAI-compatible
/// proxies do not support parallel tool calls, and a loop that expects
/// multiple calls fails worse than one that expects a single call.
pub(crate) const fn conservative_defaults() -> ModelCapabilities {
    ModelCapabilities::empty().with_streaming().with_tools()
}

/// Apply a caller override, forcing `streaming` back on.
///
/// The provider has no non-streaming path, so a caller override that cleared
/// `streaming` would advertise a self-contradictory model.
pub(crate) fn apply_override(
    base: ModelCapabilities,
    over: Option<ModelCapabilities>,
) -> ModelCapabilities {
    let mut caps = over.unwrap_or(base);
    caps.streaming = true;
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_streaming_and_tools_only() {
        let c = conservative_defaults();
        assert!(c.streaming);
        assert!(c.tools);
        assert!(!c.parallel_tool_calls, "conservative default must be false");
        assert!(!c.structured_output);
        assert!(!c.vision);
        assert!(!c.reasoning);
        assert!(!c.server_managed_state);
    }

    #[test]
    fn override_replaces_the_default_set() {
        let over = ModelCapabilities::empty().with_tools().with_vision();
        let c = apply_override(conservative_defaults(), Some(over));
        assert!(c.vision, "override must win over the default");
        assert!(c.tools);
    }

    #[test]
    fn override_cannot_clear_streaming() {
        // The provider has no non-streaming path.
        let over = ModelCapabilities::empty().with_tools();
        let c = apply_override(conservative_defaults(), Some(over));
        assert!(c.streaming, "streaming must be forced back on");
    }

    #[test]
    fn no_override_returns_the_base() {
        let c = apply_override(conservative_defaults(), None);
        assert_eq!(c, conservative_defaults());
    }
}
```

- [ ] **Step 5: Create the crate root**

`crates/paigasus-helikon-providers-litellm/src/lib.rs`:

```rust
//! LiteLLM proxy provider for the Paigasus Helikon SDK.
//!
//! Talks to a [LiteLLM](https://docs.litellm.ai) proxy over its
//! OpenAI-compatible Chat Completions endpoint, adding LiteLLM's own router
//! and observability fields. See [SMA-451] for the design.
//!
//! # Quick start
//!
//! ```ignore
//! // Ignored under doctest: the example needs a reachable proxy.
//! use paigasus_helikon_providers_litellm::LiteLlmModel;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let _model = LiteLlmModel::chat("claude-sonnet-4")
//!     .base_url("http://litellm:4000")
//!     .api_key("sk-…")
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! [SMA-451]: https://linear.app/smaschek/issue/SMA-451

mod capabilities;
```

- [ ] **Step 6: Run the tests, expect PASS**

Run: `cargo test -p paigasus-helikon-providers-litellm`
Expected: 4 tests pass. (These are written to pass immediately — the module is
pure data with no prior implementation to fail against. The behavioural TDD
cycles start in Task 2.)

- [ ] **Step 7: Verify the crate builds inside the workspace**

Run: `cargo build -p paigasus-helikon-providers-litellm`
Expected: success, and `Cargo.lock` gains the new member.

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-providers-litellm Cargo.toml Cargo.lock
git commit -m "feat(providers): SMA-451 scaffold litellm provider crate"
```

---

### Task 2: Transport — base-URL normalisation and headers

**Files:**
- Create: `crates/paigasus-helikon-providers-litellm/src/transport.rs`
- Modify: `crates/paigasus-helikon-providers-litellm/src/lib.rs` (add `mod transport;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `transport::normalise_endpoint(base_url: &str, path: &str) -> Result<String, UrlError>`
  - `transport::UrlError` — a crate-private marker; `builder.rs` maps it to `BuildError::InvalidBaseUrl`.
  - `transport::DEFAULT_CHAT_PATH: &str = "/v1/chat/completions"`

**Why this is its own task:** the normalisation rules (§6.1) are the one place
where a plausible string-based implementation is silently wrong, and they are
fully testable in isolation before any HTTP exists.

- [ ] **Step 1: Write the failing test**

Append to `crates/paigasus-helikon-providers-litellm/src/transport.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn norm(u: &str) -> Result<String, UrlError> {
        normalise_endpoint(u, DEFAULT_CHAT_PATH)
    }

    #[test]
    fn appends_v1_chat_completions_to_a_bare_host() {
        assert_eq!(
            norm("http://localhost:4000").unwrap(),
            "http://localhost:4000/v1/chat/completions"
        );
    }

    #[test]
    fn tolerates_a_trailing_slash() {
        assert_eq!(
            norm("http://localhost:4000/").unwrap(),
            "http://localhost:4000/v1/chat/completions"
        );
    }

    #[test]
    fn does_not_double_the_v1_segment() {
        assert_eq!(
            norm("http://localhost:4000/v1").unwrap(),
            "http://localhost:4000/v1/chat/completions"
        );
        assert_eq!(
            norm("http://localhost:4000/v1/").unwrap(),
            "http://localhost:4000/v1/chat/completions"
        );
    }

    #[test]
    fn preserves_a_mount_path() {
        assert_eq!(
            norm("https://gw.example.com/litellm").unwrap(),
            "https://gw.example.com/litellm/v1/chat/completions"
        );
    }

    #[test]
    fn rejects_a_scheme_less_host() {
        // `Url::parse("localhost:4000")` SUCCEEDS with scheme `localhost`,
        // so this case is only caught by the explicit scheme check.
        assert!(norm("localhost:4000").is_err());
    }

    #[test]
    fn rejects_a_non_http_scheme() {
        assert!(norm("ftp://gw.example.com").is_err());
        assert!(norm("file:///etc/passwd").is_err());
    }

    #[test]
    fn rejects_a_query_or_fragment() {
        assert!(norm("http://gw/litellm?key=x").is_err());
        assert!(norm("http://gw/litellm#frag").is_err());
    }

    #[test]
    fn rejects_unparseable_input() {
        assert!(norm("not a url").is_err());
    }

    #[test]
    fn honours_a_custom_path() {
        assert_eq!(
            normalise_endpoint("http://gw:4000", "/openai/deployments/x/chat/completions").unwrap(),
            "http://gw:4000/openai/deployments/x/chat/completions"
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-providers-litellm transport`
Expected: FAIL — `cannot find function 'normalise_endpoint'`.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/paigasus-helikon-providers-litellm/src/transport.rs`:

```rust
//! Endpoint URL construction and request headers.
//!
//! LiteLLM serves both `/chat/completions` and `/v1/chat/completions`, and
//! operators write base URLs both ways, so [`normalise_endpoint`] accepts
//! either. See the SMA-451 design §6.

/// Default request path appended to the configured base URL.
pub(crate) const DEFAULT_CHAT_PATH: &str = "/v1/chat/completions";

/// A `base_url` that could not be turned into a request endpoint.
#[derive(Debug)]
pub(crate) struct UrlError;

/// Resolve `base_url` + `path` into an absolute endpoint URL.
///
/// Operates on parsed URL path segments rather than raw strings: string
/// concatenation would mangle inputs carrying a query, and the `Url` API makes
/// the trailing-segment rules total.
///
/// Rejects any scheme other than `http`/`https`, and any URL carrying a query
/// or fragment. The scheme check is load-bearing — `Url::parse("localhost:4000")`
/// *succeeds*, parsing `localhost` as the scheme and `4000` as the path, so
/// without it the single most likely operator typo would sail through.
pub(crate) fn normalise_endpoint(base_url: &str, path: &str) -> Result<String, UrlError> {
    let mut url = reqwest::Url::parse(base_url).map_err(|_| UrlError)?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlError);
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(UrlError);
    }

    {
        let mut segs = url.path_segments_mut().map_err(|_| UrlError)?;
        // Drop a trailing empty segment produced by a trailing slash.
        segs.pop_if_empty();
        // Drop a trailing `v1`, so `…/v1` + `/v1/chat/completions` does not
        // become `…/v1/v1/chat/completions`.
        segs.pop_if_empty();
        segs.push("");
        segs.pop();
    }
    // Re-read the (now trimmed) path, drop a trailing `v1`, then append.
    let mut segments: Vec<String> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default();
    if segments.last().map(String::as_str) == Some("v1") {
        segments.pop();
    }
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        segments.push(seg.to_owned());
    }
    {
        let mut segs = url.path_segments_mut().map_err(|_| UrlError)?;
        segs.clear();
        for seg in &segments {
            segs.push(seg);
        }
    }
    Ok(url.to_string())
}
```

- [ ] **Step 4: Register the module**

In `src/lib.rs`, after `mod capabilities;`:

```rust
mod transport;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-providers-litellm transport`
Expected: all 9 tests PASS.

If `does_not_double_the_v1_segment` fails, the segment-trimming block is the
culprit — simplify it to: read `path_segments()` into a `Vec<String>` filtering
empties, pop a trailing `"v1"`, extend with `path`'s segments, then `clear()`
and re-push. Do not reach for string concatenation.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-litellm --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src/transport.rs crates/paigasus-helikon-providers-litellm/src/lib.rs
git commit -m "feat(providers): SMA-451 add litellm endpoint url normalisation"
```

---

### Task 3: Builder, config, and build-time validation

**Files:**
- Create: `crates/paigasus-helikon-providers-litellm/src/builder.rs`
- Modify: `crates/paigasus-helikon-providers-litellm/src/lib.rs`

**Interfaces:**
- Consumes: `capabilities::{conservative_defaults, apply_override}`, `transport::{normalise_endpoint, DEFAULT_CHAT_PATH}`.
- Produces:
  - `pub struct LiteLlmModelBuilder` with the methods listed in §5.
  - `pub enum BuildError` — `MissingBaseUrl`, `InvalidBaseUrl(String)`, `ReservedExtraBodyKey(String)`, `ExtraBodyNotAnObject`, `ReservedMetadataKey(String)`, `InvalidHeader(String)`.
  - `pub(crate) struct Config { http: reqwest::Client, endpoint: String, model_id: String, auth: Option<String>, headers: Vec<(String, String)>, capabilities: ModelCapabilities, extras: Extras }`
  - `pub(crate) struct Extras { fallbacks: Vec<String>, num_retries: Option<u8>, metadata: serde_json::Map<String, Value>, tags: Vec<String>, extra_body: serde_json::Map<String, Value> }`
  - `pub(crate) const RESERVED_BODY_KEYS: &[&str]`
- Note for later tasks: `Config` is consumed by `model.rs` behind an `Arc`, and `Extras` by `translate/extras.rs`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/paigasus-helikon-providers-litellm/src/builder.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    // Serialize env-mutating tests, matching providers-openai/src/builder.rs.
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn clear_env() {
        for k in [
            "LITELLM_API_BASE",
            "LITELLM_PROXY_API_BASE",
            "LITELLM_API_KEY",
            "LITELLM_PROXY_API_KEY",
        ] {
            std::env::remove_var(k);
        }
    }

    fn b() -> LiteLlmModelBuilder {
        LiteLlmModelBuilder::new("prod-fast")
    }

    #[test]
    fn missing_base_url_is_an_error() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        assert!(matches!(b().build(), Err(BuildError::MissingBaseUrl)));
    }

    #[test]
    fn base_url_falls_back_to_litellm_api_base() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("LITELLM_API_BASE", "http://from-env:4000");
        let m = b().build().expect("env base_url should be used");
        assert_eq!(m.endpoint(), "http://from-env:4000/v1/chat/completions");
        clear_env();
    }

    #[test]
    fn base_url_falls_back_to_litellm_proxy_api_base() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("LITELLM_PROXY_API_BASE", "http://proxy-env:4000");
        let m = b().build().expect("proxy env base_url should be used");
        assert_eq!(m.endpoint(), "http://proxy-env:4000/v1/chat/completions");
        clear_env();
    }

    #[test]
    fn invalid_base_url_is_an_error() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        assert!(matches!(
            b().base_url("localhost:4000").build(),
            Err(BuildError::InvalidBaseUrl(_))
        ));
    }

    #[test]
    fn auth_is_optional_and_absent_means_no_header() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        let m = b().base_url("http://p:4000").build().unwrap();
        assert!(m.auth().is_none(), "no key configured means no header");
    }

    #[test]
    fn empty_and_whitespace_keys_are_treated_as_absent() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        for key in ["", "   ", "\t"] {
            let m = b().base_url("http://p:4000").api_key(key).build().unwrap();
            assert!(
                m.auth().is_none(),
                "empty/whitespace key must not produce `Bearer `"
            );
        }
    }

    #[test]
    fn api_key_env_fallbacks_are_honoured() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        std::env::set_var("LITELLM_PROXY_API_KEY", "sk-proxy");
        let m = b().base_url("http://p:4000").build().unwrap();
        assert_eq!(m.auth(), Some("sk-proxy"));
        clear_env();
    }

    #[test]
    fn last_set_auth_wins() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        let m = b()
            .base_url("http://p:4000")
            .api_key("first")
            .bearer("second")
            .build()
            .unwrap();
        assert_eq!(m.auth(), Some("second"));
    }

    #[test]
    fn extra_body_must_be_an_object() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        let err = b()
            .base_url("http://p:4000")
            .extra_body(serde_json::json!([1, 2, 3]))
            .build()
            .unwrap_err();
        assert!(matches!(err, BuildError::ExtraBodyNotAnObject));
    }

    #[test]
    fn every_reserved_key_is_rejected_in_extra_body() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        for key in RESERVED_BODY_KEYS {
            let err = b()
                .base_url("http://p:4000")
                .extra_body(serde_json::json!({ *key: "x" }))
                .build()
                .unwrap_err();
            match err {
                BuildError::ReservedExtraBodyKey(k) => assert_eq!(&k, *key),
                other => panic!("expected ReservedExtraBodyKey for {key}, got {other:?}"),
            }
        }
    }

    #[test]
    fn litellm_extras_are_not_reserved_in_extra_body() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        // These are the keys whose upstream shape is least certain, so
        // extra_body must remain a usable escape hatch for them (§7.2).
        for key in ["fallbacks", "num_retries", "metadata", "tags"] {
            assert!(
                b().base_url("http://p:4000")
                    .extra_body(serde_json::json!({ key: "x" }))
                    .build()
                    .is_ok(),
                "{key} must NOT be reserved"
            );
        }
    }

    #[test]
    fn metadata_tags_key_is_rejected_with_its_own_variant() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        let err = b()
            .base_url("http://p:4000")
            .metadata("tags", "oops")
            .build()
            .unwrap_err();
        match err {
            BuildError::ReservedMetadataKey(k) => assert_eq!(k, "tags"),
            other => panic!("expected ReservedMetadataKey, got {other:?}"),
        }
    }

    #[test]
    fn invalid_header_name_or_value_is_rejected() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        assert!(matches!(
            b().base_url("http://p:4000")
                .header("bad header name", "v")
                .build(),
            Err(BuildError::InvalidHeader(_))
        ));
        assert!(matches!(
            b().base_url("http://p:4000")
                .header("x-ok", "bad\nvalue")
                .build(),
            Err(BuildError::InvalidHeader(_))
        ));
    }

    #[test]
    fn debug_output_redacts_the_api_key() {
        let _g = env_lock().lock().unwrap();
        clear_env();
        let m = b()
            .base_url("http://p:4000")
            .api_key("sk-super-secret-value")
            .header("authorization", "Bearer another-secret")
            .build()
            .unwrap();
        let dbg = format!("{m:?}");
        assert!(
            !dbg.contains("sk-super-secret-value"),
            "api key leaked into Debug: {dbg}"
        );
        assert!(
            !dbg.contains("another-secret"),
            "auth header value leaked into Debug: {dbg}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-providers-litellm builder`
Expected: FAIL — `LiteLlmModelBuilder` does not exist.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/paigasus-helikon-providers-litellm/src/builder.rs`:

```rust
//! `LiteLlmModelBuilder` — fluent constructor for [`crate::LiteLlmModel`].
//!
//! `base_url` is required (explicit, else `LITELLM_API_BASE`, else
//! `LITELLM_PROXY_API_BASE`); auth is optional. See the SMA-451 design §5–§7.

use paigasus_helikon_core::ModelCapabilities;
use serde_json::{Map, Value};

use crate::capabilities::{apply_override, conservative_defaults};
use crate::transport::{normalise_endpoint, DEFAULT_CHAT_PATH};

/// Request-body keys the provider computes per-request.
///
/// `.extra_body()` may not set these — letting a caller forge them would make
/// the translator's output unpredictable. The LiteLLM extras (`fallbacks`,
/// `num_retries`, `metadata`, `tags`) are deliberately **absent**: those are
/// the keys whose upstream wire shape is least certain, so `extra_body` must
/// stay a usable escape hatch for them.
pub(crate) const RESERVED_BODY_KEYS: &[&str] = &[
    "model",
    "messages",
    "stream",
    "stream_options",
    "tools",
    "tool_choice",
    "response_format",
    "temperature",
    "top_p",
    "max_tokens",
    "n",
];

/// LiteLLM-specific request fields.
#[derive(Debug, Clone, Default)]
pub(crate) struct Extras {
    pub(crate) fallbacks: Vec<String>,
    pub(crate) num_retries: Option<u8>,
    pub(crate) metadata: Map<String, Value>,
    pub(crate) tags: Vec<String>,
    pub(crate) extra_body: Map<String, Value>,
}

/// Resolved, immutable model configuration.
pub(crate) struct Config {
    pub(crate) http: reqwest::Client,
    pub(crate) endpoint: String,
    pub(crate) model_id: String,
    /// Bearer credential, or `None` for an unauthenticated proxy.
    pub(crate) auth: Option<String>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) capabilities: ModelCapabilities,
    pub(crate) extras: Extras,
}

/// Header names whose values must never appear in `Debug` output.
fn is_secret_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "authorization" || n == "api-key" || n == "x-api-key" || n.starts_with("x-litellm-key")
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str(),
                    if is_secret_header(k) { "<redacted>" } else { v.as_str() },
                )
            })
            .collect();
        f.debug_struct("Config")
            .field("endpoint", &self.endpoint)
            .field("model_id", &self.model_id)
            .field("auth", &self.auth.as_ref().map(|_| "<redacted>"))
            .field("headers", &headers)
            .field("capabilities", &self.capabilities)
            .field("extras", &self.extras)
            .finish()
    }
}

/// Construction-time errors. Runtime errors flow through
/// [`paigasus_helikon_core::ModelError`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// Neither `.base_url()` nor `LITELLM_API_BASE`/`LITELLM_PROXY_API_BASE`
    /// supplied a proxy address.
    #[error("no base URL: set .base_url() or LITELLM_API_BASE")]
    MissingBaseUrl,
    /// `base_url` was unparseable, used a scheme other than http/https, or
    /// carried a query string or fragment.
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    /// `.extra_body()` set a key the provider computes per-request.
    #[error("extra_body may not set the reserved key `{0}`")]
    ReservedExtraBodyKey(String),
    /// `.extra_body()` was given a non-object JSON value.
    #[error("extra_body must be a JSON object")]
    ExtraBodyNotAnObject,
    /// `.metadata()` set a key the provider owns — use `.tags()` instead.
    #[error("metadata key `{0}` is reserved; use the dedicated builder method")]
    ReservedMetadataKey(String),
    /// `.header()` name or value is not a valid HTTP header.
    #[error("invalid header: {0}")]
    InvalidHeader(String),
}

#[derive(Debug, Clone)]
enum Auth {
    None,
    Key(String),
}

/// Fluent builder for [`crate::LiteLlmModel`].
pub struct LiteLlmModelBuilder {
    model_id: String,
    base_url: Option<String>,
    chat_path: String,
    auth: Auth,
    http_client: Option<reqwest::Client>,
    headers: Vec<(String, String)>,
    capabilities_override: Option<ModelCapabilities>,
    extras: Extras,
    extra_body_raw: Option<Value>,
    metadata_pairs: Vec<(String, Value)>,
}

impl std::fmt::Debug for LiteLlmModelBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiteLlmModelBuilder")
            .field("model_id", &self.model_id)
            .field("base_url", &self.base_url)
            .field("chat_path", &self.chat_path)
            .field("auth", &match self.auth {
                Auth::None => "None",
                Auth::Key(_) => "<redacted>",
            })
            .finish_non_exhaustive()
    }
}

impl LiteLlmModelBuilder {
    pub(crate) fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            base_url: None,
            chat_path: DEFAULT_CHAT_PATH.to_owned(),
            auth: Auth::None,
            http_client: None,
            headers: Vec::new(),
            capabilities_override: None,
            extras: Extras::default(),
            extra_body_raw: None,
            metadata_pairs: Vec::new(),
        }
    }

    /// Proxy base URL, e.g. `http://litellm:4000`. Required.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Virtual key / API key. Last-set auth wins. Empty values are ignored.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.auth = Auth::Key(key.into());
        self
    }

    /// Pre-minted bearer token. Last-set auth wins. Empty values are ignored.
    pub fn bearer(mut self, token: impl Into<String>) -> Self {
        self.auth = Auth::Key(token.into());
        self
    }

    /// Override the request path appended to `base_url`.
    ///
    /// Escape hatch for gateways the `/v1` normalisation heuristic gets wrong.
    pub fn chat_completions_path(mut self, path: impl Into<String>) -> Self {
        self.chat_path = path.into();
        self
    }

    /// Add an arbitrary request header, e.g. `x-litellm-tags`.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Use a caller-provided `reqwest::Client`.
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Declare what the proxied backend can do. Wins over the conservative
    /// defaults, except that `streaming` is always forced on.
    pub fn with_capabilities(mut self, caps: ModelCapabilities) -> Self {
        self.capabilities_override = Some(caps);
        self
    }

    /// LiteLLM router fallback model names, tried in order.
    pub fn fallbacks<I, S>(mut self, models: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extras.fallbacks = models.into_iter().map(Into::into).collect();
        self
    }

    /// LiteLLM server-side retry count.
    ///
    /// Note this composes multiplicatively with a client-side retry decorator;
    /// see the crate README.
    pub fn num_retries(mut self, n: u8) -> Self {
        self.extras.num_retries = Some(n);
        self
    }

    /// Add a LiteLLM `metadata` entry (spend logs, tracing correlation).
    ///
    /// The key `tags` is reserved — use [`Self::tags`].
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.metadata_pairs.push((key.into(), value.into()));
        self
    }

    /// LiteLLM routing/spend tags, emitted as `metadata.tags`.
    pub fn tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extras.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Arbitrary extra request-body fields, merged at the root.
    ///
    /// Rejected at build time if it sets a key the provider computes
    /// per-request. The LiteLLM extras are *not* reserved.
    pub fn extra_body(mut self, value: Value) -> Self {
        self.extra_body_raw = Some(value);
        self
    }

    /// Validate everything and produce the model.
    pub fn build(self) -> Result<crate::LiteLlmModel, BuildError> {
        let base = self
            .base_url
            .or_else(|| std::env::var("LITELLM_API_BASE").ok())
            .or_else(|| std::env::var("LITELLM_PROXY_API_BASE").ok())
            .ok_or(BuildError::MissingBaseUrl)?;

        let endpoint = normalise_endpoint(&base, &self.chat_path)
            .map_err(|_| BuildError::InvalidBaseUrl(base.clone()))?;

        // Auth: explicit wins; empty/whitespace is treated as absent so we
        // never emit a malformed `Bearer ` header.
        let auth = match self.auth {
            Auth::Key(k) => Some(k),
            Auth::None => std::env::var("LITELLM_API_KEY")
                .ok()
                .or_else(|| std::env::var("LITELLM_PROXY_API_KEY").ok()),
        }
        .filter(|k| !k.trim().is_empty());

        for (name, value) in &self.headers {
            if reqwest::header::HeaderName::try_from(name.as_str()).is_err() {
                return Err(BuildError::InvalidHeader(name.clone()));
            }
            if reqwest::header::HeaderValue::try_from(value.as_str()).is_err() {
                return Err(BuildError::InvalidHeader(format!("value for `{name}`")));
            }
        }

        let mut extras = self.extras;

        for (k, v) in self.metadata_pairs {
            if k == "tags" {
                return Err(BuildError::ReservedMetadataKey(k));
            }
            extras.metadata.insert(k, v);
        }

        if let Some(raw) = self.extra_body_raw {
            let obj = match raw {
                Value::Object(m) => m,
                _ => return Err(BuildError::ExtraBodyNotAnObject),
            };
            for key in obj.keys() {
                if RESERVED_BODY_KEYS.contains(&key.as_str()) {
                    return Err(BuildError::ReservedExtraBodyKey(key.clone()));
                }
            }
            extras.extra_body = obj;
        }

        let http = self.http_client.unwrap_or_else(default_http_client);
        let capabilities = apply_override(conservative_defaults(), self.capabilities_override);

        Ok(crate::LiteLlmModel::from_config(Config {
            http,
            endpoint,
            model_id: self.model_id,
            auth,
            headers: self.headers,
            capabilities,
            extras,
        }))
    }
}

/// Default HTTP client: a connect timeout so a hung proxy cannot hang
/// `invoke` indefinitely, and redirects disabled so an authenticated POST is
/// never silently replayed elsewhere. No overall request timeout — a long
/// generation is not a hang.
fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_default()
}
```

- [ ] **Step 4: Add the module and a minimal `LiteLlmModel` so the builder compiles**

In `src/lib.rs`, add `mod builder;` and a temporary model shim. Replace the
shim in Task 9 — it exists only so this task is independently testable:

```rust
mod builder;

pub use builder::{BuildError, LiteLlmModelBuilder};

/// LiteLLM proxy provider.
#[derive(Debug, Clone)]
pub struct LiteLlmModel(std::sync::Arc<builder::Config>);

impl LiteLlmModel {
    /// Chat Completions builder for a proxy model alias.
    pub fn chat(model_id: impl Into<String>) -> LiteLlmModelBuilder {
        LiteLlmModelBuilder::new(model_id)
    }

    pub(crate) fn from_config(cfg: builder::Config) -> Self {
        Self(std::sync::Arc::new(cfg))
    }

    #[cfg(test)]
    pub(crate) fn endpoint(&self) -> &str {
        &self.0.endpoint
    }

    #[cfg(test)]
    pub(crate) fn auth(&self) -> Option<&str> {
        self.0.auth.as_deref()
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-providers-litellm`
Expected: all builder + capabilities + transport tests PASS (17+).

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-litellm --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src
git commit -m "feat(providers): SMA-451 add litellm builder and build-time validation"
```

---

### Task 4: Message translation (`Vec<Item>` → `messages`)

**Files:**
- Create: `crates/paigasus-helikon-providers-litellm/src/translate/mod.rs`
- Create: `crates/paigasus-helikon-providers-litellm/src/translate/request.rs`
- Modify: `crates/paigasus-helikon-providers-litellm/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `translate::request::to_chat_messages(items: &[Item]) -> serde_json::Value`.

**Porting instruction.** This is the duplication D6 authorises. Copy
`to_chat_messages` **and its private helpers** (`text_of`, `emit_user_or_hoist`,
`user_message`, `media_url`, `assistant_message`, `openai_tool_call`) verbatim
from `crates/paigasus-helikon-providers-openai/src/translate/request.rs:1-243`,
with exactly two changes:

1. Change every `tracing` target from `paigasus::openai::translate` to
   `paigasus::litellm::translate`.
2. Do **not** copy `to_responses_input` or anything below line 245 — this crate
   has no Responses backend.

**Also port the tests.** Copy the `#[cfg(test)] mod tests` block from the same
file verbatim (it starts after the Responses translation section — find it with
`grep -n 'mod tests' crates/paigasus-helikon-providers-openai/src/translate/request.rs`)
and delete any test that exercises `to_responses_input`. Porting the tests at
the moment of copying is what pins behavioural parity; §13.1 depends on it.

- [ ] **Step 1: Create the module tree**

`crates/paigasus-helikon-providers-litellm/src/translate/mod.rs`:

```rust
//! Request translation: core types → LiteLLM (OpenAI-compatible) JSON.

pub(crate) mod request;
```

And in `src/lib.rs`, add `mod translate;`.

- [ ] **Step 2: Copy the implementation**

Create `src/translate/request.rs` with the header below, then paste the ported
functions beneath it:

```rust
//! [`Vec<Item>`] → LiteLLM (OpenAI-compatible) Chat Completions `messages`.
//!
//! Duplicated from `paigasus-helikon-providers-openai` per the SMA-451 design
//! §13.1, together with that crate's unit tests so behavioural parity is
//! pinned at the moment of copying. LiteLLM fronts heterogeneous backends, so
//! the two mappings are free to diverge without regressing the OpenAI
//! provider; the cross-crate parity test in Task 11 is what makes any
//! divergence visible rather than silent.

use paigasus_helikon_core::{ContentPart, Item, MediaSource};
use serde_json::{json, Value};
```

- [ ] **Step 3: Run the ported tests**

Run: `cargo test -p paigasus-helikon-providers-litellm translate::request`
Expected: every ported test PASSES. If one fails, the copy is incomplete —
diff against the source file rather than editing the test.

- [ ] **Step 4: Add a LiteLLM-specific regression test**

Append to the ported `mod tests`:

```rust
#[test]
fn plain_text_user_turn_emits_string_content() {
    let items = vec![Item::UserMessage {
        content: vec![ContentPart::Text { text: "hi".into() }],
    }];
    let v = to_chat_messages(&items);
    assert_eq!(v, json!([{"role": "user", "content": "hi"}]));
}

#[test]
fn tool_call_then_result_round_trips() {
    let items = vec![
        Item::ToolCall {
            call_id: "call_1".into(),
            name: "get_weather".into(),
            args: json!({"city": "Berlin"}),
        },
        Item::ToolResult {
            call_id: "call_1".into(),
            content: vec![ContentPart::Text { text: "18C".into() }],
        },
    ];
    let v = to_chat_messages(&items);
    let arr = v.as_array().unwrap();
    assert_eq!(arr[0]["role"], "assistant");
    assert_eq!(arr[0]["tool_calls"][0]["id"], "call_1");
    assert_eq!(arr[0]["tool_calls"][0]["function"]["name"], "get_weather");
    assert_eq!(arr[1]["role"], "tool");
    assert_eq!(arr[1]["tool_call_id"], "call_1");
    assert_eq!(arr[1]["content"], "18C");
}
```

- [ ] **Step 5: Run and verify PASS**

Run: `cargo test -p paigasus-helikon-providers-litellm translate::request`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-litellm --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src
git commit -m "feat(providers): SMA-451 add litellm message translation"
```

---

### Task 5: Tools, tool choice, and response format

**Files:**
- Create: `crates/paigasus-helikon-providers-litellm/src/translate/tools.rs`
- Create: `crates/paigasus-helikon-providers-litellm/src/translate/response_format.rs`
- Modify: `crates/paigasus-helikon-providers-litellm/src/translate/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `tools::to_tools(defs: &[ToolDef]) -> Value` — a JSON array.
  - `tools::to_tool_choice(choice: &ToolChoice) -> Value`
  - `response_format::to_response_format(fmt: &ResponseFormat) -> Option<Value>`

- [ ] **Step 1: Write the failing tests**

`src/translate/tools.rs`:

```rust
//! `ToolDef` → LiteLLM `tools[]`, and `ToolChoice` → `tool_choice`.
//!
//! Schema normalisation delegates to [`paigasus_helikon_core::schema::strict`],
//! the canonical normaliser the OpenAI provider also uses — no schema logic is
//! duplicated here.

use paigasus_helikon_core::{ToolChoice, ToolDef};
use serde_json::{json, Value};

/// Translate tool definitions into the OpenAI `tools` array shape.
pub(crate) fn to_tools(defs: &[ToolDef]) -> Value {
    Value::Array(
        defs.iter()
            .map(|d| {
                json!({
                    "type": "function",
                    "function": {
                        "name": d.name,
                        "description": d.description,
                        "parameters": paigasus_helikon_core::schema::strict(&d.schema),
                    }
                })
            })
            .collect(),
    )
}

/// Translate the caller's tool-selection preference.
pub(crate) fn to_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::Required => json!("required"),
        ToolChoice::None => json!("none"),
        ToolChoice::Tool { name } => json!({"type": "function", "function": {"name": name}}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> ToolDef {
        ToolDef {
            name: "get_weather".into(),
            description: "Look up weather".into(),
            schema: json!({
                "type": "object",
                "properties": {"city": {"type": "string"}}
            }),
        }
    }

    #[test]
    fn tool_def_becomes_a_function_entry() {
        let v = to_tools(&[def()]);
        assert_eq!(v[0]["type"], "function");
        assert_eq!(v[0]["function"]["name"], "get_weather");
        assert_eq!(v[0]["function"]["description"], "Look up weather");
    }

    #[test]
    fn tool_schema_runs_through_the_strict_normaliser() {
        let v = to_tools(&[def()]);
        let params = &v[0]["function"]["parameters"];
        assert_eq!(params["additionalProperties"], json!(false));
        assert_eq!(params["required"], json!(["city"]));
    }

    #[test]
    fn empty_defs_produce_an_empty_array() {
        assert_eq!(to_tools(&[]), json!([]));
    }

    #[test]
    fn tool_choice_variants_map_correctly() {
        assert_eq!(to_tool_choice(&ToolChoice::Auto), json!("auto"));
        assert_eq!(to_tool_choice(&ToolChoice::Required), json!("required"));
        assert_eq!(to_tool_choice(&ToolChoice::None), json!("none"));
        assert_eq!(
            to_tool_choice(&ToolChoice::Tool { name: "f".into() }),
            json!({"type": "function", "function": {"name": "f"}})
        );
    }
}
```

`src/translate/response_format.rs`:

```rust
//! `ResponseFormat` → LiteLLM `response_format`.

use paigasus_helikon_core::ResponseFormat;
use serde_json::{json, Value};

/// Translate the caller's response-shape preference.
///
/// Returns `None` for [`ResponseFormat::Text`] — callers omit the field
/// entirely, matching "no constraint" semantics.
pub(crate) fn to_response_format(format: &ResponseFormat) -> Option<Value> {
    match format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject => Some(json!({"type": "json_object"})),
        ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => {
            let schema = if *strict {
                paigasus_helikon_core::schema::strict(schema)
            } else {
                schema.clone()
            };
            Some(json!({
                "type": "json_schema",
                "json_schema": { "name": name, "schema": schema, "strict": *strict }
            }))
        }
        // ResponseFormat is #[non_exhaustive]; unknown future variants mean
        // "no constraint" rather than a hard error.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_omitted() {
        assert!(to_response_format(&ResponseFormat::Text).is_none());
    }

    #[test]
    fn json_object_maps_directly() {
        assert_eq!(
            to_response_format(&ResponseFormat::JsonObject).unwrap(),
            json!({"type": "json_object"})
        );
    }

    #[test]
    fn strict_json_schema_is_normalised() {
        let f = ResponseFormat::JsonSchema {
            name: "Answer".into(),
            schema: json!({"type": "object", "properties": {"a": {"type": "string"}}}),
            strict: true,
        };
        let v = to_response_format(&f).unwrap();
        assert_eq!(v["type"], "json_schema");
        assert_eq!(v["json_schema"]["name"], "Answer");
        assert_eq!(v["json_schema"]["strict"], true);
        assert_eq!(v["json_schema"]["schema"]["additionalProperties"], false);
    }

    #[test]
    fn non_strict_json_schema_passes_through_untouched() {
        let schema = json!({"type": "object", "properties": {"k": {"type": "string"}}});
        let f = ResponseFormat::JsonSchema {
            name: "X".into(),
            schema: schema.clone(),
            strict: false,
        };
        let v = to_response_format(&f).unwrap();
        assert_eq!(v["json_schema"]["schema"], schema);
        assert_eq!(v["json_schema"]["strict"], false);
    }
}
```

- [ ] **Step 2: Register the modules**

In `src/translate/mod.rs`:

```rust
pub(crate) mod request;
pub(crate) mod response_format;
pub(crate) mod tools;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p paigasus-helikon-providers-litellm translate`
Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-litellm --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src
git commit -m "feat(providers): SMA-451 add litellm tool and response-format translation"
```

---

### Task 6: LiteLLM extras + full request assembly

**Files:**
- Create: `crates/paigasus-helikon-providers-litellm/src/translate/extras.rs`
- Modify: `crates/paigasus-helikon-providers-litellm/src/translate/mod.rs`
- Create: `crates/paigasus-helikon-providers-litellm/src/translate/snapshots/` (generated by `insta`)

**Interfaces:**
- Consumes: `builder::{Config, Extras, RESERVED_BODY_KEYS}`, `request::to_chat_messages`, `tools::{to_tools, to_tool_choice}`, `response_format::to_response_format`.
- Produces: `translate::build_request(cfg: &Config, req: &ModelRequest) -> serde_json::Value`.

- [ ] **Step 1: Write `extras.rs`**

```rust
//! LiteLLM-specific request fields: router controls, observability, and the
//! `extra_body` escape hatch.
//!
//! `tags` nest under `metadata.tags`. All three tag forms (`metadata.tags`,
//! top-level `tags`, and the `x-litellm-tags` header) were measured equivalent
//! on LiteLLM 1.97.0; `metadata.tags` is the form upstream documents as
//! supporting negation (`!`) and required (`&`) prefixes. See the SMA-451
//! design §7.3 and Appendix B.

use serde_json::{Map, Value};

use crate::builder::Extras;

/// Merge the LiteLLM extras into an assembled request body.
///
/// Precedence: `extra_body` wins for the unreserved LiteLLM keys, *except*
/// `metadata`, which is deep-merged so `.metadata()` and an `extra_body`
/// `metadata` object compose rather than clobber. Reserved keys were already
/// rejected at `build()`, so nothing here can overwrite a provider-computed
/// field.
pub(crate) fn apply(body: &mut Map<String, Value>, extras: &Extras) {
    if !extras.fallbacks.is_empty() {
        body.insert("fallbacks".to_owned(), Value::from(extras.fallbacks.clone()));
    }
    if let Some(n) = extras.num_retries {
        body.insert("num_retries".to_owned(), Value::from(n));
    }

    let mut metadata = extras.metadata.clone();
    if !extras.tags.is_empty() {
        metadata.insert("tags".to_owned(), Value::from(extras.tags.clone()));
    }
    if !metadata.is_empty() {
        body.insert("metadata".to_owned(), Value::Object(metadata));
    }

    for (k, v) in &extras.extra_body {
        if k == "metadata" {
            // Deep-merge: caller keys win per key, but existing builder
            // metadata survives.
            let mut merged = body
                .get("metadata")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if let Some(obj) = v.as_object() {
                for (mk, mv) in obj {
                    merged.insert(mk.clone(), mv.clone());
                }
                body.insert("metadata".to_owned(), Value::Object(merged));
                continue;
            }
        }
        body.insert(k.clone(), v.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn apply_to_empty(extras: Extras) -> Value {
        let mut body = Map::new();
        apply(&mut body, &extras);
        Value::Object(body)
    }

    #[test]
    fn unset_extras_emit_nothing() {
        assert_eq!(apply_to_empty(Extras::default()), json!({}));
    }

    #[test]
    fn fallbacks_and_num_retries_are_top_level() {
        let e = Extras {
            fallbacks: vec!["b".into(), "c".into()],
            num_retries: Some(2),
            ..Default::default()
        };
        let v = apply_to_empty(e);
        assert_eq!(v["fallbacks"], json!(["b", "c"]));
        assert_eq!(v["num_retries"], json!(2));
    }

    #[test]
    fn tags_nest_under_metadata() {
        let e = Extras {
            tags: vec!["team:research".into()],
            ..Default::default()
        };
        let v = apply_to_empty(e);
        assert_eq!(v["metadata"]["tags"], json!(["team:research"]));
        assert!(v.get("tags").is_none(), "tags must not be top-level");
    }

    #[test]
    fn metadata_and_tags_compose() {
        let mut metadata = Map::new();
        metadata.insert("trace_id".into(), json!("t-1"));
        let e = Extras {
            metadata,
            tags: vec!["free".into()],
            ..Default::default()
        };
        let v = apply_to_empty(e);
        assert_eq!(v["metadata"]["trace_id"], "t-1");
        assert_eq!(v["metadata"]["tags"], json!(["free"]));
    }

    #[test]
    fn metadata_accepts_non_string_values() {
        let mut metadata = Map::new();
        metadata.insert("nested".into(), json!({"session_id": 7}));
        let v = apply_to_empty(Extras { metadata, ..Default::default() });
        assert_eq!(v["metadata"]["nested"]["session_id"], 7);
    }

    #[test]
    fn extra_body_merges_at_the_root() {
        let mut extra_body = Map::new();
        extra_body.insert("guardrails".into(), json!(["pii"]));
        let v = apply_to_empty(Extras { extra_body, ..Default::default() });
        assert_eq!(v["guardrails"], json!(["pii"]));
    }

    #[test]
    fn extra_body_metadata_deep_merges_with_builder_metadata() {
        let mut metadata = Map::new();
        metadata.insert("trace_id".into(), json!("t-1"));
        let mut extra_body = Map::new();
        extra_body.insert("metadata".into(), json!({"spend_logs_metadata": {"x": 1}}));
        let v = apply_to_empty(Extras { metadata, extra_body, ..Default::default() });
        assert_eq!(v["metadata"]["trace_id"], "t-1", "builder metadata survives");
        assert_eq!(v["metadata"]["spend_logs_metadata"]["x"], 1);
    }

    #[test]
    fn extra_body_can_override_an_unreserved_litellm_extra() {
        // `tags` is deliberately NOT reserved so extra_body stays a usable
        // escape hatch if the metadata.tags shape ever changes upstream.
        let mut extra_body = Map::new();
        extra_body.insert("tags".into(), json!(["top-level"]));
        let v = apply_to_empty(Extras {
            tags: vec!["nested".into()],
            extra_body,
            ..Default::default()
        });
        assert_eq!(v["tags"], json!(["top-level"]));
        assert_eq!(v["metadata"]["tags"], json!(["nested"]));
    }
}
```

- [ ] **Step 2: Write `build_request` in `translate/mod.rs`**

Replace `src/translate/mod.rs` with:

```rust
//! Request translation: core types → LiteLLM (OpenAI-compatible) JSON.

pub(crate) mod extras;
pub(crate) mod request;
pub(crate) mod response_format;
pub(crate) mod tools;

use paigasus_helikon_core::{ModelRequest, ToolChoice};
use serde_json::{Map, Value};

use crate::builder::Config;

/// Assemble the full streaming Chat Completions request body.
///
/// Always streaming, always `include_usage` — the trailing usage chunk is how
/// token counts arrive (SMA-451 design §9.5).
///
/// Deliberately absent from the body: `parallel_tool_calls` (carries no caller
/// instruction, so sending it only adds downstream risk), `n` (only the first
/// choice is read), and `previous_response_id` (an OpenAI Responses concept
/// this provider has no backend for).
pub(crate) fn build_request(cfg: &Config, req: &ModelRequest) -> Value {
    let mut body = Map::new();
    body.insert("model".to_owned(), Value::from(cfg.model_id.clone()));
    body.insert("messages".to_owned(), request::to_chat_messages(&req.messages));
    body.insert("stream".to_owned(), Value::Bool(true));
    body.insert(
        "stream_options".to_owned(),
        serde_json::json!({"include_usage": true}),
    );

    if !req.tools.is_empty() {
        body.insert("tools".to_owned(), tools::to_tools(&req.tools));
        if let Some(choice) = &req.model_settings.tool_choice {
            body.insert("tool_choice".to_owned(), tools::to_tool_choice(choice));
        }
    } else if matches!(
        req.model_settings.tool_choice,
        Some(ToolChoice::Required) | Some(ToolChoice::Tool { .. })
    ) {
        tracing::warn!(
            target: "paigasus::litellm::translate",
            "tool_choice requires a tool call but the request carries no tools; dropping tool_choice"
        );
    }

    if let Some(fmt) = &req.model_settings.response_format {
        if let Some(v) = response_format::to_response_format(fmt) {
            body.insert("response_format".to_owned(), v);
        }
    }
    if let Some(t) = req.model_settings.temperature {
        body.insert("temperature".to_owned(), Value::from(t));
    }
    if let Some(p) = req.model_settings.top_p {
        body.insert("top_p".to_owned(), Value::from(p));
    }
    if let Some(m) = req.model_settings.max_output_tokens {
        body.insert("max_tokens".to_owned(), Value::from(m));
    }

    extras::apply(&mut body, &cfg.extras);
    Value::Object(body)
}
```

- [ ] **Step 3: Write the snapshot + invariant tests**

Append to `src/translate/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::RESERVED_BODY_KEYS;
    use paigasus_helikon_core::{
        ContentPart, Item, MediaSource, ModelRequest, ResponseFormat, ToolDef,
    };

    fn cfg() -> Config {
        crate::LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .api_key("sk-test")
            .build()
            .unwrap()
            .config_for_test()
    }

    fn user(text: &str) -> ModelRequest {
        let mut r = ModelRequest::new();
        r.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text { text: text.into() }],
        }];
        r
    }

    fn tool_def() -> ToolDef {
        ToolDef {
            name: "get_weather".into(),
            description: "Look up weather".into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {"city": {"type": "string"}}
            }),
        }
    }

    #[test]
    fn snap_plain_text_turn() {
        insta::assert_json_snapshot!(build_request(&cfg(), &user("hi")));
    }

    #[test]
    fn snap_system_prompt() {
        let mut r = user("hi");
        r.messages.insert(
            0,
            Item::System {
                content: vec![ContentPart::Text {
                    text: "You are terse.".into(),
                }],
            },
        );
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_tools_and_tool_choice_auto() {
        let mut r = user("weather?");
        r.tools = vec![tool_def()];
        r.model_settings.tool_choice = Some(ToolChoice::Auto);
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_tool_choice_named() {
        let mut r = user("weather?");
        r.tools = vec![tool_def()];
        r.model_settings.tool_choice = Some(ToolChoice::Tool {
            name: "get_weather".into(),
        });
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_tool_call_and_result() {
        let mut r = ModelRequest::new();
        r.messages = vec![
            Item::UserMessage {
                content: vec![ContentPart::Text { text: "weather?".into() }],
            },
            Item::ToolCall {
                call_id: "call_1".into(),
                name: "get_weather".into(),
                args: serde_json::json!({"city": "Berlin"}),
            },
            Item::ToolResult {
                call_id: "call_1".into(),
                content: vec![ContentPart::Text { text: "18C".into() }],
            },
        ];
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_structured_output_json_schema() {
        let mut r = user("give me json");
        r.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Answer".into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {"answer": {"type": "string"}}
            }),
            strict: true,
        });
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_inline_image_part() {
        let mut r = ModelRequest::new();
        r.messages = vec![Item::UserMessage {
            content: vec![
                ContentPart::Text { text: "what is this?".into() },
                ContentPart::Image {
                    source: MediaSource::Base64 {
                        mime_type: "image/png".into(),
                        data: "AAAA".into(),
                    },
                },
            ],
        }];
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_sampling_settings() {
        let mut r = user("hi");
        r.model_settings.temperature = Some(0.7);
        r.model_settings.top_p = Some(0.9);
        r.model_settings.max_output_tokens = Some(512);
        insta::assert_json_snapshot!(build_request(&cfg(), &r));
    }

    #[test]
    fn snap_litellm_extras() {
        let model = crate::LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .fallbacks(["backup-a", "backup-b"])
            .num_retries(2)
            .tags(["team:research"])
            .metadata("trace_id", "t-123")
            .extra_body(serde_json::json!({"guardrails": ["pii-check"]}))
            .build()
            .unwrap();
        insta::assert_json_snapshot!(build_request(&model.config_for_test(), &user("hi")));
    }

    // ── Invariants ──────────────────────────────────────────────────────

    #[test]
    fn parallel_tool_calls_is_never_sent() {
        let mut r = user("hi");
        r.tools = vec![tool_def()];
        let v = build_request(&cfg(), &r);
        assert!(v.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn previous_response_id_is_never_sent() {
        let mut r = user("hi");
        r.model_settings.previous_response_id = Some("resp_123".into());
        let v = build_request(&cfg(), &r);
        assert!(v.get("previous_response_id").is_none());
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains("resp_123"));
    }

    #[test]
    fn n_is_never_sent() {
        assert!(build_request(&cfg(), &user("hi")).get("n").is_none());
    }

    #[test]
    fn tool_choice_is_dropped_when_there_are_no_tools() {
        let mut r = user("hi");
        r.model_settings.tool_choice = Some(ToolChoice::Required);
        assert!(build_request(&cfg(), &r).get("tool_choice").is_none());
    }

    /// Catches "we added a new body field and forgot to reserve it".
    ///
    /// Every top-level key the translator can emit must be either reserved
    /// against `extra_body` or a known LiteLLM extra. A new field added to
    /// `build_request` without a matching `RESERVED_BODY_KEYS` entry would
    /// silently become forgeable by callers.
    #[test]
    fn every_emitted_top_level_key_is_reserved_or_a_known_extra() {
        const KNOWN_EXTRAS: &[&str] = &["fallbacks", "num_retries", "metadata", "tags"];

        let model = crate::LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .fallbacks(["b"])
            .num_retries(1)
            .tags(["t"])
            .metadata("trace_id", "x")
            .build()
            .unwrap();

        let mut r = user("hi");
        r.tools = vec![tool_def()];
        r.model_settings.tool_choice = Some(ToolChoice::Auto);
        r.model_settings.response_format = Some(ResponseFormat::JsonObject);
        r.model_settings.temperature = Some(0.5);
        r.model_settings.top_p = Some(0.9);
        r.model_settings.max_output_tokens = Some(64);

        let v = build_request(&model.config_for_test(), &r);
        for key in v.as_object().unwrap().keys() {
            assert!(
                RESERVED_BODY_KEYS.contains(&key.as_str()) || KNOWN_EXTRAS.contains(&key.as_str()),
                "body key `{key}` is neither reserved nor a known LiteLLM extra \
                 — add it to RESERVED_BODY_KEYS or to KNOWN_EXTRAS here"
            );
        }
    }
}
```

- [ ] **Step 4: Add the test accessor**

In `src/lib.rs`, add to `impl LiteLlmModel`:

```rust
    #[cfg(test)]
    pub(crate) fn config_for_test(&self) -> builder::Config {
        builder::Config {
            http: self.0.http.clone(),
            endpoint: self.0.endpoint.clone(),
            model_id: self.0.model_id.clone(),
            auth: self.0.auth.clone(),
            headers: self.0.headers.clone(),
            capabilities: self.0.capabilities,
            extras: self.0.extras.clone(),
        }
    }
```

- [ ] **Step 5: Run the tests and accept the snapshots**

Run: `cargo test -p paigasus-helikon-providers-litellm`
Expected: snapshot tests FAIL first with "snapshot missing".

Then: `cargo insta accept --workspace` (or review with `cargo insta review`).

**Before accepting, read each `.snap` file.** Confirm: `stream: true` and
`stream_options.include_usage: true` are present in every one; `tags` appears
only under `metadata`; no `parallel_tool_calls`, `n`, or `previous_response_id`
appears anywhere.

Re-run: `cargo test -p paigasus-helikon-providers-litellm`
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-litellm --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src
git commit -m "feat(providers): SMA-451 assemble litellm request body with extras"
```

---

### Task 7: SSE chunk types and the streaming translator

**Files:**
- Create: `crates/paigasus-helikon-providers-litellm/src/sse.rs`
- Create: `crates/paigasus-helikon-providers-litellm/src/stream.rs`
- Modify: `crates/paigasus-helikon-providers-litellm/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `sse::StreamChunk` (+ `Choice`, `Delta`, `ToolCallChunk`, `FunctionChunk`, `Usage`, `PromptTokensDetails`, `CompletionTokensDetails`) — all `pub(crate)`, all fields `#[serde(default)]`.
  - `stream::ChatTranslator::new()`, `::consume(&mut self, chunk: StreamChunk) -> Vec<ModelEvent>`, `::finish(&mut self) -> Vec<ModelEvent>`.

**This is the highest-risk task in the plan.** Two things must hold:
`Finish` is emitted **only** from `finish()`, and the tool-call state machine
must survive an `id` that arrives after `name`/`arguments`.

- [ ] **Step 1: Write `sse.rs`**

```rust
//! Permissive serde types for one LiteLLM Chat Completions SSE chunk.
//!
//! Every field is `#[serde(default)]` — including `choices`. Measured against
//! LiteLLM 1.97.0: the first delta carries an extra `role`, the finish chunk
//! has `delta: {}`, and the trailing usage chunk has
//! `choices: [{"index":0,"delta":{}}]` with no `finish_reason` at all. A
//! backend behind the proxy may omit more than that, and a single missing
//! field would otherwise fail the whole chunk. See the SMA-451 design §9.1.

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct StreamChunk {
    pub(crate) choices: Vec<Choice>,
    pub(crate) usage: Option<Usage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Choice {
    pub(crate) index: Option<u32>,
    pub(crate) delta: Option<Delta>,
    pub(crate) finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Delta {
    pub(crate) content: Option<String>,
    /// LiteLLM normalises Anthropic extended thinking and DeepSeek reasoning
    /// into this field.
    pub(crate) reasoning_content: Option<String>,
    /// Fallback spelling seen on some builds/backends.
    pub(crate) reasoning: Option<String>,
    pub(crate) tool_calls: Option<Vec<ToolCallChunk>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ToolCallChunk {
    pub(crate) index: Option<u32>,
    pub(crate) id: Option<String>,
    pub(crate) function: Option<FunctionChunk>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct FunctionChunk {
    pub(crate) name: Option<String>,
    pub(crate) arguments: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Usage {
    pub(crate) prompt_tokens: u32,
    pub(crate) completion_tokens: u32,
    /// Absent entirely in observed LiteLLM traffic — the whole object, not
    /// just the field.
    pub(crate) prompt_tokens_details: Option<PromptTokensDetails>,
    pub(crate) completion_tokens_details: Option<CompletionTokensDetails>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct PromptTokensDetails {
    pub(crate) cached_tokens: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CompletionTokensDetails {
    pub(crate) reasoning_tokens: Option<u32>,
}
```

- [ ] **Step 2: Write the failing translator tests**

`src/stream.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(v: serde_json::Value) -> StreamChunk {
        serde_json::from_value(v).expect("chunk must deserialize")
    }

    fn texts(evs: &[ModelEvent]) -> Vec<String> {
        evs.iter()
            .filter_map(|e| match e {
                ModelEvent::TokenDelta { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn content_deltas_become_token_deltas() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hel"}}]
        })));
        assert_eq!(texts(&evs), vec!["Hel"]);
    }

    #[test]
    fn empty_content_emits_nothing() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"content": ""}}]
        })));
        assert!(evs.is_empty());
    }

    #[test]
    fn reasoning_content_becomes_reasoning_delta() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"reasoning_content": "thinking"}}]
        })));
        assert!(matches!(&evs[0], ModelEvent::ReasoningDelta { text } if text == "thinking"));
    }

    #[test]
    fn reasoning_fallback_field_is_honoured() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"reasoning": "alt"}}]
        })));
        assert!(matches!(&evs[0], ModelEvent::ReasoningDelta { text } if text == "alt"));
    }

    #[test]
    fn finish_is_not_emitted_inline() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        })));
        assert!(
            !evs.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
            "Finish must be deferred to finish(), never emitted inline"
        );
    }

    #[test]
    fn trailing_usage_chunk_then_finish_preserves_ordering() {
        // The exact shape captured from LiteLLM 1.97.0: the usage snapshot
        // arrives in its own chunk AFTER the finish_reason chunk.
        let mut t = ChatTranslator::new();
        let mut all = Vec::new();
        all.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"content": "hi"}}]
        }))));
        all.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
        }))));
        all.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {}}],
            "usage": {"prompt_tokens": 8, "completion_tokens": 6,
                      "completion_tokens_details": {"reasoning_tokens": 0}}
        }))));
        all.extend(t.finish());

        let last = all.last().unwrap();
        assert!(
            matches!(last, ModelEvent::Finish { .. }),
            "Finish must be the terminal event, got {last:?}"
        );
        let usage_pos = all
            .iter()
            .position(|e| matches!(e, ModelEvent::Usage { .. }))
            .expect("usage must be emitted");
        assert!(usage_pos < all.len() - 1, "Usage must precede Finish");
    }

    #[test]
    fn usage_maps_all_token_fields() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 10, "completion_tokens": 4,
                "prompt_tokens_details": {"cached_tokens": 3},
                "completion_tokens_details": {"reasoning_tokens": 2}
            }
        })));
        match &evs[0] {
            ModelEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                reasoning_tokens,
            } => {
                assert_eq!(*input_tokens, 10);
                assert_eq!(*output_tokens, 4);
                assert_eq!(*cached_input_tokens, Some(3));
                assert_eq!(*reasoning_tokens, Some(2));
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn usage_without_details_objects_still_maps() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [], "usage": {"prompt_tokens": 1, "completion_tokens": 2}
        })));
        match &evs[0] {
            ModelEvent::Usage { cached_input_tokens, reasoning_tokens, .. } => {
                assert!(cached_input_tokens.is_none());
                assert!(reasoning_tokens.is_none());
            }
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn finish_reasons_map_leniently() {
        for (raw, expected) in [
            ("stop", FinishReason::Stop),
            ("length", FinishReason::Length),
            ("tool_calls", FinishReason::ToolCalls),
            ("function_call", FinishReason::ToolCalls),
            ("content_filter", FinishReason::ContentFilter),
        ] {
            let mut t = ChatTranslator::new();
            t.consume(chunk(serde_json::json!({
                "choices": [{"index": 0, "delta": {}, "finish_reason": raw}]
            })));
            let evs = t.finish();
            assert!(matches!(&evs[0], ModelEvent::Finish { reason } if *reason == expected));
        }
    }

    #[test]
    fn unknown_finish_reason_lands_in_other() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "guardrail_intervened"}]
        })));
        let evs = t.finish();
        match &evs[0] {
            ModelEvent::Finish { reason } => {
                assert_eq!(*reason, FinishReason::Other("guardrail_intervened".into()));
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn truncated_stream_emits_no_finish() {
        let mut t = ChatTranslator::new();
        t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"content": "partial"}}]
        })));
        assert!(
            t.finish().is_empty(),
            "a stream that never sent finish_reason must not fabricate Finish"
        );
    }

    #[test]
    fn tool_call_name_is_emitted_once_then_args_follow() {
        let mut t = ChatTranslator::new();
        let mut evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call_1", "function": {"name": "get_weather", "arguments": "{\"ci"}}
            ]}}]
        })));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "ty\":\"Berlin\"}"}}
            ]}}]
        }))));

        let calls: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta { call_id, name, args_delta } => {
                    Some((call_id.clone(), name.clone(), args_delta.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "call_1");
        assert_eq!(calls[0].1, Some("get_weather".to_owned()));
        assert_eq!(calls[1].1, None, "name must be emitted only once");
        let joined: String = calls.iter().map(|c| c.2.clone()).collect();
        assert_eq!(joined, "{\"city\":\"Berlin\"}");
    }

    #[test]
    fn tool_call_id_arriving_late_does_not_lose_name_or_args() {
        // The id is NOT guaranteed to arrive on the first delta.
        let mut t = ChatTranslator::new();
        let mut evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "sea", "arguments": "{\"q"}}
            ]}}]
        })));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "function": {"name": "rch", "arguments": "\":1}"}}
            ]}}]
        }))));
        evs.extend(t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call_late"}
            ]}}]
        }))));

        let calls: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta { call_id, name, args_delta } => {
                    Some((call_id.clone(), name.clone(), args_delta.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 1, "buffered until the id was known");
        assert_eq!(calls[0].0, "call_late");
        assert_eq!(
            calls[0].1,
            Some("search".to_owned()),
            "fragmented name must be concatenated"
        );
        assert_eq!(calls[0].2, "{\"q\":1}");
    }

    #[test]
    fn two_tool_call_indices_stay_separate() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "a", "function": {"name": "f", "arguments": "{}"}},
                {"index": 1, "id": "b", "function": {"name": "g", "arguments": "{}"}}
            ]}}]
        })));
        let ids: Vec<_> = evs
            .iter()
            .filter_map(|e| match e {
                ModelEvent::ToolCallDelta { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn tool_call_without_index_falls_back_to_id() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"id": "only_id", "function": {"name": "f", "arguments": "{}"}}
            ]}}]
        })));
        assert!(evs.iter().any(|e| matches!(
            e, ModelEvent::ToolCallDelta { call_id, .. } if call_id == "only_id"
        )));
    }

    #[test]
    fn only_the_first_choice_is_read() {
        let mut t = ChatTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "choices": [
                {"index": 0, "delta": {"content": "first"}},
                {"index": 1, "delta": {"content": "second"}}
            ]
        })));
        assert_eq!(texts(&evs), vec!["first"]);
    }

    #[test]
    fn chunk_with_no_choices_key_deserializes() {
        // Error/keepalive frames omit `choices` entirely.
        let c: StreamChunk = serde_json::from_str("{}").expect("must not fail");
        assert!(c.choices.is_empty());
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-providers-litellm stream`
Expected: FAIL — `ChatTranslator` does not exist.

- [ ] **Step 4: Write the translator**

Prepend to `src/stream.rs`:

```rust
//! SSE chunk → [`ModelEvent`] translation.
//!
//! Two invariants carry this module:
//!
//! 1. **`Finish` is emitted only from [`ChatTranslator::finish`]**, called at
//!    `[DONE]`/EOF — never inline with a chunk. With
//!    `stream_options.include_usage`, the usage snapshot arrives in a chunk
//!    *after* the one carrying `finish_reason`, so an inline `Finish` would be
//!    followed by `Usage` and violate core's "Finish is the terminal event"
//!    contract on every turn.
//! 2. **Tool-call `name`/`arguments` are buffered until the `id` is known**,
//!    because the id is not guaranteed to arrive first, and both fields
//!    fragment across deltas.

use std::collections::{HashMap, HashSet};

use paigasus_helikon_core::{FinishReason, ModelEvent};

use crate::sse::{StreamChunk, ToolCallChunk};

/// Correlation key for a streaming tool call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Index(u32),
    Id(String),
    Position(usize),
}

/// Name/args fragments that arrived before the `id` was known.
#[derive(Default)]
struct Pending {
    name: String,
    args: String,
}

/// Accumulates SSE deltas and produces [`ModelEvent`]s.
pub(crate) struct ChatTranslator {
    tool_calls: HashMap<Key, String>,
    name_emitted: HashSet<Key>,
    pending: HashMap<Key, Pending>,
    finish_reason: Option<String>,
    warned_multi_choice: bool,
}

impl ChatTranslator {
    pub(crate) fn new() -> Self {
        Self {
            tool_calls: HashMap::new(),
            name_emitted: HashSet::new(),
            pending: HashMap::new(),
            finish_reason: None,
            warned_multi_choice: false,
        }
    }

    /// Consume one chunk. Never emits `Finish`.
    pub(crate) fn consume(&mut self, chunk: StreamChunk) -> Vec<ModelEvent> {
        let mut out = Vec::new();

        if chunk.choices.len() > 1 && !self.warned_multi_choice {
            self.warned_multi_choice = true;
            tracing::warn!(
                target: "paigasus::litellm::stream",
                n = chunk.choices.len(),
                "response carries multiple choices; only the first is read"
            );
        }

        if let Some(choice) = chunk.choices.first() {
            if let Some(delta) = &choice.delta {
                if let Some(text) = delta.content.as_deref().filter(|s| !s.is_empty()) {
                    out.push(ModelEvent::TokenDelta { text: text.to_owned() });
                }
                let reasoning = delta
                    .reasoning_content
                    .as_deref()
                    .or(delta.reasoning.as_deref())
                    .filter(|s| !s.is_empty());
                if let Some(text) = reasoning {
                    out.push(ModelEvent::ReasoningDelta { text: text.to_owned() });
                }
                if let Some(tcs) = &delta.tool_calls {
                    for (pos, tc) in tcs.iter().enumerate() {
                        self.handle_tool_call(tc, pos, &mut out);
                    }
                }
            }
            if let Some(reason) = &choice.finish_reason {
                // Buffered — see the module docs.
                self.finish_reason = Some(reason.clone());
            }
        }

        if let Some(u) = &chunk.usage {
            out.push(ModelEvent::Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                cached_input_tokens: u
                    .prompt_tokens_details
                    .as_ref()
                    .and_then(|d| d.cached_tokens),
                reasoning_tokens: u
                    .completion_tokens_details
                    .as_ref()
                    .and_then(|d| d.reasoning_tokens),
            });
        }

        out
    }

    fn handle_tool_call(&mut self, tc: &ToolCallChunk, pos: usize, out: &mut Vec<ModelEvent>) {
        let key = match (tc.index, tc.id.as_deref()) {
            (Some(i), _) => Key::Index(i),
            (None, Some(id)) => Key::Id(id.to_owned()),
            (None, None) => {
                tracing::debug!(
                    target: "paigasus::litellm::stream",
                    pos,
                    "tool-call delta has neither index nor id; correlating by position"
                );
                Key::Position(pos)
            }
        };

        let name_frag = tc.function.as_ref().and_then(|f| f.name.as_deref());
        let args_frag = tc.function.as_ref().and_then(|f| f.arguments.as_deref());

        if let Some(id) = tc.id.as_deref() {
            self.tool_calls.entry(key.clone()).or_insert_with(|| id.to_owned());
        }

        let Some(call_id) = self.tool_calls.get(&key).cloned() else {
            // No id yet — buffer both fragments.
            let slot = self.pending.entry(key).or_default();
            if let Some(n) = name_frag {
                slot.name.push_str(n);
            }
            if let Some(a) = args_frag {
                slot.args.push_str(a);
            }
            return;
        };

        let buffered = self.pending.remove(&key).unwrap_or_default();
        let mut name = buffered.name;
        if let Some(n) = name_frag {
            name.push_str(n);
        }
        let mut args = buffered.args;
        if let Some(a) = args_frag {
            args.push_str(a);
        }

        let emit_name = if self.name_emitted.contains(&key) {
            None
        } else if name.is_empty() {
            None
        } else {
            self.name_emitted.insert(key.clone());
            Some(name)
        };

        if emit_name.is_none() && args.is_empty() {
            return;
        }

        out.push(ModelEvent::ToolCallDelta {
            call_id,
            name: emit_name,
            args_delta: args,
        });
    }

    /// Emit the terminal `Finish`, if a `finish_reason` was ever observed.
    ///
    /// Emits nothing on a truncated stream: fabricating `Finish::Stop` would
    /// make a dropped connection indistinguishable from a clean completion,
    /// and `ModelTurnAccumulator` defaults to `Stop`, so the truncated text
    /// would be committed to session history as final.
    pub(crate) fn finish(&mut self) -> Vec<ModelEvent> {
        let Some(raw) = self.finish_reason.take() else {
            return Vec::new();
        };
        let reason = match raw.as_str() {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::Length,
            "tool_calls" | "function_call" => FinishReason::ToolCalls,
            "content_filter" => FinishReason::ContentFilter,
            other => FinishReason::Other(other.to_owned()),
        };
        vec![ModelEvent::Finish { reason }]
    }
}
```

- [ ] **Step 5: Register the modules and run**

In `src/lib.rs` add `mod sse;` and `mod stream;`.

Run: `cargo test -p paigasus-helikon-providers-litellm`
Expected: all PASS (17 stream tests + earlier ones).

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-litellm --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src
git commit -m "feat(providers): SMA-451 add litellm sse types and stream translator"
```

---

### Task 8: Error classification

**Files:**
- Create: `crates/paigasus-helikon-providers-litellm/src/error.rs`
- Modify: `crates/paigasus-helikon-providers-litellm/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `error::classify(status: u16, code: Option<&str>, err_type: Option<&str>, message: &str, retry_after_ms: Option<u64>) -> ModelError`
  - `error::parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64>`
  - `error::normalise_type(raw: Option<&str>) -> Option<&str>`

**Read §10.1 first.** The envelope is not what OpenAI's is: `error.code` is the
**HTTP status as a string** (`"400"`, `"429"`, `"500"`), and `error.type` is
often `null` or the literal string `"None"`. The reliable context-length marker
is the exception class name prefixed onto `message`.

- [ ] **Step 1: Write the failing tests**

`src/error.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_string_and_null_types_both_normalise_to_none() {
        assert_eq!(normalise_type(Some("None")), None);
        assert_eq!(normalise_type(None), None);
        assert_eq!(normalise_type(Some("throttling_error")), Some("throttling_error"));
    }

    #[test]
    fn context_window_exceeded_is_detected_by_class_name() {
        // Captured from LiteLLM 1.97.0: status 400, code "400", type null.
        let m = "litellm.ContextWindowExceededError: litellm.BadRequestError: \
                 this is a mock context window exceeded error";
        assert!(matches!(
            classify(400, Some("400"), None, m, None),
            ModelError::ContextLengthExceeded
        ));
    }

    #[test]
    fn context_length_exceeded_prose_fallback_still_works() {
        let m = "This model's maximum context length is 8192 tokens";
        assert!(matches!(
            classify(400, Some("400"), None, m, None),
            ModelError::ContextLengthExceeded
        ));
    }

    #[test]
    fn rate_limit_maps_with_retry_after() {
        let m = "litellm.RateLimitError: this is a mock rate limit error";
        match classify(429, Some("429"), Some("throttling_error"), m, Some(1500)) {
            ModelError::RateLimited { retry_after_ms } => assert_eq!(retry_after_ms, Some(1500)),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn budget_exhaustion_429_is_refused_not_rate_limited() {
        // A retry loop would burn its whole budget against a limit that will
        // never clear.
        let m = "litellm.BudgetExceededError: Budget has been exceeded for key";
        assert!(matches!(
            classify(429, Some("429"), Some("budget_exceeded"), m, Some(1000)),
            ModelError::Refused { .. }
        ));
    }

    #[test]
    fn five_hundred_is_unavailable_not_other() {
        // `Other` is not retryable under runtime-tokio's RetryPolicy, and 500
        // is the single most common transient proxy failure.
        let m = "litellm.InternalServerError: this is a mock internal server error";
        for status in [500, 502, 503, 504] {
            assert!(
                matches!(
                    classify(status, Some("500"), None, m, None),
                    ModelError::Unavailable
                ),
                "status {status} must map to Unavailable"
            );
        }
    }

    #[test]
    fn auth_failures_are_refused() {
        assert!(matches!(
            classify(401, Some("401"), None, "invalid key", None),
            ModelError::Refused { .. }
        ));
        assert!(matches!(
            classify(403, Some("403"), None, "forbidden", None),
            ModelError::Refused { .. }
        ));
    }

    #[test]
    fn db_less_bad_key_400_is_refused_not_other() {
        // Measured: a wrong virtual key on a DB-less deployment returns 400
        // with type `no_db_connection`, not 401.
        assert!(matches!(
            classify(400, Some("400"), Some("no_db_connection"), "No connected db.", None),
            ModelError::Refused { .. }
        ));
    }

    #[test]
    fn content_policy_is_refused() {
        assert!(matches!(
            classify(400, Some("400"), Some("content_policy_violation"), "blocked", None),
            ModelError::Refused { .. }
        ));
    }

    #[test]
    fn unknown_model_400_falls_through_to_other() {
        let m = "/chat/completions: Invalid model name passed in model=does-not-exist";
        match classify(400, Some("400"), Some("None"), m, None) {
            ModelError::Other(e) => assert!(e.to_string().contains("does-not-exist")),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn retry_after_seconds_header_parses() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("retry-after", "3".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&h), Some(3000));
    }

    #[test]
    fn retry_after_http_date_yields_none() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&h), None);
    }

    #[test]
    fn absent_retry_after_yields_none() {
        assert_eq!(parse_retry_after_ms(&reqwest::header::HeaderMap::new()), None);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p paigasus-helikon-providers-litellm error`
Expected: FAIL — `classify` not found.

- [ ] **Step 3: Write the implementation**

Prepend to `src/error.rs`:

```rust
//! Map LiteLLM HTTP errors onto core [`ModelError`] variants.
//!
//! The envelope is `{"error": {"message", "type", "param", "code"}}`, but two
//! of those fields do not mean what the OpenAI-shaped name suggests, measured
//! against LiteLLM 1.97.0:
//!
//! - **`code` is the HTTP status restated as a string** (`"400"`, `"429"`,
//!   `"500"`) — never a semantic identifier. Matching it against
//!   `"context_length_exceeded"` would never fire.
//! - **`type` is frequently `null`, and sometimes the literal string
//!   `"None"`.**
//!
//! The dependable signal is the LiteLLM exception class name, which is
//! prefixed onto `message` (`"litellm.ContextWindowExceededError: …"`).

use paigasus_helikon_core::ModelError;

/// Treat LiteLLM's stringified `"None"` as absent.
pub(crate) fn normalise_type(raw: Option<&str>) -> Option<&str> {
    raw.filter(|t| !t.is_empty() && *t != "None" && *t != "null")
}

/// Does this message indicate a context-window overflow?
fn is_context_overflow(message: &str) -> bool {
    if message.contains("ContextWindowExceededError") {
        return true;
    }
    let lc = message.to_ascii_lowercase();
    lc.contains("context_window_exceeded")
        || lc.contains("context_length_exceeded")
        || (lc.contains("maximum context length") || lc.contains("context window"))
}

/// Does this error indicate budget exhaustion rather than throttling?
fn is_budget_exhaustion(err_type: Option<&str>, message: &str) -> bool {
    let t = err_type.unwrap_or("").to_ascii_lowercase();
    if t.contains("budget") {
        return true;
    }
    let lc = message.to_ascii_lowercase();
    lc.contains("budgetexceeded") || lc.contains("budget has been exceeded")
}

/// Classify a LiteLLM error response.
///
/// Rules are evaluated top-down; the context-overflow check is deliberately
/// **first** and not gated on status, because its measured status is 400 and
/// keying it off a status would collide with every other 400.
pub(crate) fn classify(
    status: u16,
    code: Option<&str>,
    err_type: Option<&str>,
    message: &str,
    retry_after_ms: Option<u64>,
) -> ModelError {
    let err_type = normalise_type(err_type);

    if is_context_overflow(message) {
        return ModelError::ContextLengthExceeded;
    }

    match status {
        429 if is_budget_exhaustion(err_type, message) => ModelError::Refused {
            reason: message.to_owned(),
        },
        429 => ModelError::RateLimited { retry_after_ms },
        500 | 502 | 503 | 504 => ModelError::Unavailable,
        401 | 403 => ModelError::Refused {
            reason: message.to_owned(),
        },
        _ if matches!(
            err_type,
            Some("content_policy_violation") | Some("no_db_connection")
        ) =>
        {
            ModelError::Refused {
                reason: message.to_owned(),
            }
        }
        _ => ModelError::Other(anyhow::anyhow!(
            "litellm http {status}{}: {message}",
            code.map(|c| format!(" (code {c})")).unwrap_or_default()
        )),
    }
}

/// Parse `Retry-After` as whole seconds.
///
/// The HTTP-date form yields `None` — `RateLimited::retry_after_ms` is already
/// `Option`, so callers must handle absence regardless.
pub(crate) fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|s| s.saturating_mul(1000))
}
```

- [ ] **Step 4: Register the module and run**

In `src/lib.rs` add `mod error;`.

Run: `cargo test -p paigasus-helikon-providers-litellm error`
Expected: all 13 PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-litellm --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src
git commit -m "feat(providers): SMA-451 add litellm error classification"
```

---

### Task 9: The `Model` implementation

**Files:**
- Modify: `crates/paigasus-helikon-providers-litellm/src/lib.rs`
- Create: `crates/paigasus-helikon-providers-litellm/src/model.rs`

**Interfaces:**
- Consumes: everything from Tasks 1–8.
- Produces: `pub struct LiteLlmModel` implementing `paigasus_helikon_core::Model`; `LiteLlmModel::chat`, `LiteLlmModel::from_env`.

**Reference implementation:** `crates/paigasus-helikon-providers-gemini/src/model.rs:105-175`
already has the exact HTTP+SSE driving loop shape needed here, **including the
non-2xx check before SSE framing** (`:122-138`). Follow it closely; the
differences are the header set, the error-field extraction, and calling
`translator.finish()` at both `[DONE]` and EOF.

- [ ] **Step 1: Move `LiteLlmModel` out of `lib.rs` into `model.rs`**

Create `src/model.rs`:

```rust
//! `LiteLlmModel` — the public [`paigasus_helikon_core::Model`] implementation.

use std::sync::Arc;

use async_stream::stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::builder::{BuildError, Config, LiteLlmModelBuilder};
use crate::error::{classify, parse_retry_after_ms};
use crate::sse::StreamChunk;
use crate::stream::ChatTranslator;

/// LiteLLM proxy provider.
///
/// Construct via [`Self::chat`] or [`Self::from_env`]. Always streams.
#[derive(Debug, Clone)]
pub struct LiteLlmModel(pub(crate) Arc<Config>);

impl LiteLlmModel {
    /// Chat Completions builder for a proxy model alias.
    ///
    /// The alias is whatever the proxy operator configured — it need not be an
    /// OpenAI model id.
    pub fn chat(model_id: impl Into<String>) -> LiteLlmModelBuilder {
        LiteLlmModelBuilder::new(model_id)
    }

    /// Build from the ambient environment.
    ///
    /// Reads `LITELLM_API_BASE` (falling back to `LITELLM_PROXY_API_BASE`) and
    /// `LITELLM_API_KEY` (falling back to `LITELLM_PROXY_API_KEY`). The key is
    /// optional — an unset key yields an unauthenticated model, not an error —
    /// so the only failures are [`BuildError::MissingBaseUrl`] and
    /// [`BuildError::InvalidBaseUrl`].
    pub fn from_env(model_id: impl Into<String>) -> Result<Self, BuildError> {
        Self::chat(model_id).build()
    }

    pub(crate) fn from_config(cfg: Config) -> Self {
        Self(Arc::new(cfg))
    }

    #[cfg(test)]
    pub(crate) fn endpoint(&self) -> &str {
        &self.0.endpoint
    }

    #[cfg(test)]
    pub(crate) fn auth(&self) -> Option<&str> {
        self.0.auth.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn config_for_test(&self) -> Config {
        Config {
            http: self.0.http.clone(),
            endpoint: self.0.endpoint.clone(),
            model_id: self.0.model_id.clone(),
            auth: self.0.auth.clone(),
            headers: self.0.headers.clone(),
            capabilities: self.0.capabilities,
            extras: self.0.extras.clone(),
        }
    }
}

#[async_trait]
impl Model for LiteLlmModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let cfg = self.0.clone();
        let body = crate::translate::build_request(&cfg, &request);

        let s = stream! {
            let mut req = cfg.http
                .post(&cfg.endpoint)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::ACCEPT, "text/event-stream");

            if let Some(key) = &cfg.auth {
                req = req.header(reqwest::header::AUTHORIZATION, format!("Bearer {key}"));
            }
            if let Some(n) = cfg.extras.num_retries {
                // Also sent in the body. Upstream documents the header as
                // outranking the body, so the two cannot disagree.
                req = req.header("x-litellm-num-retries", n.to_string());
            }
            // Caller headers last, so `.header()` is a genuine escape hatch.
            for (name, value) in &cfg.headers {
                req = req.header(name.as_str(), value.as_str());
            }

            let send_fut = req.json(&body).send();

            let response = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                r = send_fut => match r {
                    Ok(r) => r,
                    Err(e) => { yield Err(ModelError::Transport(e.to_string())); return; }
                },
            };

            let status = response.status();
            let headers = response.headers().clone();

            // Correlation ids, logged on every response including streaming
            // ones. A routing chokepoint with no call id is not debuggable.
            let call_id = headers
                .get("x-litellm-call-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            tracing::debug!(
                target: "paigasus::litellm::http",
                status = status.as_u16(),
                call_id = %call_id,
                model_id = headers.get("x-litellm-model-id").and_then(|v| v.to_str().ok()).unwrap_or(""),
                attempted_retries = headers.get("x-litellm-attempted-retries").and_then(|v| v.to_str().ok()).unwrap_or(""),
                attempted_fallbacks = headers.get("x-litellm-attempted-fallbacks").and_then(|v| v.to_str().ok()).unwrap_or(""),
                "litellm response"
            );

            // A failing request returns non-2xx JSON, NOT an SSE stream —
            // check before entering the framing loop.
            let is_sse = headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.starts_with("text/event-stream"));

            if !status.is_success() || !is_sse {
                let retry_after_ms = parse_retry_after_ms(&headers);
                let bytes = response.bytes().await.unwrap_or_default();
                let err = error_from_body(status.as_u16(), &bytes, retry_after_ms, &call_id);
                yield Err(err);
                return;
            }

            let mut events = response.bytes_stream().eventsource();
            let mut translator = ChatTranslator::new();

            loop {
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    n = events.next() => n,
                };
                match next {
                    None => {
                        for ev in translator.finish() { yield Ok(ev); }
                        return;
                    }
                    Some(Err(e)) => {
                        yield Err(ModelError::Transport(e.to_string()));
                        return;
                    }
                    Some(Ok(event)) => {
                        if event.data == "[DONE]" {
                            for ev in translator.finish() { yield Ok(ev); }
                            return;
                        }
                        // Defensive: a backend failing mid-generation can emit
                        // an error frame. Unverified — every reproducible
                        // failure returns non-2xx JSON before the stream opens.
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&event.data) {
                            if v.get("error").is_some() {
                                yield Err(error_from_body(
                                    500, event.data.as_bytes(), None, &call_id,
                                ));
                                return;
                            }
                        }
                        let chunk: StreamChunk = match serde_json::from_str(&event.data) {
                            Ok(c) => c,
                            Err(parse_err) => {
                                tracing::warn!(
                                    target: "paigasus::litellm::sse",
                                    %parse_err,
                                    event_len = event.data.len(),
                                    "unparseable SSE event payload; skipping"
                                );
                                continue;
                            }
                        };
                        for ev in translator.consume(chunk) {
                            yield Ok(ev);
                        }
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.0.capabilities
    }

    fn provider(&self) -> &str {
        "litellm"
    }

    fn model(&self) -> &str {
        &self.0.model_id
    }
}

/// Extract LiteLLM's error envelope and classify it.
fn error_from_body(
    status: u16,
    bytes: &[u8],
    retry_after_ms: Option<u64>,
    call_id: &str,
) -> ModelError {
    let parsed: Option<serde_json::Value> = serde_json::from_slice(bytes).ok();
    let (code, err_type, message) = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .map(|e| {
            let as_str = |k: &str| e.get(k).and_then(|x| x.as_str()).map(str::to_owned);
            (
                as_str("code"),
                as_str("type"),
                as_str("message").unwrap_or_default(),
            )
        })
        .unwrap_or_else(|| (None, None, String::from_utf8_lossy(bytes).into_owned()));

    let message = if call_id.is_empty() {
        message
    } else {
        format!("{message} (x-litellm-call-id: {call_id})")
    };

    classify(
        status,
        code.as_deref(),
        err_type.as_deref(),
        &message,
        retry_after_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_and_model_getters() {
        let m = LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .api_key("sk-x")
            .build()
            .unwrap();
        assert_eq!(m.provider(), "litellm");
        assert_eq!(m.model(), "prod-fast");
    }

    #[test]
    fn capabilities_default_to_streaming_and_tools() {
        let m = LiteLlmModel::chat("prod-fast")
            .base_url("http://p:4000")
            .build()
            .unwrap();
        assert!(m.capabilities().streaming);
        assert!(m.capabilities().tools);
        assert!(!m.capabilities().vision);
    }
}
```

- [ ] **Step 2: Rewrite `lib.rs` to its final form**

```rust
//! LiteLLM proxy provider for the Paigasus Helikon SDK.
//!
//! Talks to a [LiteLLM](https://docs.litellm.ai) proxy over its
//! OpenAI-compatible Chat Completions endpoint, adding LiteLLM's own router
//! and observability fields. See [SMA-451] for the design.
//!
//! # When to use this instead of the OpenAI provider
//!
//! `OpenAiModel::base_url()` can also point at a LiteLLM proxy, and is the
//! better choice when the proxy simply fronts OpenAI models. Reach for this
//! crate when you need LiteLLM's router fallbacks and retries, spend/trace
//! metadata, reasoning streaming from non-OpenAI backends, or arbitrary
//! operator-chosen model aliases.
//!
//! # Capabilities are your declaration
//!
//! A LiteLLM alias (`prod-fast`, `team-a/gpt`) carries no information the SDK
//! can act on, so [`LiteLlmModel`] defaults to a conservative
//! streaming + tools capability set. Declare what the backend actually
//! supports with `with_capabilities`.
//!
//! # Quick start
//!
//! ```ignore
//! // Ignored under doctest: the example needs a reachable proxy.
//! use paigasus_helikon_providers_litellm::LiteLlmModel;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let _model = LiteLlmModel::chat("claude-sonnet-4")
//!     .base_url("http://litellm:4000")
//!     .api_key("sk-…")
//!     .fallbacks(["gpt-4o-mini"])
//!     .num_retries(2)
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! # Limitations
//!
//! Chat Completions only — there is no Responses backend, and
//! `ModelSettings::previous_response_id` is ignored. The provider always
//! streams. Multi-choice responses (`n > 1`) are not supported: only the first
//! choice is read.
//!
//! [SMA-451]: https://linear.app/smaschek/issue/SMA-451

mod builder;
mod capabilities;
mod error;
mod model;
mod sse;
mod stream;
mod transport;
mod translate;

pub use builder::{BuildError, LiteLlmModelBuilder};
pub use model::LiteLlmModel;
```

- [ ] **Step 3: Run the full crate test suite**

Run: `cargo test -p paigasus-helikon-providers-litellm`
Expected: everything PASSES.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-litellm --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/src
git commit -m "feat(providers): SMA-451 implement the litellm model trait"
```

---

### Task 10: Integration tests (wire, streaming, cancellation, live)

**Files:**
- Create: `crates/paigasus-helikon-providers-litellm/tests/litellm_wire.rs`
- Create: `crates/paigasus-helikon-providers-litellm/tests/streaming.rs`
- Create: `crates/paigasus-helikon-providers-litellm/tests/cancellation.rs`
- Create: `crates/paigasus-helikon-providers-litellm/tests/live.rs`
- Create: `crates/paigasus-helikon-providers-litellm/tests/fixtures/*.txt`
- Modify: `.gitattributes` (repo root)

**Interfaces:**
- Consumes: the public crate surface only.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Extend the root `.gitattributes`**

Add beneath the existing anthropic fixture rule:

```
crates/paigasus-helikon-providers-litellm/tests/fixtures/*.txt text eol=lf
```

- [ ] **Step 2: Create the SSE fixtures**

These are transcriptions of traffic captured from LiteLLM 1.97.0 — **not
hand-invented shapes**. The design's ordering bug survived a review precisely
because a hand-written fixture encoded a shape the wire does not produce.

`tests/fixtures/text_then_trailing_usage.txt`:

```
data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1786867514,"model":"mock-fast","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1786867514,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"lo"}}]}

data: {"id":"chatcmpl-1","object":"chat.completion.chunk","created":1786867514,"model":"mock-fast","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"id":"chatcmpl-1","created":1786867514,"model":"mock-fast","object":"chat.completion.chunk","choices":[{"index":0,"delta":{}}],"usage":{"completion_tokens":6,"prompt_tokens":8,"total_tokens":14,"completion_tokens_details":{"reasoning_tokens":0}}}

data: [DONE]

```

`tests/fixtures/truncated_no_finish.txt`:

```
data: {"id":"chatcmpl-2","object":"chat.completion.chunk","created":1786867514,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"partial"}}]}

```

`tests/fixtures/unknown_finish_reason.txt`:

```
data: {"id":"chatcmpl-3","object":"chat.completion.chunk","created":1786867514,"model":"mock-fast","choices":[{"index":0,"delta":{"content":"hi"}}]}

data: {"id":"chatcmpl-3","object":"chat.completion.chunk","created":1786867514,"model":"mock-fast","choices":[{"index":0,"delta":{},"finish_reason":"guardrail_intervened"}]}

data: [DONE]

```

`tests/fixtures/unparseable_frame.txt`:

```
data: {"id":"chatcmpl-4","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"before"}}]}

data: {this is not json

data: {"id":"chatcmpl-4","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"after"}}]}

data: {"id":"chatcmpl-4","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]

```

- [ ] **Step 3: Write `tests/litellm_wire.rs`**

```rust
//! Wire-format / transport tests for the LiteLLM provider.

use futures_util::StreamExt;
use paigasus_helikon_core::{CancellationToken, ContentPart, Item, Model, ModelRequest};
use paigasus_helikon_providers_litellm::LiteLlmModel;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse_ok() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n\
             data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
            "text/event-stream",
        )
}

fn user(s: &str) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text { text: s.into() }],
    }];
    r
}

async fn drain(model: &LiteLlmModel) {
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();
    while s.next().await.is_some() {}
}

#[tokio::test]
async fn posts_to_v1_chat_completions_with_sse_accept() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("accept", "text/event-stream"))
        .and(header("content-type", "application/json"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .api_key("sk-test")
        .build()
        .unwrap();
    drain(&model).await;
}

#[tokio::test]
async fn base_url_already_ending_in_v1_does_not_double_the_segment() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(format!("{}/v1", server.uri()))
        .build()
        .unwrap();
    drain(&model).await;
}

#[tokio::test]
async fn authorization_header_is_sent_when_a_key_is_configured() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .api_key("sk-test")
        .build()
        .unwrap();
    drain(&model).await;
}

/// The security-relevant assertion for optional auth.
///
/// **This must inspect `received_requests()`.** Wiremock has no negative
/// header matcher, so a `Mock::given(method("POST"))` with no header condition
/// matches whether or not the header was sent — an implementation that always
/// sent auth would pass such a test.
#[tokio::test]
async fn no_authorization_header_when_no_key_is_configured() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    // Ensure ambient env vars cannot supply a key.
    for k in ["LITELLM_API_KEY", "LITELLM_PROXY_API_KEY"] {
        std::env::remove_var(k);
    }

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();
    drain(&model).await;

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].headers.get("authorization").is_none(),
        "no Authorization header must be sent when no key is configured"
    );
}

#[tokio::test]
async fn num_retries_is_sent_in_both_body_and_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .num_retries(3)
        .build()
        .unwrap();
    drain(&model).await;

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["num_retries"], 3);
    assert_eq!(
        requests[0].headers.get("x-litellm-num-retries").unwrap(),
        "3"
    );
}

#[tokio::test]
async fn custom_headers_are_passed_through() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("x-litellm-tags", "free"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .header("x-litellm-tags", "free")
        .build()
        .unwrap();
    drain(&model).await;
}

/// A failing request returns non-2xx JSON, not an SSE stream.
#[tokio::test]
async fn non_sse_error_response_yields_a_single_classified_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(500)
                .insert_header("content-type", "application/json")
                .set_body_raw(
                    r#"{"error":{"message":"litellm.InternalServerError: mock","type":null,"param":null,"code":"500"}}"#,
                    "application/json",
                ),
        )
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();

    let first = s.next().await.expect("one event");
    assert!(matches!(
        first,
        Err(paigasus_helikon_core::ModelError::Unavailable)
    ));
    assert!(s.next().await.is_none(), "error must terminate the stream");
}

#[tokio::test]
async fn rate_limit_carries_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("content-type", "application/json")
                .insert_header("retry-after", "2")
                .set_body_raw(
                    r#"{"error":{"message":"litellm.RateLimitError: mock","type":"throttling_error","code":"429"}}"#,
                    "application/json",
                ),
        )
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();

    match s.next().await.unwrap() {
        Err(paigasus_helikon_core::ModelError::RateLimited { retry_after_ms }) => {
            assert_eq!(retry_after_ms, Some(2000));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}
```

- [ ] **Step 4: Write `tests/streaming.rs`**

```rust
//! SSE → `ModelEvent` translation, driven through the real HTTP path.
//!
//! Fixtures are transcribed from traffic captured against LiteLLM 1.97.0.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_litellm::LiteLlmModel;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn user(s: &str) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text { text: s.into() }],
    }];
    r
}

async fn events_for(fixture: &str) -> Vec<ModelEvent> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(fixture.to_owned(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model
        .invoke(user("hi"), CancellationToken::new())
        .await
        .unwrap();

    let mut out = Vec::new();
    while let Some(ev) = s.next().await {
        out.push(ev.expect("no error expected"));
    }
    out
}

/// The regression test for the core ordering contract.
#[tokio::test]
async fn usage_arrives_before_finish_even_though_it_is_a_later_chunk() {
    let evs = events_for(include_str!("fixtures/text_then_trailing_usage.txt")).await;

    let last = evs.last().expect("at least one event");
    assert!(
        matches!(last, ModelEvent::Finish { .. }),
        "Finish must be terminal, got {last:?}"
    );
    let usage_pos = evs
        .iter()
        .position(|e| matches!(e, ModelEvent::Usage { .. }))
        .expect("Usage must be emitted");
    assert_eq!(usage_pos, evs.len() - 2, "Usage must immediately precede Finish");

    let text: String = evs
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TokenDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");
}

#[tokio::test]
async fn truncated_stream_emits_no_finish() {
    let evs = events_for(include_str!("fixtures/truncated_no_finish.txt")).await;
    assert!(
        !evs.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
        "a truncated stream must not be reported as a clean completion"
    );
}

#[tokio::test]
async fn unknown_finish_reason_lands_in_other() {
    let evs = events_for(include_str!("fixtures/unknown_finish_reason.txt")).await;
    match evs.last().unwrap() {
        ModelEvent::Finish { reason } => {
            assert_eq!(*reason, FinishReason::Other("guardrail_intervened".into()));
        }
        other => panic!("expected Finish, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unparseable_frame_is_skipped_without_killing_the_stream() {
    let evs = events_for(include_str!("fixtures/unparseable_frame.txt")).await;
    let text: String = evs
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TokenDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "beforeafter", "text on both sides of the bad frame survives");
    assert!(matches!(evs.last().unwrap(), ModelEvent::Finish { .. }));
}
```

- [ ] **Step 5: Write `tests/cancellation.rs`**

```rust
//! Cancellation is honoured before the request future resolves.
//!
//! A true mid-stream cancel is deliberately not tested: wiremock serves the
//! whole body in one `set_body_raw`, so there is no pacing to interrupt — the
//! same limitation the OpenAI provider's streaming tests document.

use futures_util::StreamExt;
use paigasus_helikon_core::{CancellationToken, ContentPart, Item, Model, ModelRequest};
use paigasus_helikon_providers_litellm::LiteLlmModel;
use std::time::Duration;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn cancel_before_response_yields_an_empty_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_delay(Duration::from_secs(30))
                .set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let model = LiteLlmModel::chat("prod-fast")
        .base_url(server.uri())
        .build()
        .unwrap();

    let mut req = ModelRequest::new();
    req.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text { text: "hi".into() }],
    }];

    let cancel = CancellationToken::new();
    let mut s = model.invoke(req, cancel.clone()).await.unwrap();
    cancel.cancel();

    // Per core's contract, a cancelled stream ends WITHOUT emitting Finish.
    let next = tokio::time::timeout(Duration::from_secs(5), s.next())
        .await
        .expect("cancellation must not hang");
    assert!(next.is_none(), "cancelled stream must end immediately");
}
```

- [ ] **Step 6: Write `tests/live.rs`**

```rust
//! Live tests against a real LiteLLM proxy.
//!
//! Env-gated: set `LITELLM_API_BASE` (and optionally `LITELLM_API_KEY`) to
//! run. Loud-skips otherwise so `cargo test` stays green without a proxy.
//!
//! A keyless rig is enough — LiteLLM `mock_response` deployments serve real
//! streaming SSE with a fake upstream key. See the SMA-451 design Appendix B
//! for the config, and SMA-523 for the CI job that will run this.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest,
};
use paigasus_helikon_providers_litellm::LiteLlmModel;

fn gate() -> Option<String> {
    match std::env::var("LITELLM_API_BASE") {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => {
            eprintln!(
                "SKIP: LITELLM_API_BASE not set — skipping live LiteLLM test. \
                 See docs/superpowers/specs/2026-08-16-sma-451-litellm-provider-design.md Appendix B."
            );
            None
        }
    }
}

fn model_id() -> String {
    std::env::var("LITELLM_TEST_MODEL").unwrap_or_else(|_| "mock-fast".to_owned())
}

#[tokio::test]
async fn live_streaming_turn_ends_with_finish_after_usage() {
    let Some(base) = gate() else { return };

    let model = LiteLlmModel::chat(model_id())
        .base_url(base)
        .build()
        .expect("build against live proxy");

    let mut req = ModelRequest::new();
    req.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text { text: "say hi".into() }],
    }];

    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let mut evs = Vec::new();
    while let Some(ev) = s.next().await {
        evs.push(ev.expect("live stream must not error"));
    }

    assert!(
        matches!(evs.last(), Some(ModelEvent::Finish { .. })),
        "Finish must be the terminal event against a real proxy"
    );
    assert!(
        evs.iter().any(|e| matches!(e, ModelEvent::Usage { .. })),
        "include_usage should produce a Usage event"
    );
}
```

- [ ] **Step 7: Run everything**

Run: `cargo test -p paigasus-helikon-providers-litellm`
Expected: all unit + integration tests PASS; `live.rs` prints its SKIP line.

- [ ] **Step 8: Mutation-check the auth test**

This is the one test whose failure mode is silent. Temporarily make
`model.rs` send `Authorization` unconditionally (e.g. `Bearer x` when
`cfg.auth` is `None`), then run:

Run: `cargo test -p paigasus-helikon-providers-litellm no_authorization_header`
Expected: **FAIL**. If it passes, the assertion is vacuous — fix it before
proceeding. Revert the temporary change and confirm it passes again.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-litellm --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-litellm/tests .gitattributes
git commit -m "test(providers): SMA-451 add litellm wire, streaming and cancellation tests"
```

---

### Task 11: Facade wiring + cross-crate parity test

**Files:**
- Modify: `crates/paigasus-helikon/Cargo.toml`
- Modify: `crates/paigasus-helikon/src/lib.rs`
- Create: `crates/paigasus-helikon/tests/openai_litellm_message_parity.rs`

**Interfaces:**
- Consumes: `paigasus_helikon_providers_litellm::LiteLlmModel`.
- Produces: `paigasus_helikon::litellm` (feature-gated re-export).

- [ ] **Step 1: Add the optional dependency**

In `crates/paigasus-helikon/Cargo.toml`, after the gemini line:

```toml
paigasus-helikon-providers-litellm  = { workspace = true, optional = true }
```

And in `[features]`, after `gemini`:

```toml
litellm            = ["dep:paigasus-helikon-providers-litellm"]
```

- [ ] **Step 2: Add the documented re-export**

In `crates/paigasus-helikon/src/lib.rs`, after the gemini re-export. **The
`///` comment is mandatory** — the docs job runs with `-D warnings` and an
undocumented `pub use` fails it:

```rust
/// LiteLLM proxy provider (OpenAI-compatible gateway). Enabled via the `litellm` feature.
#[cfg(feature = "litellm")]
pub use paigasus_helikon_providers_litellm as litellm;
```

- [ ] **Step 3: Write the cross-crate parity test**

This is the only construct that can actually fail when the duplicated
translation drifts — a snapshot inside either crate cannot.

`crates/paigasus-helikon/tests/openai_litellm_message_parity.rs`:

```rust
//! Cross-crate parity: the OpenAI and LiteLLM providers must translate the
//! same conversation into byte-identical `messages`.
//!
//! The LiteLLM crate duplicates `to_chat_messages` (SMA-451 design §13.1).
//! Snapshot tests inside either crate pin only that crate's own shape, so both
//! suites would stay green while the two implementations diverged. This test
//! is what makes divergence visible.
//!
//! If this fails, decide deliberately: either the divergence is intentional
//! (LiteLLM fronts backends OpenAI does not) — in which case move the case to
//! the documented-divergence list below — or it is a drift bug.
#![cfg(all(feature = "openai", feature = "litellm"))]

use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, MediaSource, Model, ModelRequest,
};

/// Build the request body each provider would send, via a local mock server,
/// and return the `messages` array.
async fn messages_via<M: Model>(model: &M, items: Vec<Item>) -> serde_json::Value {
    use futures_util::StreamExt;
    let mut req = ModelRequest::new();
    req.messages = items;
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    while s.next().await.is_some() {}
    serde_json::Value::Null // replaced below — see note
}

fn fixtures() -> Vec<(&'static str, Vec<Item>)> {
    vec![
        (
            "plain text",
            vec![Item::UserMessage {
                content: vec![ContentPart::Text { text: "hi".into() }],
            }],
        ),
        (
            "system + user",
            vec![
                Item::System {
                    content: vec![ContentPart::Text { text: "be terse".into() }],
                },
                Item::UserMessage {
                    content: vec![ContentPart::Text { text: "hi".into() }],
                },
            ],
        ),
        (
            "tool call + result",
            vec![
                Item::ToolCall {
                    call_id: "c1".into(),
                    name: "f".into(),
                    args: serde_json::json!({"a": 1}),
                },
                Item::ToolResult {
                    call_id: "c1".into(),
                    content: vec![ContentPart::Text { text: "ok".into() }],
                },
            ],
        ),
        (
            "multimodal user",
            vec![Item::UserMessage {
                content: vec![
                    ContentPart::Text { text: "what?".into() },
                    ContentPart::Image {
                        source: MediaSource::Base64 {
                            mime_type: "image/png".into(),
                            data: "AAAA".into(),
                        },
                    },
                ],
            }],
        ),
        (
            "assistant with nested tool_use",
            vec![Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "c2".into(),
                    name: "g".into(),
                    args: serde_json::json!({}),
                }],
                agent: None,
            }],
        ),
    ]
}

#[tokio::test]
async fn openai_and_litellm_translate_messages_identically() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let sse = |body: &'static str| {
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(body, "text/event-stream")
    };
    const DONE: &str =
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

    for (label, items) in fixtures() {
        // OpenAI provider
        let oa_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(DONE))
            .mount(&oa_server)
            .await;
        let oa = paigasus_helikon::openai::OpenAiModel::chat("gpt-4o")
            .api_key("sk-test")
            .base_url(format!("{}/v1", oa_server.uri()))
            .build()
            .unwrap();
        let _ = messages_via(&oa, items.clone()).await;
        let oa_body: serde_json::Value =
            serde_json::from_slice(&oa_server.received_requests().await.unwrap()[0].body).unwrap();

        // LiteLLM provider
        let ll_server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(DONE))
            .mount(&ll_server)
            .await;
        let ll = paigasus_helikon::litellm::LiteLlmModel::chat("prod-fast")
            .base_url(ll_server.uri())
            .api_key("sk-test")
            .build()
            .unwrap();
        let _ = messages_via(&ll, items.clone()).await;
        let ll_body: serde_json::Value =
            serde_json::from_slice(&ll_server.received_requests().await.unwrap()[0].body).unwrap();

        assert_eq!(
            oa_body["messages"], ll_body["messages"],
            "messages diverged for fixture `{label}`\n  openai:  {}\n  litellm: {}",
            oa_body["messages"], ll_body["messages"]
        );
    }
}
```

**Note on `messages_via`:** it drives the request through each provider so the
mock server records the real body; its return value is unused (the body is read
back from `received_requests()`). If the borrow checker or an unused-variable
lint objects, inline the two `invoke`+drain blocks and delete the helper — the
assertion is what matters.

- [ ] **Step 4: Add the dev-dependency the parity test needs**

`crates/paigasus-helikon/Cargo.toml` `[dev-dependencies]` already has `tokio`
and `serde_json`. Add if absent:

```toml
wiremock     = { workspace = true }
futures-util = { workspace = true }
```

- [ ] **Step 5: Run the gates**

```bash
cargo test -p paigasus-helikon --features openai,litellm --test openai_litellm_message_parity
cargo build -p paigasus-helikon --features litellm
```
Expected: parity test PASSES; facade builds with the new feature.

If a fixture legitimately diverges, do **not** delete the assertion — narrow it
to the fixtures that should match and document the exception in the test's
module docs.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon Cargo.lock
git commit -m "feat(facade): SMA-451 wire the litellm provider behind a feature"
```

---

### Task 12: Documentation

**Files:**
- Create: `crates/paigasus-helikon-providers-litellm/README.md`
- Modify: `crates/paigasus-helikon/README.md` (~line 22)
- Modify: `README.md` (repo root, ~lines 29 and 38)
- Modify: `docs/book/src/concepts/model-providers.md` (lines 6, 85, ~305-317, ~334-343)
- Modify: `docs/book/src/getting-started/workspace-layout.md` (~line 57)
- Modify: `docs/book/src/reference/crates.md` (roster ~line 27 **and** feature map ~line 57)

**Interfaces:** none.

Several of these pages carry a hardcoded provider count that goes stale
silently. Grep before editing: `grep -rn "Four adapters\|four provider" docs/book/src`.

- [ ] **Step 1: Write the crate README**

`crates/paigasus-helikon-providers-litellm/README.md`. It must cover the four
things a user will otherwise get wrong. Use `cargo add` (no hardcoded version):

````markdown
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
````

- [ ] **Step 2: Update the facade README feature map**

In `crates/paigasus-helikon/README.md`, add a row after the `gemini` row
(~line 22):

```markdown
| `litellm` | `litellm` | `paigasus-helikon-providers-litellm` |
```

**The facade README is `include_str!`'d into rustdoc**, so any ` ```rust ` fence
there becomes a compiled doctest. If you add an example, fence it ` ```ignore `.

- [ ] **Step 3: Update the root README**

In `README.md`, add LiteLLM to both provider enumerations (~lines 29 and 38).
Read the surrounding lines first and match their phrasing exactly.

- [ ] **Step 4: Update the mdBook**

`docs/book/src/concepts/model-providers.md`:
- Line 6 and line 85: "Four adapters ship today" → "Five adapters ship today".
- The "Switching providers is one line" block (~305-317) and "Enabling the
  providers" toml (~334-343): add LiteLLM.
- Add a LiteLLM section covering the same "when to use this vs
  `OpenAiModel::base_url()`" guidance as the crate README, plus the
  capability-declaration responsibility.

`docs/book/src/getting-started/workspace-layout.md` (~57): add
`- \`paigasus-helikon-providers-litellm\`` to the crate list.

`docs/book/src/reference/crates.md`: add **two** rows — the roster table (~27)
and the feature → module map (~57):

```markdown
| [`paigasus-helikon-providers-litellm`](https://docs.rs/paigasus-helikon-providers-litellm) | LiteLLM proxy adapter (`LiteLlmModel`; OpenAI-compatible gateway) | published | `0.1.0` |
```

```markdown
| `litellm` | `paigasus_helikon::litellm` | `paigasus-helikon-providers-litellm` |
```

- [ ] **Step 5: Verify the book builds**

Run: `mdbook build docs/book`
Expected: clean. `[output.linkcheck] warning-policy = "error"`, so a broken
link fails the build and the `book-build` required check.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-providers-litellm/README.md crates/paigasus-helikon/README.md README.md docs/book
git commit -m "docs(providers): SMA-451 document the litellm provider"
```

---

### Task 13: Full CI gate reproduction

**Files:** none created; fixes only.

**Interfaces:** none.

- [ ] **Step 1: Run every gate CI runs, in order**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
mdbook build docs/book
```

**Run `cargo test --workspace --all-features` exactly as written** — not
per-crate. Feature unification across the workspace has previously surfaced
failures that per-crate runs cannot (a second TLS backend being pulled in, for
instance). This is the command that matters.

- [ ] **Step 2: Fix what fails**

Likely issues, in order of probability:

- **`missing_docs`** on a `pub` item — every public item needs `///`, including
  the facade re-export.
- **doc-coverage below 80%** for the new crate — add `///` to any `pub(crate)`
  item the aggregator counts.
- **clippy `too_many_arguments`** on `classify` (5 params is under the default
  7 threshold, so this should not fire; if it does, group into a struct rather
  than `#[allow]`).

- [ ] **Step 3: Verify the crate is publishable**

```bash
cargo publish -p paigasus-helikon-providers-litellm --dry-run
```
Expected: success. This catches a missing `description`/`license` and any
`path`-only dependency that would not resolve from the registry.

- [ ] **Step 4: Commit any fixes**

```bash
cargo fmt --all
git add -u
git commit -m "chore(providers): SMA-451 satisfy lint and doc gates"
```

---

## Self-Review

**Spec coverage.** Every design section maps to a task: §5 → Tasks 3, 9;
§6 → Task 2; §7 → Tasks 5, 6; §8 → Task 5; §9 → Task 7; §10 → Tasks 8, 9;
§11 → Task 1; §12 → Task 3 (`default_http_client`); §13 → Tasks 6, 7, 10;
§13.1 → Tasks 4, 11; §15 → Tasks 1, 11, 12; §16 → Task 3 (redacting `Debug`).

**Deliberately not implemented**, per the spec's out-of-scope list: the
Responses backend, `/v1/model/info` discovery, typed `x-litellm-*` surfacing
(D5 — logging only, Task 9), object-form `fallbacks`, and batch model lists.

**Known follow-ups referenced but not built here:** SMA-522 (the same ordering
bug in the OpenAI provider) and SMA-523 (the `litellm-it` CI job).

**Naming consistency.** `Config`, `Extras`, `RESERVED_BODY_KEYS`,
`normalise_endpoint`, `build_request`, `ChatTranslator::{consume, finish}`,
`classify`, `parse_retry_after_ms`, `apply_override`, `conservative_defaults`
are each defined once and used with the same signature throughout.

**Two known plan risks, flagged for the implementer:**

1. **Task 2's `normalise_endpoint`** has a fiddly segment-manipulation block.
   If the `path_segments_mut` gymnastics fight the borrow checker, the
   fallback stated in Step 5 (collect segments into a `Vec<String>`, transform,
   `clear()`, re-push) is the intended shape. Do not fall back to string
   concatenation — the tests will catch it, but the query-string case is the
   reason.
2. **Task 11's `messages_via` helper** is written for readability and returns a
   value it does not use. Inlining it is explicitly sanctioned; the assertion
   is the point.

---

## Execution Handoff

Plan complete and saved to
`docs/superpowers/plans/2026-08-16-sma-451-litellm-provider.md`. Two execution
options:

**1. Subagent-Driven (recommended)** — a fresh subagent per task, review
between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using executing-plans,
batch execution with checkpoints.

Which approach?
