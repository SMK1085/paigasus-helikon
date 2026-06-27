# Gemini Provider (`paigasus-helikon-providers-gemini`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a self-contained `paigasus-helikon-providers-gemini` crate implementing `paigasus_helikon_core::Model` for Google Gemini (Developer API + Vertex AI), wired into the facade behind a `gemini` feature.

**Architecture:** Hand-rolled REST over `reqwest` + `eventsource-stream` SSE, mirroring the Anthropic provider's layout. Pure, unit-testable seams (`classify`, `sanitize_schema`, `build_request`/`to_wire_json`, `StreamTranslator`) plus a thin `invoke` that drives the HTTP+SSE pipeline with a `tokio::select!` cancellation guard. Native structured output via `responseMimeType`+`responseSchema` (no hidden-tool synthesis). Dual transport differs only in URL + auth header.

**Tech Stack:** Rust (edition 2021), `reqwest` (json/stream/rustls), `eventsource-stream`, `async-trait`, `async-stream`, `serde`/`serde_json`, `thiserror`, `tokio`/`tokio-util`, `tracing`; dev: `wiremock`, `insta`; optional `gcp_auth` (feature `vertex-adc`).

**Spec:** `docs/superpowers/specs/2026-06-25-sma-449-gemini-provider-design.md`

## Global Constraints

- `edition = 2021`, `rust-version = 1.94`, `license = "Apache-2.0 OR MIT"` — all `*.workspace = true` (copy from `crates/paigasus-helikon-providers-anthropic/Cargo.toml`).
- `[lints] workspace = true` in the crate Cargo.toml; code must pass `cargo clippy --all-targets -- -D warnings`.
- Run `cargo fmt --all` and `cargo clippy` locally before every commit (pre-commit hook is a no-op; pre-push enforces).
- Commits are signed via a 1Password SSH key (unlock the vault if a commit fails with "failed to fill whole buffer").
- Conventional-commit scopes: use `providers` for code (`feat(providers): SMA-449 …`). Never `git add -A` (`.env`/`.claude` are untracked-but-not-ignored) — stage explicit paths.
- Provider string is `"gemini"`. Reserved nothing — Gemini uses native structured output (no synthesized tool name).
- Never log raw request/response payloads (may contain user/tool content) — log structured metadata only (`tracing::warn!(target: "paigasus::gemini::sse", …)`).
- `CancellationToken`, `Model`, `ModelEvent`, etc. are imported from `paigasus_helikon_core` (it re-exports `CancellationToken`).

---

## File structure

```
crates/paigasus-helikon-providers-gemini/
  Cargo.toml
  README.md
  .gitattributes                 # *.sse / fixtures -> text eol=lf
  src/
    lib.rs                       # crate docs + module decls + re-exports
    error.rs                     # classify(), parse_retry_after_ms()
    capabilities.rs              # KNOWN_MODELS, conservative_defaults(), lookup()
    auth.rs                      # Auth (pub(crate)), TokenProvider; AdcTokenProvider (feature vertex-adc)
    builder.rs                   # GeminiModelBuilder, BuildError, Config, Transport
    transport.rs                 # generate_content_url(), build_headers()
    sse.rs                       # GeminiChunk types (serde)
    stream.rs                    # StreamTranslator
    model.rs                     # GeminiModel + impl Model
    translate/
      mod.rs                     # build_request(), PreparedRequest, to_wire_json snapshot tests
      request.rs                 # items_to_contents()
      tools.rs                   # function_declarations(), function_calling_config()
      response_format.rs         # generation_config_response_format(), validate_conflict()
      schema.rs                  # sanitize_schema()
      snapshots/                 # insta .snap files
  tests/
    gemini_wire.rs               # wiremock request-shape + headers + url (Developer & Vertex)
    gemini_streaming.rs          # wiremock SSE -> ModelEvent sequences
    live.rs                      # #[ignore] live smoke tests
```

Wiring touch-points (existing files):
- Root `Cargo.toml` — add `[workspace.dependencies]` entries.
- `crates/paigasus-helikon/Cargo.toml` — optional dep + `gemini` feature.
- `crates/paigasus-helikon/src/lib.rs` — `#[cfg(feature = "gemini")] pub use … as gemini;`.

---

### Task 1: Scaffold crate, wire workspace + facade, error classifier

**Files:**
- Create: `crates/paigasus-helikon-providers-gemini/Cargo.toml`
- Create: `crates/paigasus-helikon-providers-gemini/src/lib.rs`
- Create: `crates/paigasus-helikon-providers-gemini/src/error.rs`
- Modify: `Cargo.toml` (root `[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon/Cargo.toml`
- Modify: `crates/paigasus-helikon/src/lib.rs`

**Interfaces:**
- Produces: `error::classify(status: u16, status_field: Option<&str>, message: &str, retry_after_ms: Option<u64>) -> ModelError`; `error::parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64>`.

- [ ] **Step 1: Create the crate Cargo.toml**

`crates/paigasus-helikon-providers-gemini/Cargo.toml`:
```toml
[package]
name        = "paigasus-helikon-providers-gemini"
description = "Google Gemini provider for the Paigasus Helikon AI SDK."
version                = "0.1.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[features]
vertex-adc = ["dep:gcp_auth"]

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
gcp_auth              = { workspace = true, optional = true }

[dev-dependencies]
wiremock = { workspace = true }
insta    = { workspace = true, features = ["json", "yaml"] }
tokio    = { workspace = true, features = ["macros", "rt-multi-thread", "time"] }
reqwest  = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 2: Add root workspace.dependencies entries**

In root `Cargo.toml` `[workspace.dependencies]`, add (alphabetical-ish, near the other `paigasus-helikon-providers-*` entries and a `gcp_auth` line near other third-party deps):
```toml
paigasus-helikon-providers-gemini = { path = "crates/paigasus-helikon-providers-gemini", version = "0.1.0" }
gcp_auth = "0.12"
```

- [ ] **Step 3: Wire the facade**

In `crates/paigasus-helikon/Cargo.toml`, add the optional dep (near `paigasus-helikon-providers-bedrock`):
```toml
paigasus-helikon-providers-gemini = { workspace = true, optional = true }
```
and the feature (near `bedrock`):
```toml
gemini = ["dep:paigasus-helikon-providers-gemini"]
```
In `crates/paigasus-helikon/src/lib.rs`, add (near the `bedrock` re-export):
```rust
/// Google Gemini provider (Developer API + Vertex). Enabled via the `gemini` feature.
#[cfg(feature = "gemini")]
pub use paigasus_helikon_providers_gemini as gemini;
```

- [ ] **Step 4: Create minimal lib.rs with the error module**

`crates/paigasus-helikon-providers-gemini/src/lib.rs`:
```rust
//! Google Gemini provider for the Paigasus Helikon SDK.
//!
//! The public surface is [`GeminiModel`] (a [`paigasus_helikon_core::Model`])
//! and its [`GeminiModelBuilder`]. Supports both the Gemini **Developer API**
//! (API key) and **Vertex AI** (OAuth bearer / `TokenProvider`).
//!
//! ```ignore
//! use paigasus_helikon_providers_gemini::GeminiModel;
//! # async fn f() -> Result<(), Box<dyn std::error::Error>> {
//! let _model = GeminiModel::from_env("gemini-2.5-flash")?;
//! # Ok(()) }
//! ```

mod error;
```

- [ ] **Step 5: Write the failing test for `classify`**

`crates/paigasus-helikon-providers-gemini/src/error.rs`:
```rust
//! Map Google API HTTP errors onto core `ModelError` variants.

use paigasus_helikon_core::ModelError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_429_carries_retry_after() {
        let e = classify(429, Some("RESOURCE_EXHAUSTED"), "quota", Some(7000));
        assert!(matches!(e, ModelError::RateLimited { retry_after_ms: Some(7000) }));
    }

    #[test]
    fn unavailable_503_500_504() {
        for s in [503u16, 500, 504] {
            assert!(matches!(classify(s, None, "x", None), ModelError::Unavailable), "status {s}");
        }
    }

    #[test]
    fn forbidden_and_unauthenticated_are_refused() {
        assert!(matches!(classify(403, Some("PERMISSION_DENIED"), "no", None), ModelError::Refused { .. }));
        assert!(matches!(classify(401, Some("UNAUTHENTICATED"), "no", None), ModelError::Refused { .. }));
    }

    #[test]
    fn context_overflow_400() {
        let e = classify(400, Some("INVALID_ARGUMENT"), "input token count exceeds the maximum", None);
        assert!(matches!(e, ModelError::ContextLengthExceeded));
    }

    #[test]
    fn other_400_is_other() {
        assert!(matches!(classify(400, Some("INVALID_ARGUMENT"), "bad field", None), ModelError::Other(_)));
    }

    #[test]
    fn retry_after_header_seconds_to_ms() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&h), Some(3000));
        assert_eq!(parse_retry_after_ms(&reqwest::header::HeaderMap::new()), None);
    }
}
```

- [ ] **Step 6: Run the test, verify it fails**

Run: `cargo test -p paigasus-helikon-providers-gemini error::`
Expected: FAIL — `cannot find function classify`.

- [ ] **Step 7: Implement `classify` + `parse_retry_after_ms`**

Prepend to `src/error.rs` (above the `tests` module):
```rust
/// Classify a Google API error response into a core [`ModelError`].
///
/// `status` is the HTTP status; `status_field` is the JSON `error.status`
/// string (e.g. `RESOURCE_EXHAUSTED`); `message` is `error.message`.
pub(crate) fn classify(
    status: u16,
    status_field: Option<&str>,
    message: &str,
    retry_after_ms: Option<u64>,
) -> ModelError {
    match status {
        429 => ModelError::RateLimited { retry_after_ms },
        500 | 503 | 504 => ModelError::Unavailable,
        401 | 403 => ModelError::Refused { reason: message.to_owned() },
        400 => {
            let lc = message.to_ascii_lowercase();
            if lc.contains("token count") || lc.contains("maximum") && lc.contains("context")
                || lc.contains("exceeds") && lc.contains("token")
            {
                ModelError::ContextLengthExceeded
            } else {
                ModelError::Other(anyhow::anyhow!(
                    "gemini {}: {message}",
                    status_field.unwrap_or("INVALID_ARGUMENT")
                ))
            }
        }
        _ => ModelError::Other(anyhow::anyhow!(
            "gemini http {status} {}: {message}",
            status_field.unwrap_or("")
        )),
    }
}

/// Parse an integer-seconds `Retry-After` header into milliseconds.
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

- [ ] **Step 8: Run tests + build the facade feature**

Run: `cargo test -p paigasus-helikon-providers-gemini`
Expected: PASS (6 tests).
Run: `cargo build -p paigasus-helikon --features gemini`
Expected: builds (facade re-exports the crate).
Run: `cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add crates/paigasus-helikon-providers-gemini/Cargo.toml \
        crates/paigasus-helikon-providers-gemini/src/lib.rs \
        crates/paigasus-helikon-providers-gemini/src/error.rs \
        Cargo.toml crates/paigasus-helikon/Cargo.toml crates/paigasus-helikon/src/lib.rs
git commit -m "feat(providers): SMA-449 scaffold gemini crate + error classifier

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Schema sanitizer (`translate/schema.rs`)

**Files:**
- Create: `crates/paigasus-helikon-providers-gemini/src/translate/mod.rs` (module decl only for now)
- Create: `crates/paigasus-helikon-providers-gemini/src/translate/schema.rs`
- Modify: `crates/paigasus-helikon-providers-gemini/src/lib.rs` (add `mod translate;`)

**Interfaces:**
- Produces: `translate::schema::sanitize_schema(schema: &serde_json::Value) -> serde_json::Value` — rewrites JSON Schema into Gemini's OpenAPI-3.0 subset (inline `$ref`, strip unsupported keywords, preserve combinator meaning).

- [ ] **Step 1: Add module decls**

In `src/lib.rs` add `mod translate;`. Create `src/translate/mod.rs` with:
```rust
//! Translation between Paigasus carrier types and Gemini wire format.

pub(crate) mod schema;
```

- [ ] **Step 2: Write failing tests**

`src/translate/schema.rs`:
```rust
//! Rewrite JSON Schema into the OpenAPI-3.0 subset Gemini accepts.

use serde_json::{json, Map, Value};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_unsupported_keywords() {
        let input = json!({
            "$schema": "http://json-schema.org/draft/2020-12/schema",
            "$id": "x", "additionalProperties": false,
            "type": "object",
            "properties": { "a": { "type": "string", "examples": ["x"] } }
        });
        let out = sanitize_schema(&input);
        assert!(out.get("$schema").is_none());
        assert!(out.get("$id").is_none());
        assert!(out.get("additionalProperties").is_none());
        assert!(out["properties"]["a"].get("examples").is_none());
        assert_eq!(out["properties"]["a"]["type"], "string");
    }

    #[test]
    fn inlines_ref_from_defs() {
        let input = json!({
            "type": "object",
            "properties": { "child": { "$ref": "#/$defs/Child" } },
            "$defs": { "Child": { "type": "object", "properties": { "n": { "type": "integer" } } } }
        });
        let out = sanitize_schema(&input);
        assert!(out.get("$defs").is_none());
        assert_eq!(out["properties"]["child"]["type"], "object");
        assert_eq!(out["properties"]["child"]["properties"]["n"]["type"], "integer");
    }

    #[test]
    fn nullable_collapse_from_type_array() {
        let input = json!({ "type": ["string", "null"] });
        let out = sanitize_schema(&input);
        assert_eq!(out["type"], "string");
        assert_eq!(out["nullable"], true);
    }

    #[test]
    fn oneof_becomes_anyof_and_const_becomes_enum() {
        let input = json!({
            "oneOf": [ { "type": "string" }, { "const": 5 } ]
        });
        let out = sanitize_schema(&input);
        assert!(out.get("oneOf").is_none());
        let any = out["anyOf"].as_array().unwrap();
        assert_eq!(any[0]["type"], "string");
        assert_eq!(any[1]["enum"], json!([5]));
    }

    #[test]
    fn cycle_is_guarded() {
        // Self-referential $ref must not infinitely recurse.
        let input = json!({
            "type": "object",
            "properties": { "self": { "$ref": "#/$defs/Node" } },
            "$defs": { "Node": { "type": "object", "properties": { "next": { "$ref": "#/$defs/Node" } } } }
        });
        let out = sanitize_schema(&input);
        // Terminates; deep node degrades to an empty object at the depth/cycle guard.
        assert_eq!(out["properties"]["self"]["type"], "object");
    }
}
```

- [ ] **Step 3: Run, verify fail**

Run: `cargo test -p paigasus-helikon-providers-gemini schema::`
Expected: FAIL — `cannot find function sanitize_schema`.

- [ ] **Step 4: Implement**

Prepend to `src/translate/schema.rs`:
```rust
const MAX_DEPTH: usize = 64;

/// Keywords Gemini's schema validator rejects outright.
const STRIP: &[&str] = &[
    "$schema", "$id", "$anchor", "$comment", "additionalProperties",
    "unevaluatedProperties", "patternProperties", "examples", "default",
];

/// Rewrite `schema` into Gemini's OpenAPI-3.0 subset.
pub(crate) fn sanitize_schema(schema: &Value) -> Value {
    let defs = collect_defs(schema);
    rewrite(schema, &defs, 0, &mut Vec::new())
}

fn collect_defs(root: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for key in ["$defs", "definitions"] {
        if let Some(Value::Object(m)) = root.get(key) {
            for (k, v) in m {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

fn rewrite(node: &Value, defs: &Map<String, Value>, depth: usize, seen: &mut Vec<String>) -> Value {
    if depth > MAX_DEPTH {
        return json!({ "type": "object" });
    }
    let Value::Object(obj) = node else { return node.clone() };

    // 1. $ref inlining with cycle guard.
    if let Some(Value::String(r)) = obj.get("$ref") {
        let name = r.rsplit('/').next().unwrap_or_default().to_owned();
        if seen.contains(&name) {
            return json!({ "type": "object" });
        }
        if let Some(target) = defs.get(&name) {
            seen.push(name);
            let out = rewrite(target, defs, depth + 1, seen);
            seen.pop();
            return out;
        }
        return json!({ "type": "object" });
    }

    let mut out = Map::new();
    for (k, v) in obj {
        if STRIP.contains(&k.as_str()) || k == "$defs" || k == "definitions" {
            continue;
        }
        match k.as_str() {
            // 3. const -> enum:[v]
            "const" => {
                out.insert("enum".into(), json!([v.clone()]));
            }
            // type: [T, "null"] -> T + nullable:true
            "type" if v.is_array() => {
                let arr = v.as_array().unwrap();
                let non_null: Vec<&Value> = arr.iter().filter(|x| x.as_str() != Some("null")).collect();
                if arr.iter().any(|x| x.as_str() == Some("null")) {
                    out.insert("nullable".into(), Value::Bool(true));
                }
                if let [single] = non_null.as_slice() {
                    out.insert("type".into(), (*single).clone());
                } else if let Some(first) = non_null.first() {
                    out.insert("type".into(), (*first).clone());
                }
            }
            // oneOf -> anyOf (recursing members)
            "oneOf" | "anyOf" => {
                let members: Vec<Value> = v
                    .as_array()
                    .map(|a| a.iter().map(|m| rewrite(m, defs, depth + 1, seen)).collect())
                    .unwrap_or_default();
                // [T, {type:null}] -> nullable
                let nulls = members.iter().any(|m| m.get("type").and_then(|t| t.as_str()) == Some("null"));
                let non_null: Vec<Value> =
                    members.into_iter().filter(|m| m.get("type").and_then(|t| t.as_str()) != Some("null")).collect();
                if nulls {
                    out.insert("nullable".into(), Value::Bool(true));
                }
                if non_null.len() == 1 {
                    if let Value::Object(only) = &non_null[0] {
                        for (kk, vv) in only {
                            out.entry(kk.clone()).or_insert_with(|| vv.clone());
                        }
                    }
                } else {
                    out.insert("anyOf".into(), Value::Array(non_null));
                }
            }
            "properties" => {
                let mut p = Map::new();
                if let Value::Object(props) = v {
                    for (pk, pv) in props {
                        p.insert(pk.clone(), rewrite(pv, defs, depth + 1, seen));
                    }
                }
                out.insert("properties".into(), Value::Object(p));
            }
            "items" => {
                out.insert("items".into(), rewrite(v, defs, depth + 1, seen));
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}
```

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p paigasus-helikon-providers-gemini schema::`
Expected: PASS (5 tests).

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/src/lib.rs \
        crates/paigasus-helikon-providers-gemini/src/translate/mod.rs \
        crates/paigasus-helikon-providers-gemini/src/translate/schema.rs
git commit -m "feat(providers): SMA-449 gemini schema sanitizer

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Capabilities (`capabilities.rs`)

**Files:**
- Create: `crates/paigasus-helikon-providers-gemini/src/capabilities.rs`
- Modify: `src/lib.rs` (add `mod capabilities;`)

**Interfaces:**
- Produces: `capabilities::ModelEntry { caps: ModelCapabilities }`; `capabilities::lookup(model_id: &str) -> ModelEntry`; `capabilities::conservative_defaults() -> ModelEntry`.

- [ ] **Step 1: Failing test**

`src/capabilities.rs`:
```rust
//! KNOWN_MODELS capability lookup for Gemini models.

use paigasus_helikon_core::ModelCapabilities;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_25_flash_has_tools_and_structured_output() {
        let e = lookup("gemini-2.5-flash");
        assert!(e.caps.streaming && e.caps.tools && e.caps.structured_output && e.caps.vision);
        assert!(e.caps.parallel_tool_calls);
        // Reasoning streaming deferred (D3): flag stays false even for 2.5.
        assert!(!e.caps.reasoning);
    }

    #[test]
    fn unknown_model_falls_back_to_conservative() {
        let e = lookup("gemini-9-ultra");
        assert!(e.caps.streaming && e.caps.tools);
        assert!(!e.caps.structured_output);
    }
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p paigasus-helikon-providers-gemini capabilities::`
Expected: FAIL — `cannot find function lookup`.

- [ ] **Step 3: Implement** (add `mod capabilities;` to lib.rs, then prepend)

```rust
/// Capability snapshot for a model id.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModelEntry {
    pub(crate) caps: ModelCapabilities,
}

/// Conservative fallback for ids absent from [`KNOWN_MODELS`].
pub(crate) const fn conservative_defaults() -> ModelEntry {
    ModelEntry {
        caps: ModelCapabilities::empty().with_streaming().with_tools(),
    }
}

const fn full() -> ModelEntry {
    ModelEntry {
        caps: ModelCapabilities::empty()
            .with_streaming()
            .with_tools()
            .with_parallel_tool_calls()
            .with_structured_output()
            .with_vision(),
    }
}

/// Capability snapshot keyed by exact model id. Cross-check against Google's
/// published model docs at implementation time; divergences are bugs.
pub(crate) const KNOWN_MODELS: &[(&str, ModelEntry)] = &[
    ("gemini-2.5-pro", full()),
    ("gemini-2.5-flash", full()),
    ("gemini-2.0-flash", full()),
    ("gemini-2.0-flash-lite", full()),
];

/// Look up capabilities for `model_id`, falling back to conservative defaults.
pub(crate) fn lookup(model_id: &str) -> ModelEntry {
    KNOWN_MODELS
        .iter()
        .find(|(id, _)| *id == model_id)
        .map(|(_, e)| *e)
        .unwrap_or_else(conservative_defaults)
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p paigasus-helikon-providers-gemini capabilities::`
Expected: PASS (2 tests).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/src/lib.rs crates/paigasus-helikon-providers-gemini/src/capabilities.rs
git commit -m "feat(providers): SMA-449 gemini capability table

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Auth + builder (`auth.rs`, `builder.rs`)

**Files:**
- Create: `crates/paigasus-helikon-providers-gemini/src/auth.rs`
- Create: `crates/paigasus-helikon-providers-gemini/src/builder.rs`
- Modify: `src/lib.rs` (`mod auth; mod builder;` + re-exports)

**Interfaces:**
- Consumes: `capabilities::lookup`.
- Produces:
  - `pub trait TokenProvider: Send + Sync + std::fmt::Debug { async fn token(&self) -> Result<String, ModelError>; }`
  - `pub(crate) enum Auth { ApiKey(String), Bearer(String), Token(std::sync::Arc<dyn TokenProvider>) }`
  - `pub(crate) enum Transport { Developer, Vertex { project: String, location: String } }`
  - `pub(crate) struct Config { http: reqwest::Client, base_url: Option<String>, model_id: String, transport: Transport, auth: Auth, capabilities: ModelCapabilities }`
  - `pub struct GeminiModelBuilder` + `pub enum BuildError`.
  - `GeminiModel::developer(id)`, `GeminiModel::vertex(id, project, location)`, `GeminiModel::from_env(id)` (defined here, returning the builder/model).

- [ ] **Step 1: Write `auth.rs`** (no dedicated test; exercised via builder)

```rust
//! Gemini authentication: API key (Developer) or bearer/token-provider (Vertex).

use std::sync::Arc;

use async_trait::async_trait;
use paigasus_helikon_core::ModelError;

/// Supplies a fresh OAuth bearer access token for Vertex requests.
#[async_trait]
pub trait TokenProvider: Send + Sync + std::fmt::Debug {
    /// Return a bearer access token (without the `Bearer ` prefix).
    async fn token(&self) -> Result<String, ModelError>;
}

/// Resolved credential. Representation is crate-private; callers configure it
/// via builder methods.
#[derive(Clone)]
pub(crate) enum Auth {
    ApiKey(String),
    Bearer(String),
    Token(Arc<dyn TokenProvider>),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Auth::ApiKey(_) => f.write_str("Auth::ApiKey(***)"),
            Auth::Bearer(_) => f.write_str("Auth::Bearer(***)"),
            Auth::Token(_) => f.write_str("Auth::Token(<provider>)"),
        }
    }
}
```

- [ ] **Step 2: Write failing builder validation tests**

`src/builder.rs` (tests first):
```rust
//! `GeminiModelBuilder` — fluent constructor for [`crate::GeminiModel`].

#[cfg(test)]
mod tests {
    use crate::GeminiModel;

    #[test]
    fn developer_requires_api_key() {
        let err = GeminiModel::developer("gemini-2.5-flash").build().unwrap_err();
        assert!(matches!(err, crate::BuildError::MissingApiKey));
    }

    #[test]
    fn developer_rejects_empty_api_key() {
        let err = GeminiModel::developer("gemini-2.5-flash").api_key("   ").build().unwrap_err();
        assert!(matches!(err, crate::BuildError::EmptyApiKey));
    }

    #[test]
    fn developer_with_key_builds() {
        let m = GeminiModel::developer("gemini-2.5-flash").api_key("k").build().unwrap();
        assert_eq!(m.model(), "gemini-2.5-flash");
        assert_eq!(m.provider(), "gemini");
    }

    #[test]
    fn vertex_requires_auth() {
        let err = GeminiModel::vertex("gemini-2.5-pro", "proj", "us-central1").build().unwrap_err();
        assert!(matches!(err, crate::BuildError::MissingVertexAuth));
    }

    #[test]
    fn vertex_with_bearer_builds() {
        let m = GeminiModel::vertex("gemini-2.5-pro", "proj", "us-central1")
            .bearer_token("ya29.token")
            .build()
            .unwrap();
        assert_eq!(m.model(), "gemini-2.5-pro");
    }

    #[test]
    fn api_key_in_vertex_mode_is_mismatch() {
        let err = GeminiModel::vertex("gemini-2.5-pro", "p", "l").api_key("k").build().unwrap_err();
        assert!(matches!(err, crate::BuildError::AuthTransportMismatch));
    }
}
```

> Note: `GeminiModel`, `m.model()`, `m.provider()` come from Task 5+11; this task's `build()` will compile against a `GeminiModel` defined in Task 11. To keep Task 4 independently testable, define a **temporary** `model.rs` stub now (Step 4) and flesh it out in Task 11.

- [ ] **Step 3: Implement `builder.rs`** (add `mod auth; mod builder;` and re-exports to lib.rs)

In `src/lib.rs`:
```rust
mod auth;
mod builder;
mod capabilities;

pub use auth::TokenProvider;
pub use builder::{BuildError, GeminiModelBuilder};
pub use model::GeminiModel;
mod model;
```
`src/builder.rs` implementation (prepend above tests):
```rust
use std::sync::Arc;

use paigasus_helikon_core::ModelCapabilities;

use crate::auth::{Auth, TokenProvider};

/// Transport selected at construction.
#[derive(Debug, Clone)]
pub(crate) enum Transport {
    Developer,
    Vertex { project: String, location: String },
}

/// Errors raised while building a [`crate::GeminiModel`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    #[error("GEMINI_API_KEY/GOOGLE_API_KEY not set and no api_key supplied")]
    MissingApiKey,
    #[error("api key is empty")]
    EmptyApiKey,
    #[error("vertex transport requires a bearer token or TokenProvider")]
    MissingVertexAuth,
    #[error("vertex transport requires a non-empty project")]
    MissingVertexProject,
    #[error("vertex transport requires a non-empty location")]
    MissingVertexLocation,
    #[error("auth credential does not match the selected transport")]
    AuthTransportMismatch,
    #[error("base_url is not a valid URL: {0}")]
    InvalidBaseUrl(String),
    #[error("model id is empty")]
    EmptyModelId,
}

/// Builder-baked, immutable per-request config.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: Option<String>,
    pub(crate) model_id: String,
    pub(crate) transport: Transport,
    pub(crate) auth: Auth,
    pub(crate) capabilities: ModelCapabilities,
}

/// Fluent builder for [`crate::GeminiModel`].
#[derive(Debug)]
pub struct GeminiModelBuilder {
    model_id: String,
    transport: Transport,
    api_key: Option<String>,
    bearer: Option<String>,
    token: Option<Arc<dyn TokenProvider>>,
    base_url: Option<String>,
    http: Option<reqwest::Client>,
    caps_override: Option<ModelCapabilities>,
}

impl GeminiModelBuilder {
    pub(crate) fn new(model_id: impl Into<String>, transport: Transport) -> Self {
        Self {
            model_id: model_id.into(),
            transport,
            api_key: None,
            bearer: None,
            token: None,
            base_url: None,
            http: None,
            caps_override: None,
        }
    }

    /// Set the Developer-API key.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }
    /// Set a static Vertex bearer token.
    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(token.into());
        self
    }
    /// Set a Vertex token provider (fresh token per request).
    pub fn token_provider(mut self, p: impl TokenProvider + 'static) -> Self {
        self.token = Some(Arc::new(p));
        self
    }
    /// Override the API base URL (enables proxies / regional hosts).
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }
    /// Use a caller-provided `reqwest::Client`.
    pub fn http_client(mut self, c: reqwest::Client) -> Self {
        self.http = Some(c);
        self
    }
    /// Override the capability flags.
    pub fn with_capabilities(mut self, c: ModelCapabilities) -> Self {
        self.caps_override = Some(c);
        self
    }

    pub(crate) fn build_config(self) -> Result<Config, BuildError> {
        if self.model_id.trim().is_empty() {
            return Err(BuildError::EmptyModelId);
        }
        if let Some(u) = &self.base_url {
            if reqwest::Url::parse(u).is_err() {
                return Err(BuildError::InvalidBaseUrl(u.clone()));
            }
        }
        let auth = match &self.transport {
            Transport::Developer => {
                if self.bearer.is_some() || self.token.is_some() {
                    return Err(BuildError::AuthTransportMismatch);
                }
                let key = self.api_key.ok_or(BuildError::MissingApiKey)?;
                if key.trim().is_empty() {
                    return Err(BuildError::EmptyApiKey);
                }
                Auth::ApiKey(key)
            }
            Transport::Vertex { project, location } => {
                if self.api_key.is_some() {
                    return Err(BuildError::AuthTransportMismatch);
                }
                if project.trim().is_empty() {
                    return Err(BuildError::MissingVertexProject);
                }
                if location.trim().is_empty() {
                    return Err(BuildError::MissingVertexLocation);
                }
                if let Some(t) = self.token {
                    Auth::Token(t)
                } else if let Some(b) = self.bearer {
                    if b.trim().is_empty() {
                        return Err(BuildError::MissingVertexAuth);
                    }
                    Auth::Bearer(b)
                } else {
                    return Err(BuildError::MissingVertexAuth);
                }
            }
        };
        let capabilities = self
            .caps_override
            .unwrap_or_else(|| crate::capabilities::lookup(&self.model_id).caps);
        Ok(Config {
            http: self.http.unwrap_or_default(),
            base_url: self.base_url,
            model_id: self.model_id,
            transport: self.transport,
            auth,
            capabilities,
        })
    }
}
```

- [ ] **Step 4: Add the `GeminiModel` constructors + temporary stub `model.rs`**

`src/model.rs` (temporary; Task 11 adds `impl Model`):
```rust
//! `GeminiModel` — public [`paigasus_helikon_core::Model`] implementation.

use std::sync::Arc;

use crate::builder::{BuildError, Config, GeminiModelBuilder, Transport};

/// Google Gemini provider (Developer API + Vertex).
#[derive(Debug, Clone)]
pub struct GeminiModel(pub(crate) Arc<Config>);

impl GeminiModel {
    /// Developer-API builder (API key).
    pub fn developer(model_id: impl Into<String>) -> GeminiModelBuilder {
        GeminiModelBuilder::new(model_id, Transport::Developer)
    }
    /// Vertex-AI builder (project + location + bearer/token-provider).
    pub fn vertex(
        model_id: impl Into<String>,
        project: impl Into<String>,
        location: impl Into<String>,
    ) -> GeminiModelBuilder {
        GeminiModelBuilder::new(
            model_id,
            Transport::Vertex { project: project.into(), location: location.into() },
        )
    }
    /// Developer API from `GEMINI_API_KEY` (fallback `GOOGLE_API_KEY`).
    pub fn from_env(model_id: impl Into<String>) -> Result<Self, BuildError> {
        let key = std::env::var("GEMINI_API_KEY")
            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
            .map_err(|_| BuildError::MissingApiKey)?;
        Self::developer(model_id).api_key(key).build()
    }

    pub(crate) fn from_config(cfg: Config) -> Self {
        Self(Arc::new(cfg))
    }
    /// Provider id.
    pub fn provider(&self) -> &str {
        "gemini"
    }
    /// Model id.
    pub fn model(&self) -> &str {
        &self.0.model_id
    }
}

impl GeminiModelBuilder {
    /// Validate inputs and materialize the [`GeminiModel`].
    pub fn build(self) -> Result<GeminiModel, BuildError> {
        Ok(GeminiModel::from_config(self.build_config()?))
    }
}
```
(Note: in Task 11 the `provider`/`model` inherent methods become the trait impl; keep them inherent for now so Task 4 compiles independently. Task 11 replaces them with the `impl Model` trait methods.)

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p paigasus-helikon-providers-gemini builder::`
Expected: PASS (6 tests).

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/src/lib.rs \
        crates/paigasus-helikon-providers-gemini/src/auth.rs \
        crates/paigasus-helikon-providers-gemini/src/builder.rs \
        crates/paigasus-helikon-providers-gemini/src/model.rs
git commit -m "feat(providers): SMA-449 gemini builder + auth model

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Request translation (`translate/request.rs`)

**Files:**
- Create: `crates/paigasus-helikon-providers-gemini/src/translate/request.rs`
- Modify: `src/translate/mod.rs` (`pub(crate) mod request;`)

**Interfaces:**
- Produces:
  - `request::TranslatedContents { system: Option<Value>, contents: Vec<Value> }`
  - `request::items_to_contents(items: &[Item]) -> Result<TranslatedContents, ModelError>` — maps roles, tool call/result (with `id` + `call_id→name` recovery), inline images; errors on empty/system-only.

- [ ] **Step 1: Failing tests**

`src/translate/request.rs`:
```rust
//! Translate core `Item`s into Gemini `contents` + `systemInstruction`.

use paigasus_helikon_core::{ContentPart, Item, MediaSource, ModelError};
use serde_json::{json, Map, Value};

#[cfg(test)]
mod tests {
    use super::*;

    fn user(s: &str) -> Item {
        Item::UserMessage { content: vec![ContentPart::Text { text: s.into() }] }
    }

    #[test]
    fn system_goes_to_system_instruction_not_contents() {
        let items = vec![
            Item::System { content: vec![ContentPart::Text { text: "be terse".into() }] },
            user("hi"),
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(t.system.unwrap()["parts"][0]["text"], "be terse");
        assert_eq!(t.contents.len(), 1);
        assert_eq!(t.contents[0]["role"], "user");
        assert_eq!(t.contents[0]["parts"][0]["text"], "hi");
    }

    #[test]
    fn empty_and_system_only_error() {
        assert!(items_to_contents(&[]).is_err());
        let sys = vec![Item::System { content: vec![ContentPart::Text { text: "x".into() }] }];
        assert!(items_to_contents(&sys).is_err());
    }

    #[test]
    fn assistant_becomes_model_role() {
        let items = vec![
            user("hi"),
            Item::AssistantMessage { content: vec![ContentPart::Text { text: "yo".into() }], agent: None },
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(t.contents[1]["role"], "model");
    }

    #[test]
    fn tool_call_and_result_roundtrip_id_and_name() {
        let items = vec![
            user("search cats"),
            Item::ToolCall { call_id: "fc_0".into(), name: "search".into(), args: json!({"q":"cats"}) },
            Item::ToolResult { call_id: "fc_0".into(), content: vec![ContentPart::Text { text: "{\"hits\":3}".into() }] },
        ];
        let t = items_to_contents(&items).unwrap();
        let call = &t.contents[1];
        assert_eq!(call["role"], "model");
        assert_eq!(call["parts"][0]["functionCall"]["name"], "search");
        assert_eq!(call["parts"][0]["functionCall"]["id"], "fc_0");
        let result = &t.contents[2];
        assert_eq!(result["role"], "user");
        let fr = &result["parts"][0]["functionResponse"];
        assert_eq!(fr["name"], "search"); // recovered from call_id->name map
        assert_eq!(fr["id"], "fc_0");
        assert_eq!(fr["response"]["hits"], 3); // parsed JSON object
    }

    #[test]
    fn non_object_tool_result_wrapped_in_result_key() {
        let items = vec![
            user("x"),
            Item::ToolCall { call_id: "fc_0".into(), name: "echo".into(), args: json!({}) },
            Item::ToolResult { call_id: "fc_0".into(), content: vec![ContentPart::Text { text: "plain text".into() }] },
        ];
        let t = items_to_contents(&items).unwrap();
        assert_eq!(t.contents[2]["parts"][0]["functionResponse"]["response"]["result"], "plain text");
    }

    #[test]
    fn tool_result_without_matching_call_errors() {
        let items = vec![
            user("x"),
            Item::ToolResult { call_id: "ghost".into(), content: vec![ContentPart::Text { text: "{}".into() }] },
        ];
        assert!(items_to_contents(&items).is_err());
    }

    #[test]
    fn inline_base64_image_becomes_inline_data() {
        let items = vec![Item::UserMessage {
            content: vec![ContentPart::Image {
                source: MediaSource::Base64 { mime_type: "image/png".into(), data: "AAAA".into() },
            }],
        }];
        let t = items_to_contents(&items).unwrap();
        let part = &t.contents[0]["parts"][0]["inlineData"];
        assert_eq!(part["mimeType"], "image/png");
        assert_eq!(part["data"], "AAAA");
    }

    #[test]
    fn url_image_skipped() {
        let items = vec![Item::UserMessage {
            content: vec![
                ContentPart::Text { text: "see".into() },
                ContentPart::Image { source: MediaSource::Url { url: "http://x/y.png".into() } },
            ],
        }];
        let t = items_to_contents(&items).unwrap();
        // Only the text part survives.
        assert_eq!(t.contents[0]["parts"].as_array().unwrap().len(), 1);
        assert_eq!(t.contents[0]["parts"][0]["text"], "see");
    }
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p paigasus-helikon-providers-gemini request::`
Expected: FAIL — `cannot find function items_to_contents`.

- [ ] **Step 3: Implement** (add `pub(crate) mod request;` to translate/mod.rs, then prepend)

```rust
/// `contents` + optional `systemInstruction`.
pub(crate) struct TranslatedContents {
    pub(crate) system: Option<Value>,
    pub(crate) contents: Vec<Value>,
}

/// Translate core items into Gemini `contents`. Returns an error on an empty
/// or system-only conversation (Gemini 400s on empty contents).
pub(crate) fn items_to_contents(items: &[Item]) -> Result<TranslatedContents, ModelError> {
    // Build call_id -> name map from all tool calls (ToolResult has no name).
    let mut call_names: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for it in items {
        match it {
            Item::ToolCall { call_id, name, .. } => {
                call_names.insert(call_id.as_str(), name.as_str());
            }
            Item::AssistantMessage { content, .. } => {
                for p in content {
                    if let ContentPart::ToolUse { call_id, name, .. } = p {
                        call_names.insert(call_id.as_str(), name.as_str());
                    }
                }
            }
            _ => {}
        }
    }

    let mut system_parts: Vec<Value> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();

    for it in items {
        match it {
            Item::System { content } => {
                system_parts.extend(text_parts(content));
            }
            Item::UserMessage { content } => {
                contents.push(json!({ "role": "user", "parts": content_parts(content) }));
            }
            Item::AssistantMessage { content, .. } => {
                contents.push(json!({ "role": "model", "parts": assistant_parts(content) }));
            }
            Item::ToolCall { call_id, name, args } => {
                contents.push(json!({
                    "role": "model",
                    "parts": [ { "functionCall": { "id": call_id, "name": name, "args": args } } ]
                }));
            }
            Item::ToolResult { call_id, content } => {
                let name = call_names.get(call_id.as_str()).ok_or_else(|| {
                    ModelError::Other(anyhow::anyhow!(
                        "tool result references unknown call_id {call_id}"
                    ))
                })?;
                contents.push(json!({
                    "role": "user",
                    "parts": [ {
                        "functionResponse": {
                            "id": call_id,
                            "name": name,
                            "response": tool_response_object(content),
                        }
                    } ]
                }));
            }
        }
    }

    if contents.is_empty() {
        return Err(ModelError::Other(anyhow::anyhow!(
            "gemini request has no user/model turns (empty or system-only conversation)"
        )));
    }

    let system = (!system_parts.is_empty()).then(|| json!({ "parts": system_parts }));
    Ok(TranslatedContents { system, contents })
}

fn text_parts(content: &[ContentPart]) -> Vec<Value> {
    content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(json!({ "text": text })),
            _ => None,
        })
        .collect()
}

fn content_parts(content: &[ContentPart]) -> Vec<Value> {
    let mut out = Vec::new();
    for p in content {
        match p {
            ContentPart::Text { text } => out.push(json!({ "text": text })),
            ContentPart::Image { source: MediaSource::Base64 { mime_type, data } } => {
                out.push(json!({ "inlineData": { "mimeType": mime_type, "data": data } }));
            }
            other => {
                tracing::warn!(
                    target: "paigasus::gemini::translate",
                    part = ?std::mem::discriminant(other),
                    "unsupported content part; skipping"
                );
            }
        }
    }
    out
}

fn assistant_parts(content: &[ContentPart]) -> Vec<Value> {
    let mut out = Vec::new();
    for p in content {
        match p {
            ContentPart::Text { text } => out.push(json!({ "text": text })),
            ContentPart::ToolUse { call_id, name, args } => {
                out.push(json!({ "functionCall": { "id": call_id, "name": name, "args": args } }));
            }
            ContentPart::Reasoning { .. } => { /* deferred (D3) */ }
            other => {
                tracing::warn!(
                    target: "paigasus::gemini::translate",
                    part = ?std::mem::discriminant(other),
                    "unsupported assistant part; skipping"
                );
            }
        }
    }
    out
}

/// Reduce a tool result's content parts to a JSON object for `functionResponse.response`.
fn tool_response_object(content: &[ContentPart]) -> Value {
    let text: String = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => Value::Object(m),
        Ok(other) => json!({ "result": other }),
        Err(_) => json!({ "result": text }),
    }
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p paigasus-helikon-providers-gemini request::`
Expected: PASS (8 tests).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/src/translate/mod.rs crates/paigasus-helikon-providers-gemini/src/translate/request.rs
git commit -m "feat(providers): SMA-449 gemini request translation

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: Tool translation (`translate/tools.rs`)

**Files:**
- Create: `crates/paigasus-helikon-providers-gemini/src/translate/tools.rs`
- Modify: `src/translate/mod.rs` (`pub(crate) mod tools;`)

**Interfaces:**
- Consumes: `schema::sanitize_schema`.
- Produces:
  - `tools::function_declarations(defs: &[ToolDef]) -> Vec<Value>` — `[{ functionDeclarations: [...] }]` (empty `Vec` when no tools).
  - `tools::function_calling_config(choice: Option<&ToolChoice>, all_names: &[String]) -> Option<Value>` — the `toolConfig.functionCallingConfig` value, or `None`.

- [ ] **Step 1: Failing tests**

`src/translate/tools.rs`:
```rust
//! Translate core tool defs + tool choice into Gemini `tools` / `toolConfig`.

use paigasus_helikon_core::{ToolChoice, ToolDef};
use serde_json::{json, Value};

use super::schema::sanitize_schema;

#[cfg(test)]
mod tests {
    use super::*;

    fn defs() -> Vec<ToolDef> {
        vec![ToolDef {
            name: "search".into(),
            description: "search the web".into(),
            schema: json!({ "type": "object", "properties": { "q": { "type": "string" } }, "additionalProperties": false }),
        }]
    }

    #[test]
    fn declarations_wrap_and_sanitize() {
        let out = function_declarations(&defs());
        let fd = &out[0]["functionDeclarations"][0];
        assert_eq!(fd["name"], "search");
        assert_eq!(fd["description"], "search the web");
        // additionalProperties stripped by sanitizer
        assert!(fd["parameters"].get("additionalProperties").is_none());
        assert_eq!(fd["parameters"]["properties"]["q"]["type"], "string");
    }

    #[test]
    fn no_tools_is_empty() {
        assert!(function_declarations(&[]).is_empty());
    }

    #[test]
    fn choice_modes() {
        let names = vec!["search".to_owned()];
        assert_eq!(function_calling_config(Some(&ToolChoice::Auto), &names).unwrap()["mode"], "AUTO");
        assert_eq!(function_calling_config(Some(&ToolChoice::None), &names).unwrap()["mode"], "NONE");
        let req = function_calling_config(Some(&ToolChoice::Required), &names).unwrap();
        assert_eq!(req["mode"], "ANY");
        assert_eq!(req["allowedFunctionNames"], json!(["search"]));
        let one = function_calling_config(Some(&ToolChoice::Tool { name: "search".into() }), &names).unwrap();
        assert_eq!(one["mode"], "ANY");
        assert_eq!(one["allowedFunctionNames"], json!(["search"]));
    }

    #[test]
    fn no_choice_is_none() {
        assert!(function_calling_config(None, &[]).is_none());
    }
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p paigasus-helikon-providers-gemini tools::`
Expected: FAIL.

- [ ] **Step 3: Implement** (`pub(crate) mod tools;` in translate/mod.rs, then prepend)

```rust
/// Build the Gemini `tools` array (empty when there are no tool defs).
pub(crate) fn function_declarations(defs: &[ToolDef]) -> Vec<Value> {
    if defs.is_empty() {
        return Vec::new();
    }
    let decls: Vec<Value> = defs
        .iter()
        .map(|d| {
            json!({
                "name": d.name,
                "description": d.description,
                "parameters": sanitize_schema(&d.schema),
            })
        })
        .collect();
    vec![json!({ "functionDeclarations": decls })]
}

/// Build `toolConfig.functionCallingConfig`, or `None` when no choice is set.
pub(crate) fn function_calling_config(
    choice: Option<&ToolChoice>,
    all_names: &[String],
) -> Option<Value> {
    let c = choice?;
    let v = match c {
        ToolChoice::Auto => json!({ "mode": "AUTO" }),
        ToolChoice::None => json!({ "mode": "NONE" }),
        ToolChoice::Required => json!({ "mode": "ANY", "allowedFunctionNames": all_names }),
        ToolChoice::Tool { name } => json!({ "mode": "ANY", "allowedFunctionNames": [name] }),
    };
    Some(v)
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p paigasus-helikon-providers-gemini tools::`
Expected: PASS (4 tests).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/src/translate/mod.rs crates/paigasus-helikon-providers-gemini/src/translate/tools.rs
git commit -m "feat(providers): SMA-449 gemini tool translation

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: Response format + conflict guard (`translate/response_format.rs`)

**Files:**
- Create: `crates/paigasus-helikon-providers-gemini/src/translate/response_format.rs`
- Modify: `src/translate/mod.rs` (`pub(crate) mod response_format;`)

**Interfaces:**
- Consumes: `schema::sanitize_schema`.
- Produces:
  - `response_format::response_format_fields(rf: Option<&ResponseFormat>) -> Option<(String, Option<Value>)>` — `(responseMimeType, responseSchema?)` or `None` for `Text`/`None`.
  - `response_format::validate_conflict(rf: Option<&ResponseFormat>, tools: &[ToolDef], choice: Option<&ToolChoice>) -> Result<(), ModelError>`.

- [ ] **Step 1: Failing tests**

`src/translate/response_format.rs`:
```rust
//! Native structured output (responseMimeType + responseSchema) + conflict guard.

use paigasus_helikon_core::{ModelError, ResponseFormat, ToolChoice, ToolDef};
use serde_json::{json, Value};

use super::schema::sanitize_schema;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_object_sets_mime_only() {
        let (mime, schema) = response_format_fields(Some(&ResponseFormat::JsonObject)).unwrap();
        assert_eq!(mime, "application/json");
        assert!(schema.is_none());
    }

    #[test]
    fn json_schema_sets_mime_and_sanitized_schema() {
        let rf = ResponseFormat::JsonSchema {
            name: "Out".into(),
            schema: json!({ "type": "object", "additionalProperties": false, "properties": {} }),
            strict: true,
        };
        let (mime, schema) = response_format_fields(Some(&rf)).unwrap();
        assert_eq!(mime, "application/json");
        assert!(schema.unwrap().get("additionalProperties").is_none());
    }

    #[test]
    fn text_and_none_produce_nothing() {
        assert!(response_format_fields(Some(&ResponseFormat::Text)).is_none());
        assert!(response_format_fields(None).is_none());
    }

    #[test]
    fn structured_output_with_tools_conflicts() {
        let rf = ResponseFormat::JsonObject;
        let tdef = vec![ToolDef { name: "t".into(), description: "".into(), schema: json!({}) }];
        assert!(validate_conflict(Some(&rf), &tdef, None).is_err());
    }

    #[test]
    fn structured_output_with_active_tool_choice_conflicts() {
        let rf = ResponseFormat::JsonObject;
        assert!(validate_conflict(Some(&rf), &[], Some(&ToolChoice::Auto)).is_err());
    }

    #[test]
    fn structured_output_with_choice_none_is_allowed() {
        let rf = ResponseFormat::JsonObject;
        assert!(validate_conflict(Some(&rf), &[], Some(&ToolChoice::None)).is_ok());
    }

    #[test]
    fn structured_output_no_tools_ok() {
        // The finalize-after-tool-use case: tools empty, history may contain function parts.
        assert!(validate_conflict(Some(&ResponseFormat::JsonObject), &[], None).is_ok());
    }
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p paigasus-helikon-providers-gemini response_format::`
Expected: FAIL.

- [ ] **Step 3: Implement** (`pub(crate) mod response_format;` then prepend)

```rust
/// Map a `ResponseFormat` to `(responseMimeType, responseSchema?)`.
pub(crate) fn response_format_fields(
    rf: Option<&ResponseFormat>,
) -> Option<(String, Option<Value>)> {
    match rf {
        Some(ResponseFormat::JsonObject) => Some(("application/json".to_owned(), None)),
        Some(ResponseFormat::JsonSchema { schema, .. }) => {
            Some(("application/json".to_owned(), Some(sanitize_schema(schema))))
        }
        Some(ResponseFormat::Text) | None => None,
    }
}

/// Reject structured output combined with function calling. Gemini does not
/// support `responseSchema` together with tools. Inspects only the active
/// request (not history) so the loop's finalize-after-tool-use (tools empty,
/// no active choice) is allowed.
pub(crate) fn validate_conflict(
    rf: Option<&ResponseFormat>,
    tools: &[ToolDef],
    choice: Option<&ToolChoice>,
) -> Result<(), ModelError> {
    let structured = matches!(
        rf,
        Some(ResponseFormat::JsonObject) | Some(ResponseFormat::JsonSchema { .. })
    );
    if !structured {
        return Ok(());
    }
    let active_choice = !matches!(choice, None | Some(ToolChoice::None));
    if !tools.is_empty() || active_choice {
        return Err(ModelError::Other(anyhow::anyhow!(
            "gemini does not support structured output (responseSchema) together with \
             function calling; omit tools / set ToolChoice::None"
        )));
    }
    Ok(())
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p paigasus-helikon-providers-gemini response_format::`
Expected: PASS (7 tests).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/src/translate/mod.rs crates/paigasus-helikon-providers-gemini/src/translate/response_format.rs
git commit -m "feat(providers): SMA-449 gemini structured-output + conflict guard

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 8: Request orchestration + wire-format snapshot suite (`translate/mod.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/src/translate/mod.rs`
- Create: snapshot files under `crates/paigasus-helikon-providers-gemini/src/translate/snapshots/` (generated by `cargo insta accept`).

**Interfaces:**
- Consumes: `request::items_to_contents`, `tools::{function_declarations, function_calling_config}`, `response_format::{response_format_fields, validate_conflict}`, `builder::Config`.
- Produces:
  - `PreparedRequest { body: serde_json::Value }`
  - `build_request(cfg: &Config, req: &ModelRequest) -> Result<PreparedRequest, ModelError>` — assembles the full Gemini request body, running all guards.

- [ ] **Step 1: Add the orchestrator + the snapshot tests**

Top of `src/translate/mod.rs` (keep the existing `pub(crate) mod …;` lines), add imports + the orchestrator:
```rust
use paigasus_helikon_core::{ModelError, ModelRequest};
use serde_json::{Map, Value};

use crate::builder::Config;
use request::items_to_contents;
use response_format::{response_format_fields, validate_conflict};
use tools::{function_calling_config, function_declarations};

/// Fully-assembled Gemini request body.
#[derive(Debug)]
pub(crate) struct PreparedRequest {
    pub(crate) body: Value,
}

/// Assemble the Gemini `generateContent` request body, running all guards.
pub(crate) fn build_request(cfg: &Config, req: &ModelRequest) -> Result<PreparedRequest, ModelError> {
    let s = &req.model_settings;

    validate_conflict(s.response_format.as_ref(), &req.tools, s.tool_choice.as_ref())?;

    // tool_choice Tool/Required require tools.
    if matches!(
        s.tool_choice,
        Some(paigasus_helikon_core::ToolChoice::Required)
            | Some(paigasus_helikon_core::ToolChoice::Tool { .. })
    ) && req.tools.is_empty()
    {
        return Err(ModelError::Other(anyhow::anyhow!(
            "tool_choice requires at least one tool"
        )));
    }

    let translated = items_to_contents(&req.messages)?;
    let mut body = Map::new();
    body.insert("contents".into(), Value::Array(translated.contents));
    if let Some(sys) = translated.system {
        body.insert("systemInstruction".into(), sys);
    }

    let decls = function_declarations(&req.tools);
    if !decls.is_empty() {
        body.insert("tools".into(), Value::Array(decls));
    }
    let all_names: Vec<String> = req.tools.iter().map(|t| t.name.clone()).collect();
    if let Some(fcc) = function_calling_config(s.tool_choice.as_ref(), &all_names) {
        body.insert("toolConfig".into(), serde_json::json!({ "functionCallingConfig": fcc }));
    }

    let mut gen = Map::new();
    if let Some(t) = s.temperature {
        gen.insert("temperature".into(), serde_json::json!(t));
    }
    if let Some(p) = s.top_p {
        gen.insert("topP".into(), serde_json::json!(p));
    }
    if let Some(m) = s.max_output_tokens {
        gen.insert("maxOutputTokens".into(), serde_json::json!(m));
    }
    if let Some((mime, schema)) = response_format_fields(s.response_format.as_ref()) {
        gen.insert("responseMimeType".into(), Value::String(mime));
        if let Some(sc) = schema {
            gen.insert("responseSchema".into(), sc);
        }
    }
    if !gen.is_empty() {
        body.insert("generationConfig".into(), Value::Object(gen));
    }

    // model_id is carried by the URL, not the body (Developer/Vertex differ);
    // include it in the snapshot projection for stability.
    let _ = &cfg.model_id;
    Ok(PreparedRequest { body: Value::Object(body) })
}
```

- [ ] **Step 2: Add the snapshot tests** (append a `#[cfg(test)] mod snap` to `translate/mod.rs`)

```rust
#[cfg(test)]
mod snap {
    use super::*;
    use crate::builder::{Config, Transport};
    use paigasus_helikon_core::{
        ContentPart, Item, ModelCapabilities, ModelRequest, ResponseFormat, ToolChoice, ToolDef,
    };
    use serde_json::json;

    fn cfg() -> Config {
        Config {
            http: reqwest::Client::new(),
            base_url: None,
            model_id: "gemini-2.5-flash".into(),
            transport: Transport::Developer,
            auth: crate::auth::Auth::ApiKey("k".into()),
            capabilities: ModelCapabilities::empty(),
        }
    }
    fn user(s: &str) -> Item {
        Item::UserMessage { content: vec![ContentPart::Text { text: s.into() }] }
    }
    fn body(req: ModelRequest) -> serde_json::Value {
        build_request(&cfg(), &req).unwrap().body
    }

    #[test]
    fn snap_plain_text_turn() {
        let mut r = ModelRequest::new();
        r.messages = vec![user("hello")];
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_generation_config() {
        let mut r = ModelRequest::new();
        r.messages = vec![user("hi")];
        r.model_settings.temperature = Some(0.7);
        r.model_settings.top_p = Some(0.9);
        r.model_settings.max_output_tokens = Some(256);
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_system_instruction() {
        let mut r = ModelRequest::new();
        r.messages = vec![
            Item::System { content: vec![ContentPart::Text { text: "be terse".into() }] },
            user("hi"),
        ];
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_tool_declarations_and_choice_auto() {
        let mut r = ModelRequest::new();
        r.messages = vec![user("search")];
        r.tools = vec![ToolDef {
            name: "search".into(),
            description: "search".into(),
            schema: json!({ "type": "object", "properties": { "q": { "type": "string" } } }),
        }];
        r.model_settings.tool_choice = Some(ToolChoice::Auto);
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_tool_call_and_result() {
        let mut r = ModelRequest::new();
        r.messages = vec![
            user("search cats"),
            Item::ToolCall { call_id: "fc_0".into(), name: "search".into(), args: json!({"q":"cats"}) },
            Item::ToolResult { call_id: "fc_0".into(), content: vec![ContentPart::Text { text: "{\"hits\":3}".into() }] },
        ];
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_parallel_same_name_tool_calls() {
        let mut r = ModelRequest::new();
        r.messages = vec![
            user("two searches"),
            Item::ToolCall { call_id: "fc_0".into(), name: "search".into(), args: json!({"q":"a"}) },
            Item::ToolCall { call_id: "fc_1".into(), name: "search".into(), args: json!({"q":"b"}) },
            Item::ToolResult { call_id: "fc_0".into(), content: vec![ContentPart::Text { text: "{\"n\":1}".into() }] },
            Item::ToolResult { call_id: "fc_1".into(), content: vec![ContentPart::Text { text: "{\"n\":2}".into() }] },
        ];
        let b = body(r);
        // Both responses carry the real name "search" but distinct ids.
        assert_eq!(b["contents"][3]["parts"][0]["functionResponse"]["name"], "search");
        assert_eq!(b["contents"][3]["parts"][0]["functionResponse"]["id"], "fc_0");
        assert_eq!(b["contents"][4]["parts"][0]["functionResponse"]["id"], "fc_1");
        insta::assert_json_snapshot!(b);
    }

    #[test]
    fn snap_structured_output_json_schema() {
        let mut r = ModelRequest::new();
        r.messages = vec![user("extract")];
        r.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Person".into(),
            schema: json!({ "type": "object", "properties": { "name": { "type": "string" } }, "additionalProperties": false }),
            strict: true,
        });
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn snap_inline_image() {
        let mut r = ModelRequest::new();
        r.messages = vec![Item::UserMessage {
            content: vec![
                ContentPart::Text { text: "what is this".into() },
                ContentPart::Image { source: paigasus_helikon_core::MediaSource::Base64 {
                    mime_type: "image/png".into(), data: "AAAA".into() } },
            ],
        }];
        insta::assert_json_snapshot!(body(r));
    }

    #[test]
    fn structured_output_plus_tools_errors() {
        let mut r = ModelRequest::new();
        r.messages = vec![user("x")];
        r.tools = vec![ToolDef { name: "t".into(), description: "".into(), schema: json!({}) }];
        r.model_settings.response_format = Some(ResponseFormat::JsonObject);
        let err = build_request(&cfg(), &r).unwrap_err();
        insta::assert_snapshot!(err.to_string());
    }

    #[test]
    fn finalize_after_tool_use_allowed() {
        // tools: [] + JsonSchema, with prior function parts in history -> no error.
        let mut r = ModelRequest::new();
        r.messages = vec![
            user("q"),
            Item::ToolCall { call_id: "fc_0".into(), name: "search".into(), args: json!({}) },
            Item::ToolResult { call_id: "fc_0".into(), content: vec![ContentPart::Text { text: "{}".into() }] },
        ];
        r.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Out".into(), schema: json!({ "type": "object", "properties": {} }), strict: true,
        });
        assert!(build_request(&cfg(), &r).is_ok());
    }

    #[test]
    fn empty_conversation_errors() {
        let r = ModelRequest::new();
        assert!(build_request(&cfg(), &r).is_err());
    }
}
```

- [ ] **Step 2b:** Make `auth::Auth` reachable from tests — it is `pub(crate)`, fine within the crate.

- [ ] **Step 3: Run, generate snapshots**

Run: `cargo test -p paigasus-helikon-providers-gemini snap::`
Expected: snapshot tests report "new snapshot" pending.
Run: `cargo insta accept` (or `INSTA_UPDATE=always cargo test -p paigasus-helikon-providers-gemini snap::`)
Then re-run: `cargo test -p paigasus-helikon-providers-gemini snap::`
Expected: PASS.

- [ ] **Step 4: Eyeball the generated snapshots**

Open each `.snap` under `src/translate/snapshots/` and confirm: `role` is `user`/`model`; `systemInstruction.parts[0].text` present; `functionDeclarations` carry sanitized `parameters`; structured-output snapshot has `generationConfig.responseMimeType` + `responseSchema` and **no** `tools`; the conflict snapshot is the error message; no `maxOutputTokens` in `snap_plain_text_turn`.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/src/translate/mod.rs \
        crates/paigasus-helikon-providers-gemini/src/translate/snapshots/
git commit -m "feat(providers): SMA-449 gemini request assembly + wire snapshots

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 9: SSE chunk types + stream translator (`sse.rs`, `stream.rs`)

**Files:**
- Create: `crates/paigasus-helikon-providers-gemini/src/sse.rs`
- Create: `crates/paigasus-helikon-providers-gemini/src/stream.rs`
- Modify: `src/lib.rs` (`mod sse; mod stream;`)

**Interfaces:**
- Produces:
  - `sse::GeminiChunk` (serde `Deserialize` of one SSE `data:` payload) with `candidates`, `usage_metadata`, `prompt_feedback`.
  - `stream::StreamTranslator::new() -> Self`; `consume(&mut self, chunk: GeminiChunk) -> Vec<Result<ModelEvent, ModelError>>`; `finish(&mut self) -> Vec<Result<ModelEvent, ModelError>>` (emits buffered `Finish` only if a `finishReason` was seen).

- [ ] **Step 1: Define `sse.rs` chunk types**

```rust
//! Serde types for one Gemini `GenerateContentResponse` SSE chunk.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiChunk {
    #[serde(default)]
    pub(crate) candidates: Vec<Candidate>,
    #[serde(default)]
    pub(crate) usage_metadata: Option<UsageMetadata>,
    #[serde(default)]
    pub(crate) prompt_feedback: Option<PromptFeedback>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Candidate {
    #[serde(default)]
    pub(crate) content: Option<Content>,
    #[serde(default)]
    pub(crate) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Content {
    #[serde(default)]
    pub(crate) parts: Vec<Part>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Part {
    #[serde(default)]
    pub(crate) text: Option<String>,
    #[serde(default)]
    pub(crate) function_call: Option<FunctionCall>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FunctionCall {
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) args: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageMetadata {
    #[serde(default)]
    pub(crate) prompt_token_count: u32,
    #[serde(default)]
    pub(crate) candidates_token_count: u32,
    #[serde(default)]
    pub(crate) cached_content_token_count: Option<u32>,
    #[serde(default)]
    pub(crate) thoughts_token_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PromptFeedback {
    #[serde(default)]
    pub(crate) block_reason: Option<String>,
}
```

- [ ] **Step 2: Failing tests for `stream.rs`**

```rust
//! Translate Gemini SSE chunks into core `ModelEvent`s.

use paigasus_helikon_core::{FinishReason, ModelError, ModelEvent};

use crate::sse::GeminiChunk;

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(j: serde_json::Value) -> GeminiChunk {
        serde_json::from_value(j).unwrap()
    }

    #[test]
    fn text_delta_emitted() {
        let mut t = StreamTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [ { "text": "hello" } ] } } ]
        })));
        assert!(matches!(&evs[0], Ok(ModelEvent::TokenDelta { text }) if text == "hello"));
    }

    #[test]
    fn function_call_uses_native_id() {
        let mut t = StreamTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [
                { "functionCall": { "id": "fc_x", "name": "search", "args": {"q":"c"} } }
            ] } } ]
        })));
        match &evs[0] {
            Ok(ModelEvent::ToolCallDelta { call_id, name, args_delta }) => {
                assert_eq!(call_id, "fc_x");
                assert_eq!(name.as_deref(), Some("search"));
                assert!(args_delta.contains("\"q\""));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn function_call_without_id_synthesizes() {
        let mut t = StreamTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [ { "functionCall": { "name": "x", "args": {} } } ] } } ]
        })));
        match &evs[0] {
            Ok(ModelEvent::ToolCallDelta { call_id, .. }) => assert!(!call_id.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn usage_maps_thoughts_to_reasoning_tokens() {
        let mut t = StreamTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({
            "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 5,
                "cachedContentTokenCount": 2, "thoughtsTokenCount": 3 }
        })));
        match &evs[0] {
            Ok(ModelEvent::Usage { input_tokens, output_tokens, cached_input_tokens, reasoning_tokens }) => {
                assert_eq!(*input_tokens, 10);
                assert_eq!(*output_tokens, 5);
                assert_eq!(*cached_input_tokens, Some(2));
                assert_eq!(*reasoning_tokens, Some(3));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn finish_reason_stop_emitted_on_finish() {
        let mut t = StreamTranslator::new();
        let _ = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [ { "text": "hi" } ] }, "finishReason": "STOP" } ]
        })));
        let fin = t.finish();
        assert!(matches!(&fin[0], Ok(ModelEvent::Finish { reason: FinishReason::Stop })));
    }

    #[test]
    fn finish_with_function_call_is_tool_calls() {
        let mut t = StreamTranslator::new();
        let _ = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [ { "functionCall": { "name": "x", "args": {} } } ] }, "finishReason": "STOP" } ]
        })));
        let fin = t.finish();
        assert!(matches!(&fin[0], Ok(ModelEvent::Finish { reason: FinishReason::ToolCalls })));
    }

    #[test]
    fn truncated_stream_emits_no_finish() {
        let mut t = StreamTranslator::new();
        let _ = t.consume(chunk(serde_json::json!({
            "candidates": [ { "content": { "parts": [ { "text": "partial" } ] } } ]
        })));
        assert!(t.finish().is_empty());
    }

    #[test]
    fn blocked_prompt_is_refused() {
        let mut t = StreamTranslator::new();
        let evs = t.consume(chunk(serde_json::json!({ "promptFeedback": { "blockReason": "SAFETY" } })));
        assert!(matches!(&evs[0], Err(ModelError::Refused { .. })));
    }

    #[test]
    fn safety_finish_maps_to_content_filter() {
        let mut t = StreamTranslator::new();
        let _ = t.consume(chunk(serde_json::json!({ "candidates": [ { "finishReason": "SAFETY" } ] })));
        let fin = t.finish();
        assert!(matches!(&fin[0], Ok(ModelEvent::Finish { reason: FinishReason::ContentFilter })));
    }
}
```

- [ ] **Step 3: Run, verify fail**

Run: `cargo test -p paigasus-helikon-providers-gemini stream::`
Expected: FAIL.

- [ ] **Step 4: Implement** (`mod sse; mod stream;` in lib.rs, then prepend to stream.rs)

```rust
/// Stateful translator from Gemini SSE chunks to core `ModelEvent`s.
pub(crate) struct StreamTranslator {
    fn_index: usize,
    saw_function_call: bool,
    finish_reason: Option<String>,
    pending_usage: Option<ModelEvent>,
}

impl StreamTranslator {
    pub(crate) fn new() -> Self {
        Self { fn_index: 0, saw_function_call: false, finish_reason: None, pending_usage: None }
    }

    pub(crate) fn consume(&mut self, chunk: GeminiChunk) -> Vec<Result<ModelEvent, ModelError>> {
        let mut out = Vec::new();

        if let Some(pf) = &chunk.prompt_feedback {
            if let Some(reason) = &pf.block_reason {
                out.push(Err(ModelError::Refused { reason: format!("prompt blocked: {reason}") }));
                return out;
            }
        }

        if let Some(cand) = chunk.candidates.into_iter().next() {
            if let Some(content) = cand.content {
                for part in content.parts {
                    if let Some(text) = part.text {
                        out.push(Ok(ModelEvent::TokenDelta { text }));
                    } else if let Some(fc) = part.function_call {
                        self.saw_function_call = true;
                        let call_id = fc.id.unwrap_or_else(|| {
                            let id = format!("fc_{}", self.fn_index);
                            self.fn_index += 1;
                            id
                        });
                        out.push(Ok(ModelEvent::ToolCallDelta {
                            call_id,
                            name: Some(fc.name),
                            args_delta: fc.args.to_string(),
                        }));
                    }
                }
            }
            if let Some(fr) = cand.finish_reason {
                self.finish_reason = Some(fr);
            }
        }

        if let Some(u) = chunk.usage_metadata {
            let usage = ModelEvent::Usage {
                input_tokens: u.prompt_token_count,
                output_tokens: u.candidates_token_count,
                cached_input_tokens: u.cached_content_token_count,
                reasoning_tokens: u.thoughts_token_count,
            };
            out.push(Ok(usage.clone()));
            self.pending_usage = Some(usage);
        }

        out
    }

    /// Emit the terminal `Finish` — only when a `finishReason` was observed.
    pub(crate) fn finish(&mut self) -> Vec<Result<ModelEvent, ModelError>> {
        let Some(reason) = self.finish_reason.take() else {
            return Vec::new();
        };
        let fr = match reason.as_str() {
            "STOP" if self.saw_function_call => FinishReason::ToolCalls,
            "STOP" => FinishReason::Stop,
            "MAX_TOKENS" => FinishReason::Length,
            "SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" => {
                FinishReason::ContentFilter
            }
            other => FinishReason::Other(other.to_owned()),
        };
        vec![Ok(ModelEvent::Finish { reason: fr })]
    }
}

use paigasus_helikon_core::FinishReason;
```
(Move the `use paigasus_helikon_core::FinishReason;` to the top with the other imports; shown inline for clarity.)

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p paigasus-helikon-providers-gemini stream:: sse::`
Expected: PASS (9 tests).

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/src/lib.rs \
        crates/paigasus-helikon-providers-gemini/src/sse.rs \
        crates/paigasus-helikon-providers-gemini/src/stream.rs
git commit -m "feat(providers): SMA-449 gemini SSE stream translator

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 10: Transport URL + headers (`transport.rs`)

**Files:**
- Create: `crates/paigasus-helikon-providers-gemini/src/transport.rs`
- Modify: `src/lib.rs` (`mod transport;`)

**Interfaces:**
- Consumes: `builder::{Config, Transport}`, `auth::Auth`.
- Produces:
  - `transport::stream_url(cfg: &Config) -> String` — full `:streamGenerateContent?alt=sse` URL for the selected transport.
  - `transport::auth_header(auth: &Auth) -> Result<(reqwest::header::HeaderName, String), ModelError>` — for `ApiKey`/`Bearer`; `Token` is resolved in `invoke` (async) before calling this with a synthesized `Bearer`.

- [ ] **Step 1: Failing tests**

`src/transport.rs`:
```rust
//! Build per-transport request URLs and auth headers.

use paigasus_helikon_core::ModelError;

use crate::auth::Auth;
use crate::builder::{Config, Transport};

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::ModelCapabilities;

    fn dev_cfg() -> Config {
        Config {
            http: reqwest::Client::new(),
            base_url: None,
            model_id: "gemini-2.5-flash".into(),
            transport: Transport::Developer,
            auth: Auth::ApiKey("k".into()),
            capabilities: ModelCapabilities::empty(),
        }
    }
    fn vertex_cfg(loc: &str) -> Config {
        Config {
            http: reqwest::Client::new(),
            base_url: None,
            model_id: "gemini-2.5-pro".into(),
            transport: Transport::Vertex { project: "proj".into(), location: loc.into() },
            auth: Auth::Bearer("ya29".into()),
            capabilities: ModelCapabilities::empty(),
        }
    }

    #[test]
    fn developer_stream_url() {
        let u = stream_url(&dev_cfg());
        assert_eq!(u, "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse");
    }

    #[test]
    fn vertex_regional_stream_url() {
        let u = stream_url(&vertex_cfg("us-central1"));
        assert_eq!(u, "https://us-central1-aiplatform.googleapis.com/v1/projects/proj/locations/us-central1/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse");
    }

    #[test]
    fn vertex_global_host() {
        let u = stream_url(&vertex_cfg("global"));
        assert!(u.starts_with("https://aiplatform.googleapis.com/v1/projects/proj/locations/global/"));
    }

    #[test]
    fn base_url_override_developer() {
        let mut c = dev_cfg();
        c.base_url = Some("http://localhost:8080".into());
        assert_eq!(stream_url(&c), "http://localhost:8080/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse");
    }

    #[test]
    fn api_key_header() {
        let (n, v) = auth_header(&Auth::ApiKey("secret".into())).unwrap();
        assert_eq!(n.as_str(), "x-goog-api-key");
        assert_eq!(v, "secret");
    }

    #[test]
    fn bearer_header() {
        let (n, v) = auth_header(&Auth::Bearer("ya29".into())).unwrap();
        assert_eq!(n.as_str(), "authorization");
        assert_eq!(v, "Bearer ya29");
    }
}
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p paigasus-helikon-providers-gemini transport::`
Expected: FAIL.

- [ ] **Step 3: Implement** (`mod transport;` in lib.rs, then prepend)

```rust
/// Host for the selected transport, honoring `base_url`.
fn host(cfg: &Config) -> String {
    if let Some(b) = &cfg.base_url {
        return b.trim_end_matches('/').to_owned();
    }
    match &cfg.transport {
        Transport::Developer => "https://generativelanguage.googleapis.com".to_owned(),
        Transport::Vertex { location, .. } if location == "global" => {
            "https://aiplatform.googleapis.com".to_owned()
        }
        Transport::Vertex { location, .. } => {
            format!("https://{location}-aiplatform.googleapis.com")
        }
    }
}

/// Streaming endpoint URL (`:streamGenerateContent?alt=sse`).
pub(crate) fn stream_url(cfg: &Config) -> String {
    let host = host(cfg);
    match &cfg.transport {
        Transport::Developer => format!(
            "{host}/v1beta/models/{}:streamGenerateContent?alt=sse",
            cfg.model_id
        ),
        Transport::Vertex { project, location } => format!(
            "{host}/v1/projects/{project}/locations/{location}/publishers/google/models/{}:streamGenerateContent?alt=sse",
            cfg.model_id
        ),
    }
}

/// Auth header for a non-async credential (`ApiKey`/`Bearer`).
pub(crate) fn auth_header(
    auth: &Auth,
) -> Result<(reqwest::header::HeaderName, String), ModelError> {
    use reqwest::header::{AUTHORIZATION, HeaderName};
    match auth {
        Auth::ApiKey(k) => Ok((HeaderName::from_static("x-goog-api-key"), k.clone())),
        Auth::Bearer(b) => Ok((AUTHORIZATION, format!("Bearer {b}"))),
        Auth::Token(_) => Err(ModelError::Other(anyhow::anyhow!(
            "Auth::Token must be resolved before auth_header"
        ))),
    }
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p paigasus-helikon-providers-gemini transport::`
Expected: PASS (6 tests).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/src/lib.rs crates/paigasus-helikon-providers-gemini/src/transport.rs
git commit -m "feat(providers): SMA-449 gemini transport url + headers

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 11: `impl Model` for `GeminiModel` (`model.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/src/model.rs`

**Interfaces:**
- Consumes: `translate::build_request`, `transport::{stream_url, auth_header}`, `stream::StreamTranslator`, `sse::GeminiChunk`, `error::{classify, parse_retry_after_ms}`, `auth::Auth`.
- Produces: `impl Model for GeminiModel` (`invoke`, `capabilities`, `provider`, `model`).

- [ ] **Step 1: Replace the inherent `provider`/`model` methods with the trait impl + `invoke`**

Edit `src/model.rs`: remove the inherent `pub fn provider` / `pub fn model` (keep `developer`/`vertex`/`from_env`/`from_config`), and add:
```rust
use async_stream::stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::auth::Auth;
use crate::error::{classify, parse_retry_after_ms};
use crate::sse::GeminiChunk;
use crate::stream::StreamTranslator;
use crate::transport::{auth_header, stream_url};

#[async_trait]
impl Model for GeminiModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let cfg = self.0.clone();
        let prepared = crate::translate::build_request(&cfg, &request)?;
        let url = stream_url(&cfg);

        // Resolve the auth header up-front (async for Auth::Token), inside the
        // caller's await so a token-fetch failure returns Err from invoke.
        let (header_name, header_value) = match &cfg.auth {
            Auth::Token(p) => {
                let tok = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(ModelError::Unavailable),
                    t = p.token() => t?,
                };
                (reqwest::header::AUTHORIZATION, format!("Bearer {tok}"))
            }
            other => auth_header(other)?,
        };

        let client = cfg.http.clone();
        let body = prepared.body;

        let s = stream! {
            let send_fut = client
                .post(&url)
                .header(header_name, header_value)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(&body)
                .send();

            let response = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                r = send_fut => match r {
                    Ok(r) => r,
                    Err(e) => { yield Err(ModelError::Transport(e.to_string())); return; }
                },
            };

            let status = response.status();
            if !status.is_success() {
                let retry_after_ms = parse_retry_after_ms(response.headers());
                let bytes = response.bytes().await.unwrap_or_default();
                let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&bytes);
                let (sfield, message) = parsed
                    .as_ref()
                    .ok()
                    .map(|v| {
                        let s = v.get("error").and_then(|e| e.get("status")).and_then(|t| t.as_str()).unwrap_or("").to_owned();
                        let m = v.get("error").and_then(|e| e.get("message")).and_then(|t| t.as_str()).unwrap_or("").to_owned();
                        (s, m)
                    })
                    .unwrap_or_else(|| (String::new(), String::from_utf8_lossy(&bytes).into_owned()));
                yield Err(classify(status.as_u16(), Some(&sfield).filter(|s| !s.is_empty()).map(|s| s.as_str()), &message, retry_after_ms));
                return;
            }

            let mut event_stream = response.bytes_stream().eventsource();
            let mut translator = StreamTranslator::new();
            loop {
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    n = event_stream.next() => n,
                };
                match next {
                    None => {
                        for ev in translator.finish() { yield ev; }
                        return;
                    }
                    Some(Err(e)) => { yield Err(ModelError::Transport(e.to_string())); return; }
                    Some(Ok(event)) => {
                        if event.data == "[DONE]" { continue; }
                        let chunk: GeminiChunk = match serde_json::from_str(&event.data) {
                            Ok(c) => c,
                            Err(parse_err) => {
                                tracing::warn!(
                                    target: "paigasus::gemini::sse",
                                    %parse_err, event_len = event.data.len(),
                                    "unparseable SSE event payload"
                                );
                                continue;
                            }
                        };
                        for ev in translator.consume(chunk) {
                            let is_err = ev.is_err();
                            yield ev;
                            if is_err { return; }
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
        "gemini"
    }
    fn model(&self) -> &str {
        &self.0.model_id
    }
}
```

- [ ] **Step 2: Keep builder tests green**

The Task 4 builder tests call `m.model()` / `m.provider()` — now trait methods. Add `use paigasus_helikon_core::Model;` to the `builder.rs` test module so the trait methods resolve. (Edit `src/builder.rs` test module imports.)

- [ ] **Step 3: Add model getter unit tests** (append to `model.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::Model;

    #[test]
    fn getters() {
        let m = GeminiModel::developer("gemini-2.5-flash").api_key("k").build().unwrap();
        assert_eq!(m.provider(), "gemini");
        assert_eq!(m.model(), "gemini-2.5-flash");
        assert!(m.capabilities().streaming);
    }
}
```

- [ ] **Step 4: Run the whole crate + fail-check**

Run: `cargo test -p paigasus-helikon-providers-gemini`
Expected: PASS (all prior tests + getters).

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/src/model.rs crates/paigasus-helikon-providers-gemini/src/builder.rs
git commit -m "feat(providers): SMA-449 gemini Model impl (invoke)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 12: wiremock transport + streaming integration tests

**Files:**
- Create: `crates/paigasus-helikon-providers-gemini/tests/gemini_wire.rs`
- Create: `crates/paigasus-helikon-providers-gemini/tests/gemini_streaming.rs`
- Create: `crates/paigasus-helikon-providers-gemini/.gitattributes`

**Interfaces:**
- Consumes: the public `GeminiModel` API + `CancellationToken`, `Model`.

- [ ] **Step 1: `.gitattributes`** (pin SSE-bearing fixtures to LF)

`crates/paigasus-helikon-providers-gemini/.gitattributes`:
```
*.snap text eol=lf
```

- [ ] **Step 2: Write `tests/gemini_wire.rs`** (URLs, headers, error mapping for both transports)

```rust
//! Wire-format / transport tests for the Gemini provider.

use futures_util::StreamExt;
use paigasus_helikon_core::{CancellationToken, ContentPart, Item, Model, ModelError, ModelRequest};
use paigasus_helikon_providers_gemini::GeminiModel;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sse_ok() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}\n\n",
            "text/event-stream",
        )
}

fn user(s: &str) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = vec![Item::UserMessage { content: vec![ContentPart::Text { text: s.into() }] }];
    r
}

#[tokio::test]
async fn developer_url_and_api_key_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1beta/models/gemini-2.5-flash:streamGenerateContent"))
        .and(query_param("alt", "sse"))
        .and(header("x-goog-api-key", "sk-test"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = GeminiModel::developer("gemini-2.5-flash")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model.invoke(user("hi"), CancellationToken::new()).await.unwrap();
    let mut texts = Vec::new();
    while let Some(ev) = s.next().await {
        if let Ok(paigasus_helikon_core::ModelEvent::TokenDelta { text }) = ev { texts.push(text); }
    }
    assert_eq!(texts, vec!["hi"]);
}

#[tokio::test]
async fn vertex_url_and_bearer_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/projects/proj/locations/us-central1/publishers/google/models/gemini-2.5-pro:streamGenerateContent"))
        .and(header("authorization", "Bearer ya29.token"))
        .respond_with(sse_ok())
        .mount(&server)
        .await;

    let model = GeminiModel::vertex("gemini-2.5-pro", "proj", "us-central1")
        .bearer_token("ya29.token")
        .base_url(server.uri())
        .build()
        .unwrap();
    let mut s = model.invoke(user("hi"), CancellationToken::new()).await.unwrap();
    while s.next().await.is_some() {}
}

#[tokio::test]
async fn http_429_maps_to_rate_limited() {
    let server = MockServer::start().await;
    let body = serde_json::json!({"error": {"status": "RESOURCE_EXHAUSTED", "message": "quota"}});
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "5").set_body_json(body))
        .mount(&server)
        .await;

    let model = GeminiModel::developer("gemini-2.5-flash").api_key("k").base_url(server.uri()).build().unwrap();
    let mut s = model.invoke(user("hi"), CancellationToken::new()).await.unwrap();
    let first = s.next().await.unwrap();
    assert!(matches!(first, Err(ModelError::RateLimited { retry_after_ms: Some(5000) })));
}
```

- [ ] **Step 3: Write `tests/gemini_streaming.rs`** (SSE → event sequences, blocked-prompt, truncation)

```rust
//! SSE -> ModelEvent translation tests via a mock server.

use futures_util::StreamExt;
use paigasus_helikon_core::{CancellationToken, ContentPart, FinishReason, Item, Model, ModelError, ModelEvent, ModelRequest};
use paigasus_helikon_providers_gemini::GeminiModel;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn run(sse: &'static str) -> Vec<Result<ModelEvent, ModelError>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_raw(sse, "text/event-stream"))
        .mount(&server)
        .await;
    let model = GeminiModel::developer("gemini-2.5-flash").api_key("k").base_url(server.uri()).build().unwrap();
    let mut r = ModelRequest::new();
    r.messages = vec![Item::UserMessage { content: vec![ContentPart::Text { text: "hi".into() }] }];
    let mut s = model.invoke(r, CancellationToken::new()).await.unwrap();
    let mut out = Vec::new();
    while let Some(ev) = s.next().await { out.push(ev); }
    out
}

#[tokio::test]
async fn text_then_finish() {
    let evs = run("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"a\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}\n\n").await;
    assert!(matches!(evs.first().unwrap(), Ok(ModelEvent::TokenDelta { text }) if text == "a"));
    assert!(matches!(evs.last().unwrap(), Ok(ModelEvent::Finish { reason: FinishReason::Stop })));
}

#[tokio::test]
async fn blocked_prompt_refused() {
    let evs = run("data: {\"promptFeedback\":{\"blockReason\":\"SAFETY\"}}\n\n").await;
    assert!(matches!(evs.first().unwrap(), Err(ModelError::Refused { .. })));
}

#[tokio::test]
async fn truncated_stream_no_finish() {
    let evs = run("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}\n\n").await;
    assert!(evs.iter().all(|e| !matches!(e, Ok(ModelEvent::Finish { .. }))));
}
```

- [ ] **Step 4: Run**

Run: `cargo test -p paigasus-helikon-providers-gemini --test gemini_wire --test gemini_streaming`
Expected: PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/tests/gemini_wire.rs \
        crates/paigasus-helikon-providers-gemini/tests/gemini_streaming.rs \
        crates/paigasus-helikon-providers-gemini/.gitattributes
git commit -m "test(providers): SMA-449 gemini wiremock transport + streaming tests

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 13: ADC token provider behind `vertex-adc` (`auth.rs`)

**Files:**
- Modify: `crates/paigasus-helikon-providers-gemini/src/auth.rs`
- Modify: `src/lib.rs` (feature-gated re-export)
- Modify: `src/model.rs` (`vertex_from_env`)

**Interfaces:**
- Produces (feature `vertex-adc`):
  - `pub struct AdcTokenProvider` implementing `TokenProvider` via `gcp_auth`.
  - `GeminiModel::vertex_from_env(model_id) -> Result<Self, BuildError>`.

- [ ] **Step 1: Implement `AdcTokenProvider`** (append to `auth.rs`)

```rust
#[cfg(feature = "vertex-adc")]
mod adc {
    use super::{TokenProvider, async_trait};
    use paigasus_helikon_core::ModelError;

    const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

    /// Application Default Credentials token provider (metadata server,
    /// `GOOGLE_APPLICATION_CREDENTIALS`, or `gcloud` CLI), backed by `gcp_auth`.
    #[derive(Debug)]
    pub struct AdcTokenProvider {
        manager: gcp_auth::TokenManager,
    }

    impl AdcTokenProvider {
        /// Build from the ambient ADC environment.
        pub async fn from_env() -> Result<Self, ModelError> {
            let provider = gcp_auth::provider()
                .await
                .map_err(|e| ModelError::Other(anyhow::anyhow!("gcp_auth provider: {e}")))?;
            Ok(Self { manager: gcp_auth::TokenManager::new(provider) })
        }
    }

    #[async_trait]
    impl TokenProvider for AdcTokenProvider {
        async fn token(&self) -> Result<String, ModelError> {
            let t = self
                .manager
                .token(&[SCOPE])
                .await
                .map_err(|e| ModelError::Other(anyhow::anyhow!("gcp_auth token: {e}")))?;
            Ok(t.as_str().to_owned())
        }
    }
}

#[cfg(feature = "vertex-adc")]
pub use adc::AdcTokenProvider;
```
> Verify the `gcp_auth` 0.12 API at implementation time (`gcp_auth::provider()`, `TokenManager`, `token(&[scope])`, `Token::as_str`). Adjust the calls to match the crate's actual surface; the `TokenProvider` contract (`async fn token -> Result<String, ModelError>`) is fixed.

- [ ] **Step 2: Re-export behind the feature** (in `lib.rs`)
```rust
#[cfg(feature = "vertex-adc")]
pub use auth::AdcTokenProvider;
```

- [ ] **Step 3: Add `vertex_from_env`** (in `model.rs`)
```rust
impl GeminiModel {
    /// Vertex from `GOOGLE_CLOUD_PROJECT` + `GOOGLE_CLOUD_LOCATION`, using ADC.
    #[cfg(feature = "vertex-adc")]
    pub async fn vertex_from_env(model_id: impl Into<String>) -> Result<Self, crate::BuildError> {
        let project = std::env::var("GOOGLE_CLOUD_PROJECT").map_err(|_| crate::BuildError::MissingVertexProject)?;
        let location = std::env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| "global".into());
        let provider = crate::auth::AdcTokenProvider::from_env()
            .await
            .map_err(|e| crate::BuildError::InvalidBaseUrl(e.to_string()))?; // reuse a variant; see note
        Self::vertex(model_id, project, location).token_provider(provider).build()
    }
}
```
> Note: rather than overload `InvalidBaseUrl`, add a `BuildError::Adc(String)` variant in `builder.rs` and map to it. (Add the variant + adjust the match; it is build-time only.)

- [ ] **Step 4: Compile-check both feature states**

Run: `cargo build -p paigasus-helikon-providers-gemini`
Run: `cargo build -p paigasus-helikon-providers-gemini --features vertex-adc`
Run: `cargo test -p paigasus-helikon-providers-gemini --features vertex-adc`
Expected: all build; tests pass.

- [ ] **Step 5: fmt + clippy (both feature sets) + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
cargo clippy -p paigasus-helikon-providers-gemini --all-targets --features vertex-adc -- -D warnings
git add crates/paigasus-helikon-providers-gemini/src/auth.rs \
        crates/paigasus-helikon-providers-gemini/src/lib.rs \
        crates/paigasus-helikon-providers-gemini/src/builder.rs \
        crates/paigasus-helikon-providers-gemini/src/model.rs
git commit -m "feat(providers): SMA-449 optional vertex-adc ADC token provider

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 14: Live tests, README, crate docs

**Files:**
- Create: `crates/paigasus-helikon-providers-gemini/tests/live.rs`
- Create: `crates/paigasus-helikon-providers-gemini/README.md`
- Modify: `src/lib.rs` (expand crate-level docs)

**Interfaces:** none (docs + ignored live tests).

- [ ] **Step 1: Live tests** (`tests/live.rs`, all `#[ignore]`, skip-if-env-missing)

```rust
//! Live smoke tests. Ignored by default; run with `-- --ignored`.
//!
//! Developer API: set `GEMINI_API_KEY` (+ optional `GEMINI_MODEL_ID`).
//! Vertex (feature `vertex-adc`): set `GOOGLE_CLOUD_PROJECT` + `GOOGLE_CLOUD_LOCATION`
//! with working ADC.

use futures_util::StreamExt;
use paigasus_helikon_core::{CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest, ResponseFormat};
use paigasus_helikon_providers_gemini::GeminiModel;

fn dev_model() -> Option<GeminiModel> {
    let _ = std::env::var("GEMINI_API_KEY").ok()?;
    let id = std::env::var("GEMINI_MODEL_ID").unwrap_or_else(|_| "gemini-2.5-flash".into());
    GeminiModel::from_env(id).ok()
}

fn user(s: &str) -> ModelRequest {
    let mut r = ModelRequest::new();
    r.messages = vec![Item::UserMessage { content: vec![ContentPart::Text { text: s.into() }] }];
    r
}

#[tokio::test]
#[ignore]
async fn live_developer_text_turn() {
    let Some(model) = dev_model() else { return; };
    let mut s = model.invoke(user("Say hi in one word."), CancellationToken::new()).await.unwrap();
    let mut got_text = false;
    while let Some(ev) = s.next().await {
        if let Ok(ModelEvent::TokenDelta { .. }) = ev { got_text = true; }
    }
    assert!(got_text);
}

#[tokio::test]
#[ignore]
async fn live_developer_structured_output() {
    let Some(model) = dev_model() else { return; };
    let mut r = user("Return a person named Ada aged 36.");
    r.model_settings.response_format = Some(ResponseFormat::JsonSchema {
        name: "Person".into(),
        schema: serde_json::json!({ "type":"object","properties":{"name":{"type":"string"},"age":{"type":"integer"}} }),
        strict: true,
    });
    let mut s = model.invoke(r, CancellationToken::new()).await.unwrap();
    let mut json = String::new();
    while let Some(ev) = s.next().await {
        if let Ok(ModelEvent::TokenDelta { text }) = ev { json.push_str(&text); }
    }
    let v: serde_json::Value = serde_json::from_str(json.trim()).expect("valid JSON");
    assert!(v.get("name").is_some());
}

#[cfg(feature = "vertex-adc")]
#[tokio::test]
#[ignore]
async fn live_vertex_text_turn() {
    if std::env::var("GOOGLE_CLOUD_PROJECT").is_err() { return; }
    let id = std::env::var("GEMINI_MODEL_ID").unwrap_or_else(|_| "gemini-2.5-flash".into());
    let model = GeminiModel::vertex_from_env(id).await.unwrap();
    let mut s = model.invoke(user("Say hi."), CancellationToken::new()).await.unwrap();
    while s.next().await.is_some() {}
}
```

- [ ] **Step 2: README** (`README.md`) — model after `crates/paigasus-helikon-providers-bedrock/README.md`:

Sections: title + one-liner; Developer-API vs Vertex; install (`cargo add paigasus-helikon-providers-gemini`, `gemini` feature on the facade, `vertex-adc` feature for ADC); an ` ```ignore ` example using `from_env`; structured output (native `responseSchema`); a `TokenProvider`/Vertex example mentioning the `vertex-adc` feature; a **Limitations** note documenting that remote-URL images, audio, and non-text tool-result parts are dropped, and that reasoning streaming is deferred; Links; License. (Not `include_str!`'d into lib.rs.)

- [ ] **Step 3: Expand crate docs** in `lib.rs` to summarize the public surface (`GeminiModel`, builder, `TokenProvider`, the two transports) — keep the example ` ```ignore `.

- [ ] **Step 4: Verify**

Run: `cargo test -p paigasus-helikon-providers-gemini` (ignored live tests are skipped)
Run: `cargo test -p paigasus-helikon-providers-gemini -- --ignored` with `GEMINI_API_KEY` set locally if available (optional manual check).
Run: `cargo doc -p paigasus-helikon-providers-gemini --no-deps`
Expected: builds; no rustdoc warnings.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-providers-gemini --all-targets -- -D warnings
git add crates/paigasus-helikon-providers-gemini/tests/live.rs \
        crates/paigasus-helikon-providers-gemini/README.md \
        crates/paigasus-helikon-providers-gemini/src/lib.rs
git commit -m "docs(providers): SMA-449 gemini live tests + README

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Final verification (run before opening the PR)

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo clippy -p paigasus-helikon-providers-gemini --all-targets --features vertex-adc -- -D warnings`
- [ ] `cargo test -p paigasus-helikon-providers-gemini`
- [ ] `cargo build -p paigasus-helikon --features gemini`
- [ ] `cargo build -p paigasus-helikon --all-features`
- [ ] `cargo doc -p paigasus-helikon-providers-gemini --no-deps`

---

## Self-review (plan author)

**Spec coverage:**
- §4 module layout → Tasks 1–14 cover every file (error, schema, capabilities, auth, builder, request, tools, response_format, mod+snapshots, sse, stream, transport, model, adc, tests, README). ✓
- §5 public surface (GeminiModel, builder, TokenProvider, from_env/vertex_from_env, build-time validation) → Tasks 4, 11, 13. ✓
- §6 transport (Developer/Vertex URLs + headers, token timing) → Tasks 10, 11. ✓
- §7 request translation (roles, id round-trip + call_id→name, ToolResult reduction, inline images, empty guard, maxOutputTokens omitted) → Tasks 5, 8. ✓
- §8 tool choice + native structured output + conflict guard (history-exempt) → Tasks 6, 7, 8. ✓
- §9 schema sanitizer (ref inline, strip, oneOf→anyOf, [T,null]→nullable, const→enum, cycle/depth) → Task 2. ✓
- §10 stream translation (text/functionCall/usage+thoughts, finish-only-when-observed, blocked-prompt, single Usage) → Task 9. ✓
- §11 error classification → Task 1. ✓
- §12 capabilities (KNOWN_MODELS, reasoning false) → Task 3. ✓
- §13 testing (snapshot scenarios 1–13, wiremock, live) → Tasks 8, 12, 14. ✓
- §14 facade + workspace + crates.io note + gcp_auth dep → Task 1, 13; README Task 14. ✓
- D1a vertex-adc → Task 13. ✓

**Placeholder scan:** No "TBD"/"add error handling" placeholders; every code step shows code. Two flagged verification points (gcp_auth 0.12 API surface in Task 13 Step 1; the `BuildError::Adc` variant) are explicit implement-time confirmations, not hidden work.

**Type consistency:** `Config`/`Transport`/`Auth` fields match across builder.rs (defines), transport.rs, translate/mod.rs snap tests, model.rs (consume). `build_request -> PreparedRequest { body }` used consistently. `StreamTranslator::{new, consume, finish}` signatures match Task 9 and Task 11 usage. `function_calling_config`/`function_declarations`/`response_format_fields`/`validate_conflict`/`items_to_contents`/`sanitize_schema` names consistent between definer and caller tasks.

**Note for implementers:** Task 4 defines temporary inherent `provider`/`model` methods that Task 11 converts to trait methods — Task 11 Step 2 updates the builder test imports accordingly. Implement tasks in order.
